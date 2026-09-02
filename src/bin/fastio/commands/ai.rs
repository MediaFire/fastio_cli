/// Ripley (formerly `ai`) command implementations for `fastio ripley *`.
///
/// Handles AI chat, chat history, and workspace summaries. Search is NOT a
/// Ripley verb — `fastio search workspace` / `fastio search share` (unified,
/// grouped buckets) is the search surface; `fastio files search` is the flat
/// filename/content leg.
/// The clap group is `ripley` with a hidden `ai` back-compat alias; this
/// module retains the `ai`-prefixed internal names for now — only the command
/// surface was renamed, not the internal module layout.
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use super::CommandContext;
use super::{PollAction, classify_poll_error};
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
        /// Replay guard — reuse a previous turn's key instead of billing a new turn.
        idempotency_key: Option<String>,
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
        /// Replay guard — reuse a previous turn's key instead of billing a new turn.
        idempotency_key: Option<String>,
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
            idempotency_key,
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
                idempotency_key.as_deref(),
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
            match api::ai::publish_chat(&client, profile_type, profile_id, chat_id).await {
                Ok(value) => {
                    ctx.output.render(&value)?;
                    Ok(())
                }
                Err(e) => Err(map_publish_error(e)),
            }
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
        // All four delegated-job stubs share the same guarded "pending"
        // response and call no endpoint (the server delegation contract is
        // not finalized).
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
            idempotency_key,
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
                idempotency_key.as_deref(),
            )
            .await
        }
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

/// Extract citations from an AI message response.
///
/// Citations live at **`result.citations`**, per the published API docs.
/// `response.citations` and a bare `citations` are pre-2026-08-31 locations the
/// schema explicitly says do not exist; they are kept as fallbacks after the
/// real one so an older deployment still renders.
fn extract_citations(msg: &Value) -> Option<Value> {
    msg.get("result")
        .and_then(|r| r.get("citations"))
        .or_else(|| msg.get("response").and_then(|r| r.get("citations")))
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
/// On the migrated `/ai/agent/` contract a file reference's version is
/// optional — an empty version auto-resolves to the current version
/// server-side (`build_references` emits `"version_id": ""`). Therefore:
/// - a fully-qualified `nodeId:versionId` entry passes through as-is (trimmed);
/// - a BARE node id (no version, or a trailing-colon empty version) is kept as
///   the bare `nodeId` — it is NO LONGER an error;
/// - whitespace-only / empty entries are dropped.
///
/// Errors only if nothing valid remains after filtering.
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
            // Bare id, or trailing-colon empty version → keep the node id alone;
            // `build_references` resolves the empty version server-side.
            Some((node, _)) if !node.trim().is_empty() => {
                pairs.push(node.trim().to_owned());
            }
            None => {
                pairs.push(n.to_owned());
            }
            // An empty node half (e.g. `:v1`) is unusable — drop it.
            Some(_) => {}
        }
    }
    anyhow::ensure!(
        !pairs.is_empty(),
        "`--node-ids` contained no valid node ids; \
         use `--files-scope nodeId[:versionId]` instead"
    );
    Ok(pairs.join(","))
}

/// Normalise a legacy `--folder-id` value into a `folders_scope` entry.
///
/// The migrated `/ai/agent/` contract has no folder-depth field, so any
/// `:depth` suffix is dropped. Accepts `nodeId` or `nodeId:depth`; trims,
/// rejects an empty node id, and returns just the node id.
fn folders_scope_from_legacy_folder_id(folder_id: &str) -> Result<String> {
    let fid = folder_id.trim();
    anyhow::ensure!(!fid.is_empty(), "`--folder-id` must not be empty");
    let node = match fid.split_once(':') {
        Some((node, _depth)) => node.trim(),
        None => fid,
    };
    anyhow::ensure!(
        !node.is_empty(),
        "`--folder-id` entry `{fid}` has an empty node id"
    );
    Ok(node.to_owned())
}

/// Resolve the effective [`ChatScope`] from the visible and legacy flags.
///
/// Precedence: the preferred `--files-scope` / `--folders-scope` /
/// `--files-attach` flags WIN. If a preferred flag and its legacy counterpart
/// are BOTH supplied, that is a hard error (the user should pick one). When
/// only a legacy flag is given, it is translated (with a one-time stderr
/// deprecation note) into the corresponding scope field:
/// - `--node-ids` -> `files_scope` (bare ids are accepted; the version
///   auto-resolves — see [`files_scope_from_legacy_node_ids`]).
/// - `--folder-id` -> `folders_scope` (depth is dropped — see
///   [`folders_scope_from_legacy_folder_id`]).
/// - `--intelligence` -> no longer maps to a chat parameter; ignored with a
///   deprecation warning.
///
/// On the migrated `/ai/agent/` contract `files_scope`/`folders_scope` and
/// `files_attach` all collapse into the single `references` array
/// ([`fastio_cli::api::ai::build_references`]), so they may now be combined —
/// there is NO mutual-exclusion check.
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
                "[deprecated] `--folder-id` is translated to `folders_scope`. \
                 Prefer `--folders-scope nodeId`."
            );
        }
        scope.folders_scope = Some(folders_scope_from_legacy_folder_id(fid)?);
    }

    // files_attach: no legacy counterpart; just forward (trim, reject empty).
    // On the migrated /ai/agent/ contract files_attach may be freely combined
    // with files_scope/folders_scope — all collapse into the single
    // `references` array — so there is no mutual-exclusion check here.
    if let Some(fa) = flags.files_attach {
        let fa = fa.trim();
        anyhow::ensure!(!fa.is_empty(), "`--files-attach` must not be empty");
        scope.files_attach = Some(fa.to_owned());
    }

    if flags.intelligence.is_some() && !quiet {
        eprintln!(
            "[deprecated] `--intelligence` no longer maps to a chat parameter and is ignored; \
             scope flags now control indexing reach."
        );
    }

    Ok(scope)
}

/// Send a chat message and poll for the AI response.
#[allow(clippy::too_many_lines)]
async fn chat(
    ctx: &CommandContext<'_>,
    workspace: &str,
    message: &str,
    existing_chat_id: Option<&str>,
    flags: &ChatFlags<'_>,
    idempotency_key: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );

    let scope = resolve_chat_scope(ctx.output.quiet, flags)?;

    let client = ctx.build_client()?;

    // Minted HERE, not inside the API layer, so a failure can quote it back. A
    // key the caller never sees cannot be re-sent, and re-sending it is the
    // entire point — see `api::ai::new_idempotency_key`.
    let idempotency_key = idempotency_key.map_or_else(api::ai::new_idempotency_key, str::to_owned);

    // Step 1: Create chat or send message to existing chat
    let (chat_id, initial_response) = if let Some(cid) = existing_chat_id {
        let resp = match api::ai::send_message(
            &client,
            workspace,
            cid,
            message,
            None,
            &scope,
            Some(&idempotency_key),
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                return Err(map_ai_send_error(
                    e,
                    "failed to send AI message",
                    &idempotency_key,
                ));
            }
        };
        (cid.to_owned(), resp)
    } else {
        let mut options = ChatCreateOptions::default();
        options.personality = Some("detailed".to_owned());
        options.scope = scope;
        options.idempotency_key = Some(idempotency_key.clone());
        let resp =
            match api::ai::create_chat(&client, workspace, message, "chat_with_files", &options)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    return Err(map_ai_send_error(
                        e,
                        "failed to create AI chat",
                        &idempotency_key,
                    ));
                }
            };
        let cid =
            extract_chat_id(&resp).ok_or_else(|| anyhow::anyhow!("no chat_id in response"))?;
        (cid, resp)
    };

    // Step 2: Extract message_id from the response (probes `turn.turn_id` first,
    // then the legacy fallbacks — handles both string and numeric IDs).
    let message_id = extract_message_id(&initial_response)
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
        // Render the structured body first so machine consumers still get it,
        // then exit NON-ZERO: the turn ended without an answer, and a shell
        // `ask && next` must not run `next`.
        WaitOutcome::TurnFailed(msg) => {
            let result = render_answer(&chat_id, &message_id, &msg);
            ctx.output.render(&result)?;
            let state = result
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("failed");
            anyhow::bail!(
                "the AI turn ended as `{state}` without producing an answer \
                 (chat_id={chat_id}, message_id={message_id})"
            )
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
        let mut value = api::ai::list_messages(&client, workspace, cid, offset)
            .await
            .context("failed to list chat messages")?;
        // This endpoint declares no `limit` — the published API docs say it
        // "returns all messages" — so a `?limit=` query parameter is swallowed
        // server-side. Applying the limit to the returned items keeps the flag
        // honest instead of leaving it a no-op that looks like it works.
        truncate_message_items(&mut value, limit);
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

/// Cap `messages.items` to `limit`, keeping `messages.count` truthful.
///
/// `count` is documented in the published API docs as "Number of turn items
/// returned in `items`", so trimming `items` without trimming `count` would
/// emit a self-contradicting envelope — the exact kind of quiet inconsistency a
/// consumer paginating on `count` would trip over.
fn truncate_message_items(value: &mut Value, limit: Option<u32>) {
    let Some(limit) = limit.map(|l| l as usize) else {
        return;
    };
    let Some(messages) = value.get_mut("messages").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(items) = messages.get_mut("items").and_then(Value::as_array_mut)
        && items.len() > limit
    {
        items.truncate(limit);
        let n = items.len();
        messages.insert("count".to_owned(), Value::from(n));
    }
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
    // The `/ai/share/` endpoint caps `files` at 1-25 per the published API
    // docs; reject oversized requests client-side before the network round-trip.
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
#[allow(clippy::too_many_lines)]
async fn ask(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    question: &str,
    scope: &AskScopeFlags,
    personality: Option<&str>,
    kind: Option<&str>,
    no_wait: bool,
    idempotency_key: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !profile_id.trim().is_empty(),
        "{profile_type} ID must not be empty"
    );
    anyhow::ensure!(!question.trim().is_empty(), "question must not be empty");
    // `personality` is dead on /ai/agent; kept in the signature for compat but
    // never sent.
    let _ = personality;
    // `kind` is workspace-only; a share rejects it, so it's silently dropped for
    // a share context. Note the drop on stderr (gated on quiet, like the
    // deprecation notices) rather than hard-erroring — keep the call lenient.
    if profile_type == "share" && kind.is_some() && !ctx.output.quiet {
        eprintln!("Note: --kind is workspace-only and was ignored for this share.");
    }

    let client = ctx.build_client()?;

    // Build the create form (always form-encoded). `type`/`personality` are dead
    // on the migrated agent endpoint; file/folder context is the single
    // structured `references` field. files_scope + files_attach collapse into
    // file references, folders_scope into folder references — see
    // `build_references` — so they may be freely combined (no exclusion).
    let mut form = std::collections::HashMap::new();
    form.insert("question".to_owned(), question.to_owned());
    // `kind` is workspace-only — share chats reject it.
    if profile_type == "workspace"
        && let Some(k) = kind
    {
        form.insert("kind".to_owned(), k.to_owned());
    }
    let mut chat_scope = ChatScope::default();
    chat_scope.files_scope = scope.files_scope.clone();
    chat_scope.folders_scope = scope.folders_scope.clone();
    chat_scope.files_attach = scope.files_attach.clone();
    if let Some(references) = api::ai::build_references(&chat_scope) {
        form.insert("references".to_owned(), references);
    }
    if let Some(subjects) = api::ai::build_subjects(&chat_scope) {
        form.insert("subjects".to_owned(), subjects);
    }
    // Third turn-creating path (the context-aware one that serves both workspace
    // and share), and it needs the replay guard for the same reason as the other
    // two: without it a retry bills a second turn.
    let idempotency_key = idempotency_key.map_or_else(api::ai::new_idempotency_key, str::to_owned);
    if !idempotency_key.is_empty() {
        form.insert("idempotency_key".to_owned(), idempotency_key.clone());
    }

    let resp = match api::ai::ai_api_form(&client, profile_type, profile_id, "agent/", &form).await
    {
        Ok(v) => v,
        Err(e) => {
            return Err(map_ai_send_error(
                e,
                "failed to create AI chat",
                &idempotency_key,
            ));
        }
    };

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
        // Render the structured body first so machine consumers still get it,
        // then exit NON-ZERO — see the workspace arm above.
        WaitOutcome::TurnFailed(msg) => {
            let result = render_answer(&chat_id, &message_id, &msg);
            ctx.output.render(&result)?;
            let state = result
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("failed");
            anyhow::bail!(
                "the AI turn ended as `{state}` without producing an answer \
                 (chat_id={chat_id}, message_id={message_id})"
            )
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
    /// The turn reached a terminal status — `complete`, `failed`, `cancelled`,
    /// `lost` or `needs_input` in the documented status set, plus the tolerated
    /// `errored`. Carries the details body.
    ///
    /// **Terminal, not necessarily successful.** `render_answer` splits an
    /// answer from a `needs_input` clarifying question. A turn that ended
    /// without an answer is [`WaitOutcome::TurnFailed`] instead, so a caller
    /// cannot mistake one for the other.
    Complete(Value),
    /// The turn reached a terminal status carrying no answer — `failed`,
    /// `cancelled` and `lost` from the documented status set, plus the
    /// tolerated, undocumented `errored`.
    ///
    /// Separate from [`WaitOutcome::Complete`] so the command can exit non-zero.
    /// Rendering these as a completed answer would let
    /// `fastio ripley ask … && next` run `next` after the AI had failed —
    /// reporting success for a question that was never answered.
    ///
    /// Distinct from [`WaitOutcome::Failed`], which is a TRANSPORT error — the
    /// request itself failed. Here the request succeeded and the AI's turn is
    /// the thing that ended badly.
    TurnFailed(Value),
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
/// Strategy (per the published API docs): long-poll the workspace/share
/// activity endpoint (which returns promptly on any change and otherwise holds
/// ~95s), then fetch message details once to read the authoritative `state`. We
/// do not poll message-details in a tight loop (the documented anti-pattern).
/// The loop is bounded by [`ASK_MAX_WAIT_SECS`] so it cannot hang past a JWT's
/// lifetime; a 401 short-circuits to [`WaitOutcome::AuthExpired`] with a
/// re-auth hint.
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
                // Unwrap the workspace `message` OR share `turn` detail wrapper
                // documented in the published API docs, so a share
                // `needs_input` turn's `state` is read rather than missed —
                // without this the loop polls to a misleading timeout for
                // shares.
                let msg = api::ai::message_detail(&msg_data);
                // The field is `status`, not `state` — see `turn_status`.
                // Reading `state` instead yields `""` on every poll, which runs
                // the full wait budget on every single invocation.
                let state = api::ai::turn_status(msg);
                // Terminal: complete / failed / cancelled / lost /
                // needs_input, per the published API docs. A `needs_input` turn
                // answered with a clarifying question is terminal too — without
                // it here the loop would poll to a misleading timeout.
                // `render_answer` surfaces the clarification question for that
                // case.
                if api::ai::is_failed_state(&state) {
                    return WaitOutcome::TurnFailed(msg_data);
                }
                if api::ai::is_terminal_turn_status(&state) {
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
                        // Clamp the rate-limit backoff to the remaining wait so a
                        // 429 with a long reset cannot push us past the deadline;
                        // a ~0 remaining breaks to the timeout path below.
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        if remaining.is_zero() {
                            return WaitOutcome::TimedOut;
                        }
                        tokio::time::sleep(remaining.min(Duration::from_secs(retry_after_secs)))
                            .await;
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
                    // Clamp the rate-limit backoff (with its 2s floor) to the
                    // remaining wait so a long 429 reset cannot overshoot the
                    // deadline; a ~0 remaining breaks to the timeout path.
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return WaitOutcome::TimedOut;
                    }
                    tokio::time::sleep(remaining.min(Duration::from_secs(retry_after_secs.max(2))))
                        .await;
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

/// Extract the chat id from a create-chat/message response.
///
/// CREATE (`POST …/ai/agent/`) returns `{thread: {thread_id}, ...}` (NESTED),
/// while follow-up MESSAGE-SEND (`POST …/ai/agent/{chat}/message/`) returns a
/// FLAT top-level `{turn_id, thread_id, seq, status, ...}`, per the published
/// API docs. So `thread.thread_id` is probed first, then the flat top-level
/// `thread_id`; the older `chat_id` / `chat.id` / `id` shapes remain as
/// trailing fallbacks.
fn extract_chat_id(resp: &Value) -> Option<String> {
    let v = resp
        .get("thread")
        .and_then(|t| t.get("thread_id"))
        .or_else(|| resp.get("thread_id"))
        .or_else(|| resp.get("chat_id"))
        .or_else(|| resp.get("chat").and_then(|c| c.get("id")))
        .or_else(|| resp.get("id"))?;
    json_id_to_string(v)
}

/// Extract the initial message id from a create-chat/message response.
///
/// CREATE (`POST …/ai/agent/`) returns `{turn: {turn_id}, ...}` (NESTED), while
/// follow-up MESSAGE-SEND (`POST …/ai/agent/{chat}/message/`) returns a FLAT
/// top-level `{turn_id, thread_id, seq, status, ...}` — the created turn's
/// fields are at the top level, NOT under a `turn`/`message` object, per the
/// published API docs. So `turn.turn_id` is probed first, then the flat
/// top-level `turn_id`; the older `message_id` / `chat.message.id` /
/// `message.id` / `message.message_id` shapes remain as trailing fallbacks.
/// Without the flat probe, a follow-up `ripley chat --chat-id …` aborts with
/// "no `message_id` in response".
fn extract_message_id(resp: &Value) -> Option<String> {
    let v = resp
        .get("turn")
        .and_then(|t| t.get("turn_id"))
        .or_else(|| resp.get("turn_id"))
        .or_else(|| resp.get("message_id"))
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
    // Unwrap the workspace `message` OR share `turn` detail wrapper documented
    // in the published API docs, so state/text/response are read from the right
    // object in both contexts.
    let msg = api::ai::message_detail(msg_data);
    // `status`, not `state` — see `api::ai::turn_status`. The local is named for
    // the SERVER field it reads; the rendered key below stays `state` because
    // that is the CLI's own published output shape, which consumers parse.
    let status = api::ai::turn_status(msg);
    // The answer lives at `result.answer` in the published API docs. The
    // pre-2026-08-31 `text` / `response.text` locations are fields the schema
    // says do not exist, and reading them renders an empty answer even on a
    // completed turn.
    //
    // On a `needs_input` turn the answer slot holds the USER's ORIGINAL question
    // echoed back — NOT an answer — so populating `response` from it would
    // render the user's own words as the reply. Leave `response` empty for
    // needs_input; the `clarification` field below carries the real question.
    // Terminal is not the same as successful. `failed`/`cancelled`/`lost` (and
    // the tolerated `errored`) end the wait carrying no answer, so running them
    // through the answer extractor yields `""` — which renders as a finished
    // reply that happens to be empty: exit 0, blank response, no explanation.
    // They get their own rendering instead.
    let failed = api::ai::is_failed_state(&status);
    let response_text = if status == "needs_input" || failed {
        String::new()
    } else {
        api::ai::extract_answer_text(msg_data, msg)
    };

    let mut result = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "state": status,
        "response": response_text,
    });
    if failed {
        // Surface the server's own diagnostic when it sent one — otherwise the
        // user gets our generic sentence and the actual reason is discarded.
        let detail = msg
            .get("error")
            .and_then(|e| {
                e.get("message")
                    .or_else(|| e.get("text"))
                    .and_then(Value::as_str)
            })
            .filter(|s| !s.is_empty());
        let mut text = format!(
            "The turn ended as `{status}` without producing an answer. This is a \
             terminal state — the AI is NOT still working, so re-running the wait \
             will not help."
        );
        if let Some(d) = detail {
            text.push_str(" Server reported: ");
            text.push_str(d);
        }
        text.push_str(" Inspect it with `fastio ripley message <chat_id> <message_id>`.");
        result["message"] = serde_json::Value::String(text);
    }
    // `result.truncated` means the answer is incomplete; the published API docs
    // say to surface it, otherwise a clipped answer is presented as whole.
    // Emitted only when true, to keep the common shape unchanged.
    if api::ai::answer_was_truncated(msg) {
        result["truncated"] = serde_json::Value::Bool(true);
    }
    // Citations live at `result.citations` on the message, per the published
    // API docs, and the probe order matters: a bare `citations` on the raw
    // envelope is an undocumented legacy location and must not outrank the
    // schema's — the same precedence the `clarification` probe follows.
    // `extract_citations` walks the documented location and its own fallbacks;
    // the envelope-level probe stays only as a last resort.
    let citations = extract_citations(msg).or_else(|| msg_data.get("citations").cloned());
    if let Some(c) = citations
        && let Some(obj) = result.as_object_mut()
    {
        obj.insert("citations".to_owned(), c);
    }

    // `needs_input` in the published API docs: the assistant answered with a
    // clarifying question instead of a full response (so `response` is typically
    // empty). Surface the question and how to reply so the user isn't left with
    // a blank answer. The clarifying question lives in the message's
    // `clarification` object (or a bare `question` field on a share turn).
    if status == "needs_input"
        && let Some(obj) = result.as_object_mut()
    {
        let question = api::ai::extract_clarification_question(msg_data);
        let message = match question.as_deref() {
            Some(q) => format!(
                "Ripley needs more information to continue: {q}\n\
                 Reply by sending your answer as a new message in this same chat \
                 (chat_id={chat_id})."
            ),
            None => format!(
                "Ripley needs more information to continue but did not include a question. \
                 Reply by sending a new message in this same chat (chat_id={chat_id})."
            ),
        };
        if let Some(q) = question {
            obj.insert("clarification".to_owned(), Value::String(q));
        }
        obj.insert("message".to_owned(), Value::String(message));
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
    // The offset is a path segment here, not a query parameter, per the
    // published API docs (the Share variant of the endpoint explicitly defers to
    // the workspace one). Sent as `?offset=` it is swallowed by an endpoint that
    // declares no query parameters at all, so `--offset 50` would re-read page
    // 1, and paging through a long chat would loop on the first page forever
    // with no error to show for it.
    let mut sub = format!("agent/{}/messages/list/", urlencoding::encode(chat_id));
    if let Some(o) = offset.filter(|o| *o > 0) {
        sub.push_str(&o.to_string());
    }
    let mut value = api::ai::ai_api(&client, profile_type, profile_id, &sub, "GET", None, None)
        .await
        .context("failed to list chat messages")?;
    // Likewise there is no server-side `limit` — the published API docs say the
    // endpoint "returns all messages" — so a `?limit=` does nothing. Apply it
    // here rather than leave the flag an inert no-op.
    truncate_message_items(&mut value, limit);
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

/// The shared "delegation not yet available" message emitted by every hidden
/// delegated-job stub. Returns an `Err` so a caller/script sees a non-success
/// exit, but with a clear, non-alarming message rather than a guessed endpoint
/// call. The server delegation contract is not finalized, so these commands
/// MUST NOT call any endpoint.
fn delegated_jobs_unavailable() -> Result<()> {
    anyhow::bail!(
        "Ripley delegated jobs are not yet available — the server delegation contract is \
         being finalized. Use `fastio ripley ask` for synchronous Q&A in the meantime."
    )
}

// ─── Error mapping ──────────────────────────────────────────────────────────
//
// Ripley-specific wording lives HERE (and in the MCP mirror), never in the
// global `error.rs` hints which stay resource-agnostic — mirroring the
// File-Share (`map_fileshare_error`) and signing (`map_signing_error`) layers.
// Each mapper reframes the rendered `hint:` line by wrapping the inner
// `ApiError` in `CliError::MappedApi` so the render layer prints OUR override
// instead of the inner `ApiError`'s misleading generic status hint.

/// Override `hint:` line for a publish-disabled failure (HTTP 403). Publishing
/// a chat public is turned off platform-wide per the published API docs, so
/// steer the user to that fact rather than the generic-403 "check that your
/// account has the required role" (the role is not the problem).
const HINT_PUBLISH_DISABLED: &str = "Publishing chats publicly is currently disabled platform-wide. \
     Chats published before this change remain public; new chats cannot be made public.";

/// Override `hint:` line for a "conversation too large" 409 on the AI
/// send/create path. The condition is PERMANENT (a thread only grows), so this
/// overrides the generic-409 "wait a moment and retry" — retrying the same chat
/// cannot succeed; the recovery is to start a NEW chat.
const HINT_CONVERSATION_TOO_LARGE: &str = "This conversation is too large to continue — start a new chat \
     (`fastio ripley ask …`) to keep going. Retrying the same chat will not help.";

/// Map a publish-chat error. A 403 means publishing is disabled platform-wide
/// per the published API contract: reframe it with a clear headline and
/// override the misleading generic-403 hint. Every other error keeps the
/// operation label and its generic suggestion.
fn map_publish_error(err: CliError) -> anyhow::Error {
    const CTX: &str = "failed to publish chat";
    if let CliError::Api(api) = err {
        if api.http_status == 403 {
            return anyhow::Error::from(CliError::MappedApi {
                api,
                hint: Some(HINT_PUBLISH_DISABLED),
            })
            .context(format!(
                "{CTX}: publishing chats publicly is currently disabled platform-wide (403). \
                 Chats published before this change remain public."
            ));
        }
        return anyhow::Error::from(CliError::Api(api)).context(CTX);
    }
    anyhow::Error::from(err).context(CTX)
}

/// Map an AI chat send/create error. A 409 (a conversation-too-large
/// per-call-site code, or any 409 on this path — the size cap is the only
/// documented 409 cause for these endpoints) means the thread exceeded the size
/// cap. That is PERMANENT, so reframe with a clear "start a new chat" headline
/// and override the misleading generic-409 "wait and retry". Every other error
/// keeps the operation label and its generic suggestion.
fn map_ai_send_error(err: CliError, ctx: &'static str, idempotency_key: &str) -> anyhow::Error {
    if let CliError::Api(api) = err {
        // A retryable 409 `SEQUENCE_FAILURE` is the case the key exists for, so
        // the recovery advice has to carry the key itself. Telling someone to
        // "retry the same idempotency key" without telling them what it was is
        // the state this CLI shipped in — advice describing an impossible act,
        // while the retry they actually performed billed a second turn.
        if api.http_status == 409
            && !fastio_cli::api::ai::CONVERSATION_TOO_LARGE_CODES.contains(&api.code)
        {
            return anyhow::Error::from(CliError::Api(api)).context(format!(
                "{ctx}: retry the SAME request with `--idempotency-key {idempotency_key}` — \
                 that replays the turn instead of creating (and billing) a second one. \
                 Do NOT start a new chat."
            ));
        }
        // `api` here shadows the `api` module, so reference the shared const by
        // its fully-qualified path. Match the SPECIFIC `STATE_TOO_LARGE` per-site
        // codes ONLY — NOT a bare 409. The create/message endpoints also return
        // 409 (`APP_CONFLICT`) for a retryable `SEQUENCE_FAILURE` (a sub-second
        // fresh-race; the client should retry the SAME idempotency key, NOT start
        // a new chat), so a blanket 409 → "too large" would give wrong recovery
        // advice. Other 409s fall through to the generic conflict handling.
        if fastio_cli::api::ai::CONVERSATION_TOO_LARGE_CODES.contains(&api.code) {
            return anyhow::Error::from(CliError::MappedApi {
                api,
                hint: Some(HINT_CONVERSATION_TOO_LARGE),
            })
            .context(format!(
                "{ctx}: this conversation is too large to continue (409) — start a new chat to \
                 keep going"
            ));
        }
        return anyhow::Error::from(CliError::Api(api)).context(ctx);
    }
    anyhow::Error::from(err).context(ctx)
}

#[cfg(test)]
mod phase2_tests {
    use super::{
        PollAction, classify_poll_error, delegated_jobs_unavailable, extract_chat_id,
        extract_message_id,
    };
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
    fn extract_ids_handle_migrated_thread_turn_shape() {
        // The migrated /ai/agent/ create response is
        // {result, thread: {thread_id}, turn: {turn_id}} as documented.
        // chat_id comes from thread.thread_id, message_id from turn.turn_id.
        let resp = json!({
            "result": "yes",
            "thread": {"thread_id": "T1"},
            "turn": {"turn_id": "U1"},
        });
        assert_eq!(extract_chat_id(&resp).as_deref(), Some("T1"));
        assert_eq!(extract_message_id(&resp).as_deref(), Some("U1"));
    }

    #[test]
    fn extract_ids_handle_flat_message_send_shape() {
        // The follow-up message-send response is FLAT — the created turn's
        // fields are at the top level, not nested under `turn`/`message`, per
        // the published API docs: message_id = top-level `turn_id`, chat_id =
        // top-level `thread_id`. Without the flat probe a `ripley chat`
        // follow-up aborts with "no message_id in response".
        let resp = json!({
            "result": true,
            "turn_id": "9azfj-flat-turn",
            "thread_id": "9iacg-flat-thread",
            "seq": 2,
            "status": "pending",
        });
        assert_eq!(
            extract_message_id(&resp).as_deref(),
            Some("9azfj-flat-turn")
        );
        assert_eq!(extract_chat_id(&resp).as_deref(), Some("9iacg-flat-thread"));
    }

    #[test]
    fn extract_ids_handle_documented_chat_message_shape() {
        // Legacy fallback: the older create response
        // {"chat": {"id": ..., "message": {"id": ...}}} is still accepted.
        let resp = json!({"chat": {"id": "C1", "message": {"id": "M1"}}});
        assert_eq!(extract_chat_id(&resp).as_deref(), Some("C1"));
        assert_eq!(extract_message_id(&resp).as_deref(), Some("M1"));
    }

    #[test]
    fn extract_ids_thread_turn_wins_over_legacy_fallbacks() {
        // If both the migrated and legacy shapes are present, thread/turn win.
        let resp = json!({
            "thread": {"thread_id": "T9"},
            "turn": {"turn_id": "U9"},
            "chat_id": "OLD_C",
            "message_id": "OLD_M",
        });
        assert_eq!(extract_chat_id(&resp).as_deref(), Some("T9"));
        assert_eq!(extract_message_id(&resp).as_deref(), Some("U9"));
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

    /// `--limit` is applied client-side, and `count` must follow `items`.
    ///
    /// The endpoint declares no `limit` — the published API docs say it
    /// "returns all messages" — so sending the flag as a query parameter would
    /// be ignored. `count` is documented as the number of items returned, so
    /// trimming one without the other would emit a self-contradicting envelope.
    #[test]
    fn message_limit_trims_items_and_count_together() {
        use super::truncate_message_items;
        let mut v = json!({
            "messages": {"count": 3, "items": [{"seq": 1}, {"seq": 2}, {"seq": 3}]}
        });
        truncate_message_items(&mut v, Some(2));
        assert_eq!(v["messages"]["items"].as_array().map(Vec::len), Some(2));
        assert_eq!(v["messages"]["count"], json!(2), "count must follow items");

        // A limit at or above the item count changes nothing at all.
        let mut v2 = json!({"messages": {"count": 3, "items": [{"seq": 1}]}});
        truncate_message_items(&mut v2, Some(9));
        assert_eq!(
            v2["messages"]["count"],
            json!(3),
            "a limit larger than the page must not rewrite the server's count"
        );

        // No limit, and a shape without `messages`, are both no-ops.
        let mut v3 = json!({"messages": {"count": 1, "items": [{"seq": 1}]}});
        truncate_message_items(&mut v3, None);
        assert_eq!(v3["messages"]["items"].as_array().map(Vec::len), Some(1));
        let mut v4 = json!({"unexpected": true});
        truncate_message_items(&mut v4, Some(1));
        assert_eq!(v4, json!({"unexpected": true}));
    }

    /// Pins the citation probe order, which the test below cannot.
    ///
    /// `render_answer_pulls_text_and_citations` populates exactly one location,
    /// so it passes under either ordering — it measures presence, not
    /// precedence. This one populates both the documented `result.citations`
    /// (per the published API docs) and the undocumented envelope-level
    /// `citations`, so it fails if the legacy location is ever probed first.
    #[test]
    fn render_answer_prefers_the_documented_citation_location() {
        use super::render_answer;
        let data = json!({
            "message": {
                "status": "complete",
                "result": {
                    "answer": "the answer",
                    "citations": [{"nodeId": "documented"}],
                },
            },
            // The pre-2026-08-31 location the schema says does not exist.
            "citations": [{"nodeId": "legacy"}],
        });
        let out = render_answer("c1", "m1", &data);
        let rendered = serde_json::to_string(&out).unwrap_or_default();
        assert!(
            rendered.contains("documented"),
            "`result.citations` is the schema location and must win, got: {rendered}"
        );
        assert!(
            !rendered.contains("legacy"),
            "the undocumented envelope-level `citations` must NOT outrank the \
             documented one, got: {rendered}"
        );
    }

    #[test]
    fn render_answer_pulls_text_and_citations() {
        use super::render_answer;
        // The documented top-level `text`, plus citations.
        let msg = json!({
            "message": {"status": "complete", "result": {"answer": "the answer"}},
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
mod phase8_tests {
    use super::{
        HINT_CONVERSATION_TOO_LARGE, HINT_PUBLISH_DISABLED, map_ai_send_error, map_publish_error,
        render_answer,
    };
    use fastio_cli::error::{ApiError, CliError};
    use serde_json::json;

    /// Construct a `CliError::Api` with the given code + HTTP status.
    fn api_err(code: u32, http_status: u16) -> CliError {
        CliError::Api(ApiError::new(code, None, "boom".to_owned(), http_status))
    }

    /// Drive a mapped error through the REAL render path so the test inspects
    /// the `(headline, hint)` a user actually sees on stderr — the load-bearing
    /// surface, since the regression these mappers guard against is the inner
    /// `ApiError`'s GENERIC hint leaking onto the `hint:` line. Mirrors the
    /// File-Share `render_mapped` helper.
    fn render_mapped(err: &anyhow::Error) -> (String, Option<&'static str>) {
        let cli_err = err
            .downcast_ref::<CliError>()
            .expect("a mapped Ripley error must be rooted at a CliError");
        crate::cli_error_render(err, cli_err)
    }

    /// A terminal turn is not necessarily a successful one.
    ///
    /// Treating `failed`/`cancelled`/`lost` as terminal without also teaching
    /// the renderer that terminal != successful makes a cancelled turn produce
    /// `{"state":"cancelled","response":""}` and exit 0 — an empty answer
    /// presented as a finished one. This test pins the failure rendering so that
    /// cannot come back.
    #[test]
    fn a_failed_turn_is_not_rendered_as_an_empty_answer() {
        for status in ["failed", "cancelled", "lost", "errored"] {
            let msg = json!({"message": {"status": status}});
            let out = render_answer("c1", "m1", &msg);
            assert_eq!(out.get("state").and_then(|v| v.as_str()), Some(status));
            assert_eq!(
                out.get("response").and_then(|v| v.as_str()),
                Some(""),
                "{status}: there is no answer to render"
            );
            let message = out
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("{status} must carry an explanatory message"));
            assert!(
                message.contains("without producing an answer"),
                "{status}: message must say no answer was produced, got: {message}"
            );
            assert!(
                message.contains("not still working") || message.contains("NOT still working"),
                "{status}: must say retrying will not help, got: {message}"
            );
        }
    }

    /// A successful turn must NOT pick up the failure message.
    /// Without this, the server's diagnostic is discarded, leaving only our
    /// generic sentence. When `error.message` is present it must reach the user.
    #[test]
    fn a_failed_turn_surfaces_the_servers_own_diagnostic() {
        let msg = json!({
            "message": {"status": "failed", "error": {"message": "model backend unavailable"}}
        });
        let out = render_answer("c1", "m1", &msg);
        let message = out.get("message").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            message.contains("model backend unavailable"),
            "the server's reason must be surfaced, got: {message}"
        );
        // The generic guidance stays alongside it.
        assert!(message.contains("not still working") || message.contains("NOT still working"));
    }

    #[test]
    fn a_completed_turn_carries_no_failure_message() {
        let msg = json!({"message": {"status": "complete", "result": {"answer": "hi"}}});
        let out = render_answer("c1", "m1", &msg);
        assert_eq!(out.get("response").and_then(|v| v.as_str()), Some("hi"));
        assert!(
            out.get("message").is_none(),
            "a completed turn must not carry a failure message"
        );
    }

    #[test]
    fn render_answer_surfaces_clarification_on_needs_input() {
        // A `needs_input` turn carries a clarifying question in a
        // `clarification` object, per the published API docs; render_answer must
        // surface it (and a reply hint) rather than an empty answer.
        let msg = json!({
            "message": {"status": "needs_input"},
            "clarification": {"type": "clarification", "question": "Which workspace?"},
        });
        let out = render_answer("C1", "M1", &msg);
        assert_eq!(
            out.get("state").and_then(|v| v.as_str()),
            Some("needs_input")
        );
        assert_eq!(
            out.get("clarification").and_then(|v| v.as_str()),
            Some("Which workspace?"),
            "the clarifying question must be surfaced: {out}"
        );
        let message = out
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            message.contains("Which workspace?") && message.contains("chat_id=C1"),
            "the message must restate the question and how to reply: {message}"
        );
    }

    #[test]
    fn render_answer_needs_input_without_question_still_guides_reply() {
        // A `needs_input` turn with no clarification object/question still yields
        // a guidance message (and no bogus `clarification` field).
        let msg = json!({"message": {"status": "needs_input"}});
        let out = render_answer("C9", "M9", &msg);
        assert!(
            out.get("clarification").is_none(),
            "no question → no field: {out}"
        );
        let message = out
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            message.contains("more information") && message.contains("chat_id=C9"),
            "must still guide the user to reply: {message}"
        );
    }

    #[test]
    fn render_answer_complete_has_no_clarification_message() {
        // A normal `complete` answer must NOT gain a needs_input clarification
        // message — that path is gated on the state.
        let msg = json!({"message": {"status": "complete", "result": {"answer": "the answer"}}});
        let out = render_answer("C2", "M2", &msg);
        assert!(out.get("clarification").is_none());
        assert!(
            out.get("message").is_none(),
            "no needs_input message on complete: {out}"
        );
    }

    #[test]
    fn render_answer_needs_input_does_not_echo_user_text_into_response() {
        // On a `needs_input` turn the body's `text` is the USER's ORIGINAL
        // question echoed back — it must NOT be rendered as the answer. The
        // `response` field stays empty; the `clarification` carries the actual
        // assistant question.
        let msg = json!({
            "message": {"status": "needs_input", "text": "What were the Q3 figures?"},
            "clarification": {"question": "Which fiscal year?"},
        });
        let out = render_answer("C3", "M3", &msg);
        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some(""),
            "needs_input must not echo the user's text into response: {out}"
        );
        assert_eq!(
            out.get("clarification").and_then(|v| v.as_str()),
            Some("Which fiscal year?"),
            "the assistant's clarifying question must still be surfaced: {out}"
        );
    }

    #[test]
    fn render_answer_share_turn_needs_input_surfaces_clarification() {
        // A SHARE detail wraps the turn under `turn`, not `message`, per the
        // published API docs. render_answer must read state/clarification from
        // the turn wrapper just like the workspace `message` case.
        let msg = json!({
            "turn": {"status": "needs_input", "text": "echoed user question"},
            "clarification": {"type": "clarification", "question": "Which share folder?"},
        });
        let out = render_answer("CS", "MS", &msg);
        assert_eq!(
            out.get("state").and_then(|v| v.as_str()),
            Some("needs_input"),
            "share turn state must be read from the `turn` wrapper: {out}"
        );
        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some(""),
            "share needs_input must not echo the user's text: {out}"
        );
        assert_eq!(
            out.get("clarification").and_then(|v| v.as_str()),
            Some("Which share folder?"),
            "share turn clarification must be surfaced: {out}"
        );
    }

    #[test]
    fn render_answer_share_turn_complete_pulls_response_text() {
        // A complete SHARE turn still renders its answer — `response` comes from
        // the turn's `response.text` (the wrapper-unwrap must not break the
        // happy path).
        let msg = json!({
            "turn": {"status": "complete", "result": {"answer": "the share answer"}},
        });
        let out = render_answer("CS2", "MS2", &msg);
        assert_eq!(out.get("state").and_then(|v| v.as_str()), Some("complete"));
        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some("the share answer"),
            "a complete share turn must surface its answer text: {out}"
        );
    }

    #[test]
    fn publish_403_maps_to_disabled_message_and_hint() {
        // A 403 on publish means publishing is disabled platform-wide — the
        // headline must say so and the rendered hint must be OUR override, not
        // the generic-403 "check that your account has the required role".
        let err = map_publish_error(api_err(0, 403));
        let (headline, hint) = render_mapped(&err);
        assert!(
            headline.to_lowercase().contains("disabled"),
            "headline must say publishing is disabled: {headline}"
        );
        assert_eq!(
            hint,
            Some(HINT_PUBLISH_DISABLED),
            "hint must be the override"
        );
        assert!(
            !hint
                .unwrap_or_default()
                .to_lowercase()
                .contains("required role"),
            "must NOT keep the generic-403 role hint: {hint:?}"
        );
    }

    #[test]
    fn publish_non_403_passes_through_with_label() {
        // A non-403 publish error keeps the operation label and its generic
        // suggestion (e.g. a 406 chat-not-found).
        let err = map_publish_error(api_err(1658, 406));
        let chain = format!("{err:#}");
        assert!(
            chain.contains("failed to publish chat"),
            "label kept: {chain}"
        );
        let (_h, hint) = render_mapped(&err);
        assert_ne!(
            hint,
            Some(HINT_PUBLISH_DISABLED),
            "a non-403 must NOT claim publishing is disabled"
        );
    }

    #[test]
    fn ai_send_bare_409_sequence_failure_not_mislabeled_too_large() {
        // R6 precision: the create/message endpoints return 409 (`APP_CONFLICT`)
        // for BOTH `STATE_TOO_LARGE` (start a new chat) AND a retryable
        // `SEQUENCE_FAILURE` (a fresh-race; retry the same idempotency key). A
        // bare 409 WITHOUT a too-large per-site code must NOT be mislabeled
        // "too large" — that would give the wrong recovery advice. It falls
        // through to the generic conflict handling, keeping the operation label.
        let err = map_ai_send_error(api_err(0, 409), "failed to create AI chat", "test-key");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("failed to create AI chat"),
            "generic label kept for a non-too-large 409: {chain}"
        );
        let (_headline, hint) = render_mapped(&err);
        assert_ne!(
            hint,
            Some(HINT_CONVERSATION_TOO_LARGE),
            "a bare 409 (e.g. SEQUENCE_FAILURE) must NOT claim the conversation is too large"
        );
    }

    #[test]
    fn ai_send_too_large_code_maps_even_without_409_status() {
        // The specific per-call-site codes map to the too-large hint regardless
        // of the HTTP status the transport reports.
        for code in [168_116u32, 153_795, 148_135, 144_657] {
            let err =
                map_ai_send_error(api_err(code, 400), "failed to send AI message", "test-key");
            let (_h, hint) = render_mapped(&err);
            assert_eq!(
                hint,
                Some(HINT_CONVERSATION_TOO_LARGE),
                "code {code} must map"
            );
        }
    }

    #[test]
    fn ai_send_non_409_passes_through_with_label() {
        // An unrelated error (e.g. a 402 billing) keeps the operation label and
        // its own generic suggestion — it must NOT be mislabeled too-large.
        let err = map_ai_send_error(api_err(0, 402), "failed to create AI chat", "test-key");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("failed to create AI chat"),
            "label kept: {chain}"
        );
        let (_h, hint) = render_mapped(&err);
        assert_ne!(
            hint,
            Some(HINT_CONVERSATION_TOO_LARGE),
            "a non-409 must NOT claim the conversation is too large"
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
    fn bare_node_id_is_now_accepted() {
        // On the migrated /ai/agent/ contract the version is optional (it
        // auto-resolves), so a bare node id is kept as-is (no more error).
        let nodes = vec!["abc".to_owned()];
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                ..flags()
            },
        )
        .expect("bare node id is now accepted");
        assert_eq!(scope.files_scope.as_deref(), Some("abc"));
    }

    #[test]
    fn node_id_with_empty_version_half_is_kept_as_bare_id() {
        // `abc:` (trailing colon, empty version) → kept as the bare node id
        // `abc`; build_references emits `"version_id": ""` downstream.
        let nodes = vec!["abc:".to_owned()];
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                ..flags()
            },
        )
        .expect("empty version half is now accepted as a bare node id");
        assert_eq!(scope.files_scope.as_deref(), Some("abc"));
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
    fn folder_id_maps_to_folders_scope_bare_node() {
        // The migrated contract has no folder-depth field — a bare folder id
        // maps to just the node id (no `:10` appended).
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                folder_id: Some("F123"),
                ..flags()
            },
        )
        .expect("bare folder id maps to the node id");
        assert_eq!(scope.folders_scope.as_deref(), Some("F123"));
        assert!(scope.files_scope.is_none());
    }

    #[test]
    fn folder_id_qualified_depth_is_dropped() {
        // A `nodeId:depth` folder id keeps only the node — depth is dropped.
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                folder_id: Some("F123:3"),
                ..flags()
            },
        )
        .expect("nodeId:depth folder id keeps only the node");
        assert_eq!(scope.folders_scope.as_deref(), Some("F123"));
    }

    #[test]
    fn folder_id_out_of_range_depth_is_dropped_not_rejected() {
        // Depth is no longer validated (it's dropped), so an out-of-range
        // depth is no longer an error — the node survives.
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                folder_id: Some("F123:99"),
                ..flags()
            },
        )
        .expect("depth is dropped, so 99 is no longer an error");
        assert_eq!(scope.folders_scope.as_deref(), Some("F123"));
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
        // `files_scope` + `folders_scope` reach the scope verbatim. (On the
        // migrated contract they may also be combined with `files_attach` —
        // covered by `files_attach_merges_with_files_scope` below.)
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
    fn files_attach_merges_with_files_scope() {
        // On the migrated /ai/agent/ contract files_scope/folders_scope become
        // `references` while files_attach becomes the SEPARATE `subjects` array,
        // so they may be combined — no longer a client-side error. The resolved
        // scope keeps both fields populated verbatim (build_references /
        // build_subjects emit the two arrays).
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                files_scope: Some("n1:v1"),
                files_attach: Some("a1:v1"),
                ..flags()
            },
        )
        .expect("files_attach + files_scope are now allowed (they merge)");
        assert_eq!(scope.files_scope.as_deref(), Some("n1:v1"));
        assert_eq!(scope.files_attach.as_deref(), Some("a1:v1"));
    }

    #[test]
    fn files_attach_merges_with_legacy_node_ids() {
        // The legacy `--node-ids` -> files_scope translation combined with
        // `--files-attach` is also allowed now (references + subjects).
        let nodes = vec!["abc:v1".to_owned()];
        let scope = resolve_chat_scope(
            true,
            &ChatFlags {
                node_ids: Some(&nodes),
                files_attach: Some("a1:v1"),
                ..flags()
            },
        )
        .expect("files_attach + legacy --node-ids are now allowed (they merge)");
        assert_eq!(scope.files_scope.as_deref(), Some("abc:v1"));
        assert_eq!(scope.files_attach.as_deref(), Some("a1:v1"));
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
