/// AI command implementations for `fastio ai *`.
///
/// Handles AI chat, semantic search, chat history, and workspace summaries.
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use super::CommandContext;
use fastio_cli::api;

/// Maximum polling attempts for AI response.
const MAX_POLL_ATTEMPTS: u32 = 15;

/// Delay between poll attempts in seconds.
const POLL_DELAY_SECS: u64 = 2;

/// AI subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AiCommand {
    /// Send a chat message and poll for the AI response.
    Chat {
        /// Workspace ID.
        workspace: String,
        /// User message text.
        message: String,
        /// Existing chat ID (optional, creates new if omitted).
        chat_id: Option<String>,
        /// Scope query to specific file/folder node IDs.
        node_ids: Option<Vec<String>>,
        /// Folder ID to scope the AI query to.
        folder_id: Option<String>,
        /// Enable enhanced intelligence for this query.
        intelligence: Option<bool>,
    },
    /// Semantic search over indexed workspace files.
    Search {
        /// Workspace ID.
        workspace: String,
        /// Search query.
        query: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Get chat message history.
    History {
        /// Workspace ID.
        workspace: String,
        /// Chat ID (lists all chats if omitted).
        chat_id: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Generate a shareable AI summary from specific workspace files.
    Summary {
        /// Workspace ID.
        workspace: String,
        /// File node IDs to include in the summary (at least one required).
        node_ids: Vec<String>,
    },
}

/// Execute an AI subcommand.
pub async fn execute(command: &AiCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        AiCommand::Chat {
            workspace,
            message,
            chat_id,
            node_ids,
            folder_id,
            intelligence,
        } => {
            chat(
                ctx,
                workspace,
                message,
                chat_id.as_deref(),
                node_ids.as_deref(),
                folder_id.as_deref(),
                *intelligence,
            )
            .await
        }
        AiCommand::Search {
            workspace,
            query,
            limit,
            offset,
        } => search(ctx, workspace, query, *limit, *offset).await,
        AiCommand::History {
            workspace,
            chat_id,
            limit,
            offset,
        } => history(ctx, workspace, chat_id.as_deref(), *limit, *offset).await,
        AiCommand::Summary {
            workspace,
            node_ids,
        } => summary(ctx, workspace, node_ids).await,
    }
}

/// Extract a string field from a JSON value, handling both string and numeric types.
///
/// The Fast.io API may return IDs as either JSON strings or numbers (19-digit
/// numeric profile/entity IDs). This helper normalises both forms to `String`.
fn extract_string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Extract citations from an AI message response.
///
/// Citations may appear at `response.citations` or directly at `citations`
/// depending on the API version.
fn extract_citations(msg: &Value) -> Option<Value> {
    msg.get("response")
        .and_then(|r| r.get("citations"))
        .or_else(|| msg.get("citations"))
        .cloned()
}

/// Send a chat message and poll for the AI response.
async fn chat(
    ctx: &CommandContext<'_>,
    workspace: &str,
    message: &str,
    existing_chat_id: Option<&str>,
    node_ids: Option<&[String]>,
    folder_id: Option<&str>,
    intelligence: Option<bool>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;

    // Step 1: Create chat or send message to existing chat
    let (chat_id, initial_response) = if let Some(cid) = existing_chat_id {
        let resp = api::ai::send_message(&client, workspace, cid, message)
            .await
            .context("failed to send AI message")?;
        (cid.to_owned(), resp)
    } else {
        let resp = api::ai::create_chat(
            &client,
            workspace,
            message,
            "chat_with_files",
            node_ids,
            folder_id,
            intelligence,
        )
        .await
        .context("failed to create AI chat")?;
        let cid = resp
            .get("chat_id")
            .or_else(|| resp.get("chat").and_then(|c| c.get("id")))
            .or_else(|| resp.get("id"));
        let cid = cid
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("no chat_id in response"))?;
        (cid, resp)
    };

    // Step 2: Extract message_id from response (handle both string and numeric IDs)
    let message_id_value = initial_response
        .get("message_id")
        .or_else(|| initial_response.get("message").and_then(|m| m.get("id")))
        .or_else(|| {
            initial_response
                .get("message")
                .and_then(|m| m.get("message_id"))
        });
    let message_id = message_id_value
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("no message_id in response"))?;

    // Step 3: Poll for AI response
    if !ctx.output.quiet {
        eprintln!("Waiting for AI response...");
    }

    for attempt in 0..MAX_POLL_ATTEMPTS {
        let details = api::ai::get_message_details(&client, workspace, &chat_id, &message_id).await;

        if let Ok(msg_data) = details {
            let msg = msg_data.get("message").unwrap_or(&msg_data);
            let state = extract_string_field(msg, "state").unwrap_or_default();

            if state == "complete" || state == "errored" {
                let response_text = msg
                    .get("response")
                    .and_then(|r| r.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();

                let mut result = serde_json::json!({
                    "chat_id": chat_id,
                    "message_id": message_id,
                    "state": state,
                    "response": response_text,
                });

                // Include citations if present in the AI response
                if let Some(citations) = extract_citations(msg)
                    && let Some(obj) = result.as_object_mut()
                {
                    obj.insert("citations".to_owned(), citations);
                }

                ctx.output.render(&result)?;
                return Ok(());
            }
        }

        if attempt < MAX_POLL_ATTEMPTS - 1 {
            tokio::time::sleep(Duration::from_secs(POLL_DELAY_SECS)).await;
        }
    }

    // Timed out
    let result = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "state": "processing",
        "message": "Polling timed out after ~30 seconds. The AI may still be processing.",
    });
    ctx.output.render(&result)?;
    Ok(())
}

/// Semantic search over indexed workspace files.
async fn search(
    ctx: &CommandContext<'_>,
    workspace: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;
    let value = api::ai::search(&client, workspace, query, limit, offset)
        .await
        .context("failed to perform AI search")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get chat message history.
async fn history(
    ctx: &CommandContext<'_>,
    workspace: &str,
    chat_id: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;

    if let Some(cid) = chat_id {
        let value = api::ai::list_messages(&client, workspace, cid, limit, offset)
            .await
            .context("failed to list chat messages")?;
        ctx.output.render(&value)?;
    } else {
        let value = api::ai::list_chats(&client, workspace, limit, offset)
            .await
            .context("failed to list chats")?;
        ctx.output.render(&value)?;
    }
    Ok(())
}

/// Generate a shareable AI summary from specific workspace files.
async fn summary(ctx: &CommandContext<'_>, workspace: &str, node_ids: &[String]) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    if node_ids.is_empty() {
        anyhow::bail!("at least one node ID is required (specify file node IDs to summarize)");
    }

    let client = ctx.build_client()?;
    let value = api::ai::summarize(&client, workspace, node_ids)
        .await
        .context("failed to generate AI summary")?;
    ctx.output.render(&value)?;
    Ok(())
}
