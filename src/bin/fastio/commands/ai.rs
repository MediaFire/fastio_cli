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
use super::workflow::{PollAction, classify_poll_error};
use fastio_cli::api;
use fastio_cli::api::ai::{ChatCreateOptions, ChatScope};
use fastio_cli::error::CliError;

/// Resolved scope/attachment flags for the `ask`/`chat` verbs, carried from
/// the clap layer into the internal command enum.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AskScopeFlags {
    /// `--files-scope`: comma-separated `nodeId:versionId` pairs.
    pub files_scope: Option<String>,
    /// `--folders-scope`: comma-separated `nodeId:depth` pairs.
    pub folders_scope: Option<String>,
    /// `--files-attach`: comma-separated `nodeId:versionId` pairs.
    pub files_attach: Option<String>,
}

/// AI-memory scope, resolved from the clap `--org` XOR `--workspace` flags.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MemoryTarget {
    /// Organization-scoped memory.
    Org(String),
    /// Workspace-scoped memory.
    Workspace(String),
}

/// AI-memory subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MemoryCommand {
    /// Read the caller's memory row.
    Get(MemoryTarget),
    /// Write the caller's memory row (optional revision CAS).
    Set {
        /// Org- or workspace-scoped target.
        target: MemoryTarget,
        /// New content (≤64KB).
        content: String,
        /// Optimistic-concurrency revision.
        revision: Option<u64>,
    },
    /// Hard-delete the caller's memory row.
    Delete(MemoryTarget),
}

/// AI subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AiCommand {
    /// Ask a question and wait for the answer (headline verb).
    Ask {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID (workspace ID or share ID).
        profile_id: String,
        /// The question to ask.
        question: String,
        /// Scope/attachment flags.
        scope: AskScopeFlags,
        /// Response style (`concise` / `detailed`).
        personality: Option<String>,
        /// Chat kind (`user` / `agent`; workspace-only).
        kind: Option<String>,
        /// Return IDs immediately without waiting for the answer.
        no_wait: bool,
    },
    /// Show full details (and history) for a chat.
    Details {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID.
        profile_id: String,
        /// Chat ID.
        chat_id: String,
    },
    /// List messages in a chat.
    Messages {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID.
        profile_id: String,
        /// Chat ID.
        chat_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Show a single message's details.
    Message {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID.
        profile_id: String,
        /// Chat ID.
        chat_id: String,
        /// Message ID.
        message_id: String,
    },
    /// List the caller's chats (workspace context only for `kind`/`deleted`).
    List {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID.
        profile_id: String,
        /// Filter by chat kind (`user`/`agent`/`all`).
        kind: Option<String>,
        /// List soft-deleted chats.
        deleted: bool,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Rename a chat.
    Update {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID.
        profile_id: String,
        /// Chat ID.
        chat_id: String,
        /// New name.
        name: String,
    },
    /// Publish a private chat (make it public; one-way).
    Publish {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID.
        profile_id: String,
        /// Chat ID.
        chat_id: String,
    },
    /// Soft-delete a chat.
    Delete {
        /// Profile type: `workspace` or `share`.
        profile_type: String,
        /// Profile ID.
        profile_id: String,
        /// Chat ID.
        chat_id: String,
    },
    /// List recent AI token-usage transactions (workspace-only).
    Transactions {
        /// Workspace ID.
        workspace: String,
    },
    /// AI-generate a title/description for a share (share-only).
    Autotitle {
        /// Share ID.
        share: String,
        /// Optional context to guide generation.
        user_context: Option<String>,
    },
    /// Manage the caller's AI memory (org or workspace; self-only).
    Memory(MemoryCommand),
    /// Hand work to Ripley to run on the caller's behalf. The server
    /// delegation contract is not finalized, so this is a guarded stub —
    /// the payload is retained for the forthcoming wiring but unread today
    /// (the handler emits a "pending" message and calls no endpoint).
    Delegate {
        /// The instruction the caller wanted to delegate (unused while pending).
        #[allow(dead_code)]
        instruction: String,
    },
    /// Show the status of a delegated job (guarded stub; see [`AiCommand::Delegate`]).
    JobStatus {
        /// Delegated-job ID (unused while pending).
        #[allow(dead_code)]
        id: String,
    },
    /// Show the tool-call log of a delegated job (guarded stub; see [`AiCommand::Delegate`]).
    JobLogs {
        /// Delegated-job ID (unused while pending).
        #[allow(dead_code)]
        id: String,
    },
    /// Cancel an in-flight delegated job (guarded stub; see [`AiCommand::Delegate`]).
    JobCancel {
        /// Delegated-job ID (unused while pending).
        #[allow(dead_code)]
        id: String,
    },
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
#[allow(clippy::too_many_lines)]
pub async fn execute(command: &AiCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        AiCommand::Ask {
            profile_type,
            profile_id,
            question,
            scope,
            personality,
            kind,
            no_wait,
        } => {
            ask(
                ctx,
                profile_type,
                profile_id,
                question,
                scope,
                personality.as_deref(),
                kind.as_deref(),
                *no_wait,
            )
            .await
        }
        AiCommand::Details {
            profile_type,
            profile_id,
            chat_id,
        } => {
            let client = ctx.build_client()?;
            let value = api::ai::chat_details(&client, profile_type, profile_id, chat_id)
                .await
                .context("failed to get chat details")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        AiCommand::Messages {
            profile_type,
            profile_id,
            chat_id,
            limit,
            offset,
        } => messages(ctx, profile_type, profile_id, chat_id, *limit, *offset).await,
        AiCommand::Message {
            profile_type,
            profile_id,
            chat_id,
            message_id,
        } => message(ctx, profile_type, profile_id, chat_id, message_id).await,
        AiCommand::List {
            profile_type,
            profile_id,
            kind,
            deleted,
            limit,
            offset,
        } => {
            list_chats(
                ctx,
                profile_type,
                profile_id,
                kind.as_deref(),
                *deleted,
                *limit,
                *offset,
            )
            .await
        }
        AiCommand::Update {
            profile_type,
            profile_id,
            chat_id,
            name,
        } => {
            let client = ctx.build_client()?;
            let value = api::ai::update_chat(&client, profile_type, profile_id, chat_id, name)
                .await
                .context("failed to rename chat")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        AiCommand::Publish {
            profile_type,
            profile_id,
            chat_id,
        } => {
            let client = ctx.build_client()?;
            let value = api::ai::publish_chat(&client, profile_type, profile_id, chat_id)
                .await
                .context("failed to publish chat")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        AiCommand::Delete {
            profile_type,
            profile_id,
            chat_id,
        } => {
            let client = ctx.build_client()?;
            let value = api::ai::delete_chat(&client, profile_type, profile_id, chat_id)
                .await
                .context("failed to delete chat")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        AiCommand::Transactions { workspace } => transactions(ctx, workspace).await,
        AiCommand::Autotitle {
            share,
            user_context,
        } => autotitle(ctx, share, user_context.as_deref()).await,
        AiCommand::Memory(mem) => memory(ctx, mem).await,
        // All four delegated-job stubs share the same guarded "pending"
        // response and call no endpoint (the server delegation contract is
        // not finalized — see phase2.log "Delegated-job contract").
        AiCommand::Delegate { .. }
        | AiCommand::JobStatus { .. }
        | AiCommand::JobLogs { .. }
        | AiCommand::JobCancel { .. } => delegated_jobs_unavailable(),
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

    // Step 3: Wait for the AI response using the SAME bounded activity-poll
    // path as `ask` — long-poll activity and confirm via a single message-
    // details read (never a tight `--detail`-style loop), with the shared
    // error classifier so a persistent 403/404/402 is surfaced instead of
    // looping to a misleading timeout. `chat` is workspace-only, so the
    // profile is always `("workspace", workspace)`.
    if !ctx.output.quiet {
        eprintln!("Waiting for AI response...");
    }

    match wait_for_answer(&client, "workspace", workspace, &chat_id, &message_id).await {
        WaitOutcome::Complete(msg) => {
            let result = render_answer(&chat_id, &message_id, &msg);
            ctx.output.render(&result)?;
            Ok(())
        }
        WaitOutcome::TimedOut => {
            let result = serde_json::json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "state": "processing",
                "message": format!(
                    "Timed out after ~{ASK_MAX_WAIT_SECS}s waiting for the answer. \
                     The AI may still be processing — re-check with \
                     `fastio ripley message --workspace {workspace} {chat_id} {message_id}`."
                ),
            });
            ctx.output.render(&result)?;
            Ok(())
        }
        WaitOutcome::AuthExpired => anyhow::bail!(
            "authentication expired while waiting for the answer. The chat was created \
             (chat_id={chat_id}, message_id={message_id}); re-authenticate \
             (`fastio auth login`) and re-check with \
             `fastio ripley message --workspace {workspace} {chat_id} {message_id}`."
        ),
        WaitOutcome::Failed(err) => Err(anyhow::Error::new(err).context(format!(
            "error while waiting for the AI response (chat_id={chat_id}, \
             message_id={message_id}); re-check with \
             `fastio ripley message --workspace {workspace} {chat_id} {message_id}`"
        ))),
    }
}

/// Semantic search over indexed workspace files.
///
/// Re-pointed (Phase 3) off the deprecated `/ai/search/` onto the single
/// `api::storage::search_files` builder (`/storage/search/`), which performs
/// semantic search automatically when workspace intelligence is enabled. This
/// is now a thin deprecated alias; a one-time stderr notice steers callers to
/// `fastio files search` / `fastio ripley ask`.
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
    anyhow::ensure!(!query.trim().is_empty(), "search query must not be empty");
    fastio_cli::deprecation::warn_once(
        "ripley-search",
        "`ripley search` (formerly `ai search`) now uses `/storage/search/`. \
         Prefer `fastio files search` for file search, or `fastio ripley ask` \
         to have Ripley answer a question over your content.",
        ctx.output.quiet,
    );
    let client = ctx.build_client()?;
    let params = api::storage::SearchFilesParams::new()
        .limit(limit)
        .offset(offset);
    let value = api::storage::search_files(&client, workspace, query, params)
        .await
        .context("failed to perform search")?;
    // Same files-MAP → rows normalization as `fastio files search`.
    let value = api::storage::normalize_search_response(value);
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
        let options = api::ai::ListChatsOptions::paged(limit, offset);
        let value = api::ai::list_chats(&client, workspace, &options)
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

/// Maximum wall-clock the `ask` wait loop will spend before giving up, in
/// seconds. The activity-poll long-poll holds the connection up to ~95s
/// server-side; a JWT also expires after ~1 hour. We bound total waiting well
/// under the JWT lifetime so a stuck/very-slow answer surfaces a clear timeout
/// rather than hanging indefinitely (and eventually 401-ing mid-loop).
const ASK_MAX_WAIT_SECS: u64 = 300;

/// Per-iteration long-poll `wait` hint passed to the activity endpoint, in
/// seconds. The server caps this at 95s; we use a smaller value so each
/// iteration returns promptly and the overall budget is checked frequently.
const ASK_POLL_WAIT_SECS: u32 = 20;

/// Ask Ripley a question: create a chat, then (unless `--no-wait`) wait for
/// the answer via the existing activity-poll loop and a message-details fetch.
///
/// Both workspace and share contexts are supported via the context-aware
/// `ai_api_form`/`ai_api` builders. `privacy`/`kind` are workspace-only, so
/// `kind` is forwarded only in a workspace context (share rejects it).
#[allow(clippy::too_many_arguments)]
async fn ask(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    question: &str,
    scope: &AskScopeFlags,
    personality: Option<&str>,
    kind: Option<&str>,
    no_wait: bool,
) -> Result<()> {
    anyhow::ensure!(
        !profile_id.trim().is_empty(),
        "{profile_type} ID must not be empty"
    );
    anyhow::ensure!(!question.trim().is_empty(), "question must not be empty");
    // Mutual exclusion: files_attach cannot combine with files_scope/folders_scope
    // (ai.txt:115,311,609). Enforce client-side before the round-trip.
    anyhow::ensure!(
        !(scope.files_attach.is_some()
            && (scope.files_scope.is_some() || scope.folders_scope.is_some())),
        "files_attach cannot be combined with files_scope/folders_scope — use one or the other"
    );
    // `kind` is workspace-only; a share rejects it, so it's silently dropped for
    // a share context. Note the drop on stderr (gated on quiet, like the
    // deprecation notices) rather than hard-erroring — keep the call lenient.
    if profile_type == "share" && kind.is_some() && !ctx.output.quiet {
        eprintln!("Note: --kind is workspace-only and was ignored for this share.");
    }

    let client = ctx.build_client()?;

    // Build the create form (always form-encoded; documented field set only).
    let mut form = std::collections::HashMap::new();
    form.insert("type".to_owned(), "chat_with_files".to_owned());
    form.insert("question".to_owned(), question.to_owned());
    form.insert(
        "personality".to_owned(),
        personality.unwrap_or("detailed").to_owned(),
    );
    // `kind` is workspace-only — share chats reject it.
    if profile_type == "workspace"
        && let Some(k) = kind
    {
        form.insert("kind".to_owned(), k.to_owned());
    }
    if let Some(v) = &scope.files_scope {
        form.insert("files_scope".to_owned(), v.clone());
    }
    if let Some(v) = &scope.folders_scope {
        form.insert("folders_scope".to_owned(), v.clone());
    }
    if let Some(v) = &scope.files_attach {
        form.insert("files_attach".to_owned(), v.clone());
    }

    let resp = api::ai::ai_api_form(&client, profile_type, profile_id, "agent/", &form)
        .await
        .context("failed to create AI chat")?;

    let chat_id =
        extract_chat_id(&resp).ok_or_else(|| anyhow::anyhow!("no chat_id in response"))?;
    let message_id =
        extract_message_id(&resp).ok_or_else(|| anyhow::anyhow!("no message_id in response"))?;

    if no_wait {
        let result = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "state": "processing",
        });
        ctx.output.render(&result)?;
        return Ok(());
    }

    if !ctx.output.quiet {
        eprintln!("Waiting for Ripley's answer...");
    }

    let final_msg = wait_for_answer(&client, profile_type, profile_id, &chat_id, &message_id).await;
    match final_msg {
        WaitOutcome::Complete(msg) => {
            let result = render_answer(&chat_id, &message_id, &msg);
            ctx.output.render(&result)?;
            Ok(())
        }
        WaitOutcome::TimedOut => {
            let result = serde_json::json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "state": "processing",
                "message": format!(
                    "Timed out after ~{ASK_MAX_WAIT_SECS}s waiting for the answer. \
                     The AI may still be processing — re-check with \
                     `fastio ripley message --{profile_type} {profile_id} {chat_id} {message_id}`."
                ),
            });
            ctx.output.render(&result)?;
            Ok(())
        }
        WaitOutcome::AuthExpired => {
            anyhow::bail!(
                "authentication expired while waiting for the answer. The chat was created \
                 (chat_id={chat_id}, message_id={message_id}); re-authenticate \
                 (`fastio auth login`) and re-check with \
                 `fastio ripley message --{profile_type} {profile_id} {chat_id} {message_id}`."
            );
        }
        WaitOutcome::Failed(err) => Err(anyhow::Error::new(err).context(format!(
            "error while waiting for Ripley's answer (chat_id={chat_id}, \
             message_id={message_id}); re-check with \
             `fastio ripley message --{profile_type} {profile_id} {chat_id} {message_id}`"
        ))),
    }
}

/// Outcome of the bounded `ask` wait loop.
enum WaitOutcome {
    /// The message reached `complete`/`errored`; carries its details body.
    Complete(Value),
    /// The wait budget elapsed without a terminal state.
    TimedOut,
    /// A 401 surfaced (JWT expired mid-wait).
    AuthExpired,
    /// A persistent, non-transient error (403 / 404 / 402 / parse) surfaced
    /// mid-wait. Carries the underlying error so it is reported rather than
    /// silently looping to a misleading timeout.
    Failed(CliError),
}

/// Bounded wait for an answer using the documented activity-poll + a
/// message-details confirmation.
///
/// Strategy (per `~/vividengine/llms/ai.txt:1000-1014,1812-1830`): long-poll
/// the workspace/share activity endpoint (which returns promptly on any
/// change and otherwise holds ~95s), then fetch message details once to read
/// the authoritative `state`. We do NOT poll message-details in a tight loop
/// (the doc's anti-pattern). The loop is bounded by [`ASK_MAX_WAIT_SECS`] so
/// it cannot hang past a JWT's lifetime; a 401 short-circuits to
/// [`WaitOutcome::AuthExpired`] with a re-auth hint.
async fn wait_for_answer(
    client: &fastio_cli::client::ApiClient,
    profile_type: &str,
    profile_id: &str,
    chat_id: &str,
    message_id: &str,
) -> WaitOutcome {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(ASK_MAX_WAIT_SECS);
    let mut lastactivity: Option<String> = None;
    // Context-aware message-details sub-path. `get_message_details` is
    // workspace-hardcoded, so use the context-aware `ai_api` builder here —
    // `profile_id` may be a share id when `ask` is share-scoped.
    let details_sub = format!(
        "agent/{}/message/{}/details/",
        urlencoding::encode(chat_id),
        urlencoding::encode(message_id),
    );

    loop {
        // First, read the authoritative message state. If it's already
        // terminal, we're done (also covers a fast/cached answer before the
        // first poll). A 401 here means the JWT expired.
        match api::ai::ai_api(
            client,
            profile_type,
            profile_id,
            &details_sub,
            "GET",
            None,
            None,
        )
        .await
        {
            Ok(msg_data) => {
                let msg = msg_data.get("message").unwrap_or(&msg_data);
                let state = extract_string_field(msg, "state").unwrap_or_default();
                if state == "complete" || state == "errored" {
                    return WaitOutcome::Complete(msg_data);
                }
            }
            Err(CliError::Api(e)) if e.http_status == 401 => {
                return WaitOutcome::AuthExpired;
            }
            // Classify the error rather than swallowing it: a transient blip
            // falls through to the long-poll and retries; a persistent 4xx
            // (403/404/402/parse) is surfaced instead of looping to a
            // misleading timeout.
            Err(e) => match classify_poll_error(e) {
                PollAction::RateLimited { retry_after_secs } => {
                    if retry_after_secs > 0 {
                        tokio::time::sleep(Duration::from_secs(retry_after_secs)).await;
                    }
                }
                PollAction::RetryTransient => {}
                PollAction::Fatal(err) => return WaitOutcome::Failed(err),
            },
        }

        if tokio::time::Instant::now() >= deadline {
            return WaitOutcome::TimedOut;
        }

        // Long-poll activity. The server returns promptly when the
        // `ai_chat:{chatId}` key fires; otherwise it holds the connection.
        // Activity-poll uses the same `/activity/poll/{id}/` endpoint for both
        // workspace and share profile ids.
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_secs();
        if remaining == 0 {
            return WaitOutcome::TimedOut;
        }
        let wait = ASK_POLL_WAIT_SECS.min(u32::try_from(remaining).unwrap_or(ASK_POLL_WAIT_SECS));
        match api::event::poll_activity(
            client,
            profile_id,
            lastactivity.as_deref(),
            Some(wait),
            false,
        )
        .await
        {
            Ok(poll) => {
                if let Some(ts) = poll.get("lastactivity").and_then(Value::as_str) {
                    lastactivity = Some(ts.to_owned());
                }
            }
            Err(CliError::Api(e)) if e.http_status == 401 => {
                return WaitOutcome::AuthExpired;
            }
            // A transient poll error shouldn't abort the wait; back off briefly
            // and retry the details check. A persistent 4xx is fatal.
            Err(e) => match classify_poll_error(e) {
                PollAction::RateLimited { retry_after_secs } => {
                    tokio::time::sleep(Duration::from_secs(retry_after_secs.max(2))).await;
                }
                PollAction::RetryTransient => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                PollAction::Fatal(err) => return WaitOutcome::Failed(err),
            },
        }

        if tokio::time::Instant::now() >= deadline {
            return WaitOutcome::TimedOut;
        }
    }
}

/// Extract the chat id from a create-chat response, handling the documented
/// `chat.id` shape plus the legacy `chat_id`/`id` fallbacks.
fn extract_chat_id(resp: &Value) -> Option<String> {
    let v = resp
        .get("chat_id")
        .or_else(|| resp.get("chat").and_then(|c| c.get("id")))
        .or_else(|| resp.get("id"))?;
    json_id_to_string(v)
}

/// Extract the initial message id from a create-chat response, handling the
/// documented `chat.message.id` shape plus the legacy fallbacks.
fn extract_message_id(resp: &Value) -> Option<String> {
    let v = resp
        .get("message_id")
        .or_else(|| {
            resp.get("chat")
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("id"))
        })
        .or_else(|| resp.get("message").and_then(|m| m.get("id")))
        .or_else(|| resp.get("message").and_then(|m| m.get("message_id")))?;
    json_id_to_string(v)
}

/// Normalise a JSON id (string or numeric) to `String`.
fn json_id_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Build the rendered answer payload from a completed message-details body.
fn render_answer(chat_id: &str, message_id: &str, msg_data: &Value) -> Value {
    let msg = msg_data.get("message").unwrap_or(msg_data);
    let state = extract_string_field(msg, "state").unwrap_or_default();
    // The response text lives at top-level `text` (ai.txt:761) or
    // `response.text`; accept either.
    let response_text = msg_data
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| {
            msg.get("response")
                .and_then(|r| r.get("text"))
                .and_then(Value::as_str)
        })
        .or_else(|| msg.get("text").and_then(Value::as_str))
        .unwrap_or_default();

    let mut result = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "state": state,
        "response": response_text,
    });
    // Citations may appear at top-level, `response.citations`, or `citations`.
    let citations = msg_data
        .get("citations")
        .cloned()
        .or_else(|| extract_citations(msg));
    if let Some(c) = citations
        && let Some(obj) = result.as_object_mut()
    {
        obj.insert("citations".to_owned(), c);
    }
    result
}

/// List messages in a chat (context-aware).
async fn messages(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    chat_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let mut params = std::collections::HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let sub = format!("agent/{}/messages/list/", urlencoding::encode(chat_id));
    let p = if params.is_empty() {
        None
    } else {
        Some(&params)
    };
    let value = api::ai::ai_api(&client, profile_type, profile_id, &sub, "GET", None, p)
        .await
        .context("failed to list chat messages")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Show a single message's details (context-aware).
async fn message(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    chat_id: &str,
    message_id: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let sub = format!(
        "agent/{}/message/{}/details/",
        urlencoding::encode(chat_id),
        urlencoding::encode(message_id),
    );
    let value = api::ai::ai_api(&client, profile_type, profile_id, &sub, "GET", None, None)
        .await
        .context("failed to get message details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List the caller's chats (context-aware; `kind`/`deleted` filters).
async fn list_chats(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    kind: Option<&str>,
    deleted: bool,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let mut params = std::collections::HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if let Some(k) = kind {
        params.insert("kind".to_owned(), k.to_owned());
    }
    let sub = if deleted {
        "agent/list/deleted"
    } else {
        "agent/list/"
    };
    let p = if params.is_empty() {
        None
    } else {
        Some(&params)
    };
    let value = api::ai::ai_api(&client, profile_type, profile_id, sub, "GET", None, p)
        .await
        .context("failed to list chats")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List recent AI token-usage transactions (workspace-only).
async fn transactions(ctx: &CommandContext<'_>, workspace: &str) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;
    let value = api::ai::transactions(&client, workspace)
        .await
        .context("failed to list AI transactions")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// AI-generate a title/description for a share (share-only).
async fn autotitle(
    ctx: &CommandContext<'_>,
    share: &str,
    user_context: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(!share.trim().is_empty(), "share ID must not be empty");
    let client = ctx.build_client()?;
    let value = api::ai::autotitle(&client, share, user_context)
        .await
        .context("failed to generate share title")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Dispatch a memory subcommand to the `ai_memory` API.
async fn memory(ctx: &CommandContext<'_>, cmd: &MemoryCommand) -> Result<()> {
    let client = ctx.build_client()?;
    let value = match cmd {
        MemoryCommand::Get(target) => {
            let (scope, id) = memory_scope(target);
            api::ai_memory::get(&client, scope, id)
                .await
                .context("failed to read AI memory")?
        }
        MemoryCommand::Set {
            target,
            content,
            revision,
        } => {
            let (scope, id) = memory_scope(target);
            api::ai_memory::set(&client, scope, id, content, *revision)
                .await
                .context("failed to write AI memory")?
        }
        MemoryCommand::Delete(target) => {
            let (scope, id) = memory_scope(target);
            api::ai_memory::delete(&client, scope, id)
                .await
                .context("failed to delete AI memory")?
        }
    };
    ctx.output.render(&value)?;
    Ok(())
}

/// Map an internal [`MemoryTarget`] to the `ai_memory` scope + id.
fn memory_scope(target: &MemoryTarget) -> (api::ai_memory::MemoryScope, &str) {
    match target {
        MemoryTarget::Org(id) => (api::ai_memory::MemoryScope::Org, id),
        MemoryTarget::Workspace(id) => (api::ai_memory::MemoryScope::Workspace, id),
    }
}

/// The shared "delegation not yet available" message emitted by every hidden
/// delegated-job stub. Returns an `Err` so a caller/script sees a non-success
/// exit, but with a clear, non-alarming message rather than a guessed endpoint
/// call. The server delegation contract is not finalized (see
/// `phase2.log` → "Delegated-job contract — OPEN QUESTIONS"); these commands
/// MUST NOT call any endpoint.
fn delegated_jobs_unavailable() -> Result<()> {
    anyhow::bail!(
        "Ripley delegated jobs are not yet available — the server delegation contract is \
         being finalized. Use `fastio ripley ask` for synchronous Q&A in the meantime."
    )
}

#[cfg(test)]
mod phase2_tests {
    use super::{
        MemoryTarget, PollAction, classify_poll_error, delegated_jobs_unavailable, extract_chat_id,
        extract_message_id, memory_scope,
    };
    use fastio_cli::api::ai_memory::MemoryScope;
    use fastio_cli::error::{ApiError, CliError};
    use serde_json::json;

    #[test]
    fn fatal_wait_error_is_surfaced_not_swallowed() {
        // FIX G: a persistent 4xx during the Ripley `ask`/`chat` wait must be
        // classified Fatal so the loop returns `WaitOutcome::Failed(err)`
        // instead of swallowing it and looping to a misleading timeout. The
        // `ask`/`chat` loops reuse this exact `classify_poll_error`.
        for status in [403u16, 404, 402] {
            let err = CliError::Api(ApiError::new(0, None, "boom".to_owned(), status));
            assert!(
                matches!(classify_poll_error(err), PollAction::Fatal(_)),
                "HTTP {status} during the wait must be Fatal (surfaced), not swallowed"
            );
        }
        // A 500 is transient (keeps polling), not fatal.
        let transient = CliError::Api(ApiError::new(0, None, "boom".to_owned(), 503));
        assert!(matches!(
            classify_poll_error(transient),
            PollAction::RetryTransient
        ));
    }

    #[test]
    fn delegated_jobs_unavailable_returns_pending_error() {
        // The hidden delegated-job stubs MUST emit the "pending" message and
        // call no endpoint. This helper is the sole body of all four arms.
        let err = delegated_jobs_unavailable().expect_err("must be an Err (pending)");
        let msg = err.to_string();
        assert!(
            msg.contains("not yet available"),
            "expected the pending message, got: {msg}"
        );
        assert!(
            msg.contains("delegation contract"),
            "expected the contract-pending wording, got: {msg}"
        );
    }

    #[test]
    fn memory_scope_maps_org_and_workspace() {
        let org = MemoryTarget::Org("o1".to_owned());
        let (scope, id) = memory_scope(&org);
        assert_eq!(scope, MemoryScope::Org);
        assert_eq!(id, "o1");
        let ws = MemoryTarget::Workspace("ws1".to_owned());
        let (scope, id) = memory_scope(&ws);
        assert_eq!(scope, MemoryScope::Workspace);
        assert_eq!(id, "ws1");
    }

    #[test]
    fn extract_ids_handle_documented_chat_message_shape() {
        // ai.txt:288-303 — the documented create response is
        // {"chat": {"id": ..., "message": {"id": ...}}}.
        let resp = json!({"chat": {"id": "C1", "message": {"id": "M1"}}});
        assert_eq!(extract_chat_id(&resp).as_deref(), Some("C1"));
        assert_eq!(extract_message_id(&resp).as_deref(), Some("M1"));
    }

    #[test]
    fn extract_ids_handle_flat_fallback_shape() {
        let resp = json!({"chat_id": "C2", "message_id": "M2"});
        assert_eq!(extract_chat_id(&resp).as_deref(), Some("C2"));
        assert_eq!(extract_message_id(&resp).as_deref(), Some("M2"));
    }

    #[test]
    fn extract_ids_normalize_numeric() {
        let resp = json!({"chat_id": 12345, "message_id": 678});
        assert_eq!(extract_chat_id(&resp).as_deref(), Some("12345"));
        assert_eq!(extract_message_id(&resp).as_deref(), Some("678"));
    }

    #[test]
    fn extract_ids_missing_returns_none() {
        let resp = json!({"result": true});
        assert!(extract_chat_id(&resp).is_none());
        assert!(extract_message_id(&resp).is_none());
    }

    #[test]
    fn render_answer_pulls_text_and_citations() {
        use super::render_answer;
        // Top-level `text` (ai.txt:761) plus citations.
        let msg = json!({
            "message": {"state": "complete"},
            "text": "the answer",
            "citations": [{"nodeId": "n1"}],
        });
        let out = render_answer("C1", "M1", &msg);
        assert_eq!(out.get("state").and_then(|v| v.as_str()), Some("complete"));
        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some("the answer")
        );
        assert!(
            out.get("citations").is_some(),
            "citations should be surfaced"
        );
    }
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
