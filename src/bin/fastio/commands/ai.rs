/// Ripley (formerly `ai`) command implementations for `fastio ripley *`.
///
/// Handles AI chat, semantic search, chat history, and workspace summaries.
/// The clap group is `ripley` with a hidden `ai` back-compat alias; this
/// module retains the `ai`-prefixed internal names for now (renaming the
/// command surface, not the internal module layout, is Phase 1's scope).
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::api::ai::{ChatCreateOptions, ChatScope};

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
        /// Preferred file scope: comma-separated `nodeId:versionId` pairs.
        files_scope: Option<String>,
        /// Preferred folder scope: comma-separated `nodeId:depth` pairs.
        folders_scope: Option<String>,
        /// Preferred file attachments: comma-separated `nodeId:versionId` pairs.
        files_attach: Option<String>,
        /// [deprecated] Scope query to specific file node IDs.
        node_ids: Option<Vec<String>>,
        /// [deprecated] Folder ID to scope the AI query to.
        folder_id: Option<String>,
        /// [deprecated] Enable enhanced intelligence for this query.
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
    /// Cancel an in-progress chat message.
    Cancel {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID (workspace ID or share ID).
        profile_id: String,
        /// Chat ID.
        chat_id: String,
    },
}

/// Execute an AI subcommand.
pub async fn execute(command: &AiCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        AiCommand::Chat {
            workspace,
            message,
            chat_id,
            files_scope,
            folders_scope,
            files_attach,
            node_ids,
            folder_id,
            intelligence,
        } => {
            chat(
                ctx,
                workspace,
                message,
                chat_id.as_deref(),
                &ChatFlags {
                    files_scope: files_scope.as_deref(),
                    folders_scope: folders_scope.as_deref(),
                    files_attach: files_attach.as_deref(),
                    node_ids: node_ids.as_deref(),
                    folder_id: folder_id.as_deref(),
                    intelligence: *intelligence,
                },
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
        AiCommand::Cancel {
            profile_type,
            profile_id,
            chat_id,
        } => cancel(ctx, profile_type, profile_id, chat_id).await,
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

/// The full set of scope-related inputs supplied to `ripley chat`: the
/// preferred (visible) `--files-scope` / `--folders-scope` / `--files-attach`
/// flags alongside the hidden, deprecated legacy flags.
#[derive(Debug, Default, Clone, Copy)]
struct ChatFlags<'a> {
    files_scope: Option<&'a str>,
    folders_scope: Option<&'a str>,
    files_attach: Option<&'a str>,
    node_ids: Option<&'a [String]>,
    folder_id: Option<&'a str>,
    intelligence: Option<bool>,
}

/// Build a `files_scope` value from legacy `--node-ids` entries.
///
/// The API requires every `files_scope` pair to be `nodeId:versionId` with
/// BOTH parts non-empty (ai.txt:125-127). Therefore:
/// - entries already in `nodeId:versionId` form pass through (after trim);
/// - whitespace-only / empty entries are dropped;
/// - a BARE node id (no `:versionId`, or one with an empty version half) is
///   an error — we refuse to fabricate an invalid `nodeId:` pair.
///
/// Errors if a bare id is present, or if nothing valid remains.
fn files_scope_from_legacy_node_ids(node_ids: &[String]) -> Result<String> {
    let mut pairs: Vec<String> = Vec::new();
    for raw in node_ids {
        let n = raw.trim();
        if n.is_empty() {
            continue;
        }
        match n.split_once(':') {
            Some((node, version)) if !node.trim().is_empty() && !version.trim().is_empty() => {
                pairs.push(format!("{}:{}", node.trim(), version.trim()));
            }
            _ => {
                anyhow::bail!(
                    "`--node-ids` entry `{n}` is missing a version; the API requires \
                     `nodeId:versionId` pairs. Supply versions via \
                     `--files-scope nodeId:versionId` (or `--files-attach`)."
                );
            }
        }
    }
    anyhow::ensure!(
        !pairs.is_empty(),
        "`--node-ids` contained no valid `nodeId:versionId` entries; \
         use `--files-scope nodeId:versionId` instead"
    );
    Ok(pairs.join(","))
}

/// Normalise a legacy `--folder-id` value into a `folders_scope` entry.
///
/// Trims and rejects an empty value. An already-`nodeId:depth`-qualified
/// value is accepted as-is after validating the depth is 1..=10; an
/// unqualified id gets `:10` appended (full depth).
fn folders_scope_from_legacy_folder_id(folder_id: &str) -> Result<String> {
    let fid = folder_id.trim();
    anyhow::ensure!(!fid.is_empty(), "`--folder-id` must not be empty");
    if let Some((node, depth)) = fid.split_once(':') {
        let node = node.trim();
        let depth = depth.trim();
        anyhow::ensure!(
            !node.is_empty(),
            "`--folder-id` entry `{fid}` has an empty node id"
        );
        let parsed: u32 = depth.parse().map_err(|_| {
            anyhow::anyhow!("`--folder-id` depth `{depth}` is not a valid integer (expected 1-10)")
        })?;
        anyhow::ensure!(
            (1..=10).contains(&parsed),
            "`--folder-id` depth must be 1-10, got {parsed}"
        );
        Ok(format!("{node}:{parsed}"))
    } else {
        Ok(format!("{fid}:10"))
    }
}

/// Resolve the effective [`ChatScope`] from the visible and legacy flags.
///
/// Precedence: the preferred `--files-scope` / `--folders-scope` /
/// `--files-attach` flags WIN. If a preferred flag and its legacy counterpart
/// are BOTH supplied, that is a hard error (the user should pick one). When
/// only a legacy flag is given, it is translated (with a one-time stderr
/// deprecation note) into the corresponding scope field:
/// - `--node-ids` -> `files_scope` (requires `nodeId:versionId` entries; bare
///   ids are rejected — see [`files_scope_from_legacy_node_ids`]).
/// - `--folder-id` -> `folders_scope` (see [`folders_scope_from_legacy_folder_id`]).
/// - `--intelligence` -> no longer maps to a chat parameter; ignored with a
///   deprecation warning.
fn resolve_chat_scope(quiet: bool, flags: &ChatFlags<'_>) -> Result<ChatScope> {
    let mut scope = ChatScope::default();

    let legacy_nodes = flags
        .node_ids
        .filter(|nodes| !nodes.is_empty() && nodes.iter().any(|n| !n.trim().is_empty()));

    // files_scope: prefer the new flag; hard-error on conflict.
    if let Some(fs) = flags.files_scope {
        anyhow::ensure!(
            legacy_nodes.is_none(),
            "conflicting flags: pass either `--files-scope` or the deprecated \
             `--node-ids`, not both"
        );
        let fs = fs.trim();
        anyhow::ensure!(!fs.is_empty(), "`--files-scope` must not be empty");
        scope.files_scope = Some(fs.to_owned());
    } else if let Some(nodes) = legacy_nodes {
        if !quiet {
            eprintln!(
                "[deprecated] `--node-ids` is translated to `files_scope`. \
                 Prefer `--files-scope nodeId:versionId`."
            );
        }
        scope.files_scope = Some(files_scope_from_legacy_node_ids(nodes)?);
    }

    // folders_scope: prefer the new flag; hard-error on conflict.
    if let Some(folders) = flags.folders_scope {
        anyhow::ensure!(
            flags.folder_id.is_none(),
            "conflicting flags: pass either `--folders-scope` or the deprecated \
             `--folder-id`, not both"
        );
        let folders = folders.trim();
        anyhow::ensure!(!folders.is_empty(), "`--folders-scope` must not be empty");
        scope.folders_scope = Some(folders.to_owned());
    } else if let Some(fid) = flags.folder_id {
        if !quiet {
            eprintln!(
                "[deprecated] `--folder-id` is translated to `folders_scope` (full depth). \
                 Prefer `--folders-scope nodeId:depth`."
            );
        }
        scope.folders_scope = Some(folders_scope_from_legacy_folder_id(fid)?);
    }

    // files_attach: no legacy counterpart; just forward (trim, reject empty).
    if let Some(fa) = flags.files_attach {
        let fa = fa.trim();
        anyhow::ensure!(!fa.is_empty(), "`--files-attach` must not be empty");
        scope.files_attach = Some(fa.to_owned());
    }

    // Mutual exclusion: `files_attach` and `files_scope`/`folders_scope` cannot
    // be combined in the same request — the server rejects both with `1605`
    // (ai.txt:115,311,609). Enforce on the RESOLVED scope so the legacy
    // `--node-ids` -> files_scope and `--folder-id` -> folders_scope
    // translations are caught too, not just the literal `--files-scope` flag.
    anyhow::ensure!(
        !(scope.files_attach.is_some()
            && (scope.files_scope.is_some() || scope.folders_scope.is_some())),
        "files_attach cannot be combined with files_scope/folders_scope — \
         use one or the other"
    );

    if flags.intelligence.is_some() && !quiet {
        eprintln!(
            "[deprecated] `--intelligence` no longer maps to a chat parameter and is ignored; \
             scope flags now control indexing reach."
        );
    }

    Ok(scope)
}

/// Send a chat message and poll for the AI response.
async fn chat(
    ctx: &CommandContext<'_>,
    workspace: &str,
    message: &str,
    existing_chat_id: Option<&str>,
    flags: &ChatFlags<'_>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );

    let scope = resolve_chat_scope(ctx.output.quiet, flags)?;

    let client = ctx.build_client()?;

    // Step 1: Create chat or send message to existing chat
    let (chat_id, initial_response) = if let Some(cid) = existing_chat_id {
        let resp = api::ai::send_message(&client, workspace, cid, message, None, &scope)
            .await
            .context("failed to send AI message")?;
        (cid.to_owned(), resp)
    } else {
        let mut options = ChatCreateOptions::default();
        options.personality = Some("detailed".to_owned());
        options.scope = scope;
        let resp = api::ai::create_chat(&client, workspace, message, "chat_with_files", &options)
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
    // The `/ai/share/` endpoint caps `files` at 1-25 (ai.txt:894); reject
    // oversized requests client-side before the network round-trip.
    anyhow::ensure!(
        node_ids.len() <= 25,
        "too many files: {} supplied, but AI share accepts at most 25",
        node_ids.len()
    );

    let client = ctx.build_client()?;
    let value = api::ai::summarize(&client, workspace, node_ids)
        .await
        .context("failed to generate AI summary")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Cancel an in-progress chat message.
///
/// `profile_type` and `profile_id` are pre-resolved by `map_ripley_command` from
/// the clap-enforced `--workspace` xor `--share` flags, so the handler does
/// not re-validate that invariant. ID trimming and `profile_type`
/// whitelisting happen inside `cancel_message` so the MCP path benefits
/// from the same guards.
async fn cancel(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    chat_id: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::ai::cancel_message(&client, profile_type, profile_id, chat_id)
        .await
        .context("failed to cancel AI chat message")?;
    ctx.output.render(&value)?;
    Ok(())
}

#[cfg(test)]
mod legacy_flag_tests {
    use super::{ChatFlags, resolve_chat_scope};

    fn flags() -> ChatFlags<'static> {
        ChatFlags::default()
    }

    #[test]
    fn node_ids_with_versions_pass_through() {
        // Fully-qualified `nodeId:versionId` entries pass through (trimmed).
        let nodes = vec!["abc:v1".to_owned(), " def:v2 ".to_owned()];
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                ..flags()
            },
        )
        .expect("qualified node ids should be accepted");
        assert_eq!(scope.files_scope.as_deref(), Some("abc:v1,def:v2"));
        assert!(scope.folders_scope.is_none());
    }

    #[test]
    fn bare_node_id_is_rejected() {
        // A bare node id (no version) is invalid per ai.txt:125-127.
        let nodes = vec!["abc".to_owned()];
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                ..flags()
            },
        )
        .expect_err("bare node id must be rejected");
        assert!(err.to_string().contains("missing a version"));
    }

    #[test]
    fn node_id_with_empty_version_half_is_rejected() {
        // `abc:` (trailing colon, empty version) must also be rejected.
        let nodes = vec!["abc:".to_owned()];
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                ..flags()
            },
        )
        .expect_err("empty version half must be rejected");
        assert!(err.to_string().contains("missing a version"));
    }

    #[test]
    fn whitespace_node_ids_produce_no_files_scope() {
        // `--node-ids ""` / whitespace entries are filtered; nothing valid
        // remains so this resolves to an empty (no-op) scope, not `:`/`:10`.
        let nodes = vec![String::new(), "   ".to_owned()];
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                ..flags()
            },
        )
        .expect("all-whitespace node ids are treated as unset");
        assert!(scope.files_scope.is_none());
    }

    #[test]
    fn folder_id_maps_to_folders_scope_depth_10() {
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                folder_id: Some("F123"),
                ..flags()
            },
        )
        .expect("bare folder id gets :10 appended");
        assert_eq!(scope.folders_scope.as_deref(), Some("F123:10"));
        assert!(scope.files_scope.is_none());
    }

    #[test]
    fn folder_id_already_qualified_is_validated_and_kept() {
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                folder_id: Some("F123:3"),
                ..flags()
            },
        )
        .expect("valid nodeId:depth folder id is kept as-is");
        assert_eq!(scope.folders_scope.as_deref(), Some("F123:3"));
    }

    #[test]
    fn folder_id_with_out_of_range_depth_is_rejected() {
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                folder_id: Some("F123:99"),
                ..flags()
            },
        )
        .expect_err("depth outside 1-10 must be rejected");
        assert!(err.to_string().contains("depth must be 1-10"));
    }

    #[test]
    fn empty_folder_id_is_rejected() {
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                folder_id: Some("   "),
                ..flags()
            },
        )
        .expect_err("whitespace-only folder id must be rejected");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn intelligence_is_ignored_and_does_not_populate_scope() {
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                intelligence: Some(true),
                ..flags()
            },
        )
        .expect("intelligence is ignored, not an error");
        assert!(scope.files_scope.is_none());
        assert!(scope.folders_scope.is_none());
        assert!(scope.files_attach.is_none());
    }

    #[test]
    fn new_flags_reach_the_scope() {
        // `files_scope` + `folders_scope` reach the scope verbatim. (They
        // cannot be combined with `files_attach` — that mutual exclusion is
        // covered by `files_attach_with_files_scope_errors` below.)
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                files_scope: Some("n1:v1,n2:v2"),
                folders_scope: Some("f1:5"),
                ..flags()
            },
        )
        .expect("new scope flags should populate the scope verbatim");
        assert_eq!(scope.files_scope.as_deref(), Some("n1:v1,n2:v2"));
        assert_eq!(scope.folders_scope.as_deref(), Some("f1:5"));
        assert!(scope.files_attach.is_none());
    }

    #[test]
    fn files_attach_with_files_scope_errors() {
        // ai.txt:115,311,609 — `files_attach` and `files_scope` are mutually
        // exclusive; sending both makes the server reject with `1605`.
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                files_scope: Some("n1:v1"),
                files_attach: Some("a1:v1"),
                ..flags()
            },
        )
        .expect_err("files_attach + files_scope must be rejected client-side");
        assert!(
            err.to_string()
                .contains("files_attach cannot be combined with files_scope/folders_scope")
        );
    }

    #[test]
    fn files_attach_with_legacy_node_ids_errors() {
        // The guard runs on the RESOLVED scope, so the legacy
        // `--node-ids` -> files_scope translation is caught too.
        let nodes = vec!["abc:v1".to_owned()];
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                files_attach: Some("a1:v1"),
                ..flags()
            },
        )
        .expect_err("files_attach + legacy --node-ids must be rejected client-side");
        assert!(
            err.to_string()
                .contains("files_attach cannot be combined with files_scope/folders_scope")
        );
    }

    #[test]
    fn files_attach_alone_is_fine() {
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                files_attach: Some("a1:v1"),
                ..flags()
            },
        )
        .expect("files_attach on its own is valid");
        assert_eq!(scope.files_attach.as_deref(), Some("a1:v1"));
        assert!(scope.files_scope.is_none());
        assert!(scope.folders_scope.is_none());
    }

    #[test]
    fn files_scope_alone_is_fine() {
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                files_scope: Some("n1:v1"),
                ..flags()
            },
        )
        .expect("files_scope on its own is valid");
        assert_eq!(scope.files_scope.as_deref(), Some("n1:v1"));
        assert!(scope.files_attach.is_none());
    }

    #[test]
    fn new_files_scope_conflicts_with_legacy_node_ids() {
        let nodes = vec!["abc:v1".to_owned()];
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                files_scope: Some("n1:v1"),
                node_ids: Some(&nodes),
                ..flags()
            },
        )
        .expect_err("supplying both --files-scope and --node-ids is a conflict");
        assert!(err.to_string().contains("conflicting flags"));
    }

    #[test]
    fn new_folders_scope_conflicts_with_legacy_folder_id() {
        let err = resolve_chat_scope(
            true,
            &ChatFlags {
                folders_scope: Some("f1:5"),
                folder_id: Some("F123"),
                ..flags()
            },
        )
        .expect_err("supplying both --folders-scope and --folder-id is a conflict");
        assert!(err.to_string().contains("conflicting flags"));
    }
}
