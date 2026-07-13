#![allow(clippy::missing_errors_doc)]

/// AI API endpoints for the Fast.io REST API.
///
/// Maps to endpoints documented at `/current/workspace/{id}/ai/`.
/// Supports chat creation, message send/read, semantic search, and summarize.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::{ApiError, CliError};

/// Optional scope/attachment parameters shared by chat create and follow-up
/// message sends.
///
/// These fields carry the caller-facing CSV inputs (still populated from the
/// `--files-scope` / `--folders-scope` / `--files-attach` flags and the MCP
/// args of the same names):
/// - `files_scope` — comma-separated file `nodeId:versionId` pairs → SCOPE file
///   items in the `references` array (context the message text discusses).
/// - `files_attach` — comma-separated file `nodeId:versionId` pairs → ATTACHED
///   file items in the SEPARATE `subjects` array (the platform's focus surface
///   for attachments); same item shape as a reference.
/// - `folders_scope` — comma-separated `nodeId:depth` folder pairs. The depth
///   half is **dropped** (the migrated contract has no folder-depth field); only
///   the node id survives, as a FOLDER reference item.
///
/// The wire contract is NOT these three fields. [`apply_scope`] emits two
/// structured JSON arrays: [`build_references`] converts `files_scope` +
/// `folders_scope` into the `references` field, and [`build_subjects`] converts
/// `files_attach` into the SEPARATE `subjects` field (the legacy
/// `files_scope`/`folders_scope`/`files_attach` form fields are dead on
/// `/ai/agent/` — the backend silently drops unknown keys). See
/// [`build_ref_array`] for the exact item shapes; an empty/absent version
/// resolves to `"version_id": ""`, which the backend auto-resolves to the
/// current version.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ChatScope {
    /// Comma-separated file `nodeId:versionId` scope pairs → FILE references.
    pub files_scope: Option<String>,
    /// Comma-separated `nodeId:depth` folder pairs → FOLDER references (depth
    /// is dropped).
    pub folders_scope: Option<String>,
    /// Comma-separated file `nodeId:versionId` attachment pairs → FILE items in
    /// the SEPARATE `subjects` array (same item shape as a reference).
    pub files_attach: Option<String>,
}

/// Optional create-time chat parameters beyond the required `type`/`question`.
///
/// Mirrors the `/ai/agent/` create contract
/// (`~/vividengine/llms/ai.txt:263-273`). `privacy`/`kind` are
/// workspace-only — share chats reject them; the caller is responsible for
/// not populating them in a share context.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ChatCreateOptions {
    /// `private` or `public` (workspace-only; share chats are always private).
    pub privacy: Option<String>,
    /// Chat display name (auto-generated from the question if omitted).
    pub name: Option<String>,
    /// `concise` or `detailed`. **Dead on `/ai/agent/`** — retained for
    /// signature/back-compat but no longer sent (see [`create_chat`]).
    pub personality: Option<String>,
    /// `user` or `agent` (workspace-only; immutable after creation).
    pub kind: Option<String>,
    /// File/folder scope and attachment parameters.
    pub scope: ChatScope,
}

/// Create a new AI chat session.
///
/// `POST /workspace/{workspace_id}/ai/agent/` (or the share variant via the
/// `ai_api*` path builders). The body is **form-encoded** per the
/// `/ai/agent/` contract (`~/vividengine/llms/ai.txt:251-284`). File/folder
/// context is carried by the single structured `references` form field (see
/// [`ChatScope`] / [`build_references`]); the legacy JSON
/// `nodes`/`folder_id`/`intelligence` fields AND the flat
/// `files_scope`/`folders_scope`/`files_attach`/`type`/`personality` form
/// fields are dead on the migrated agent endpoint.
pub async fn create_chat(
    client: &ApiClient,
    workspace_id: &str,
    question: &str,
    chat_type: &str,
    options: &ChatCreateOptions,
) -> Result<Value, CliError> {
    // `chat_type` is dead on /ai/agent; kept for signature compat.
    let _ = chat_type;
    let mut form = HashMap::new();
    form.insert("question".to_owned(), question.to_owned());
    if let Some(p) = &options.privacy {
        form.insert("privacy".to_owned(), p.clone());
    }
    if let Some(n) = &options.name {
        form.insert("name".to_owned(), n.clone());
    }
    // `personality` is dead on /ai/agent; the field is kept on
    // `ChatCreateOptions` for signature compat but no longer sent.
    let _ = &options.personality;
    if let Some(k) = &options.kind {
        form.insert("kind".to_owned(), k.clone());
    }
    apply_scope(&mut form, &options.scope);
    let path = format!("/workspace/{}/ai/agent/", urlencoding::encode(workspace_id),);
    client.post(&path, &form).await
}

/// Insert the structured `references` and `subjects` fields into a form body.
///
/// On the migrated `/ai/agent/` endpoint file context is carried by TWO distinct
/// structured arrays (the legacy `files_scope`/`folders_scope`/`files_attach`
/// form fields are dead — the backend drops unknown keys):
/// - `references` (SCOPE) — from `files_scope` + `folders_scope`. The platform
///   routes it to the gRPC `ChatQuery.content[]` (inline "pill at end of the
///   message").
/// - `subjects` (ATTACH) — from `files_attach`. The platform routes it to the
///   gRPC `ChatQuery.focus.subjects[]`, the intended surface for ATTACHED files.
///
/// Each field is omitted when it resolves to no items.
fn apply_scope(form: &mut HashMap<String, String>, scope: &ChatScope) {
    if let Some(references) = build_references(scope) {
        form.insert("references".to_owned(), references);
    }
    if let Some(subjects) = build_subjects(scope) {
        form.insert("subjects".to_owned(), subjects);
    }
}

/// Build a `/ai/agent/` structured reference array — the shared JSON shape used
/// by BOTH the `references` and `subjects` form fields — from CSV inputs.
///
/// Each `file_csvs` entry is a comma-separated list of file `nodeId` /
/// `nodeId:versionId` → FILE items
/// `{"type":"file","id":<node>,"file_details":{"node_id":<node>,"version_id":<ver or "">}}`
/// (empty/absent version → `""`, which the backend auto-resolves to the current
/// version). `folder_csv` is a comma-separated list of folder `nodeId` /
/// `nodeId:depth` (depth dropped) → FOLDER items
/// `{"type":"folder","id":<node>,"folder_details":{"node_id":<node>}}`.
///
/// Entries are trimmed; empty entries skipped; items DEDUPED by `(type, id)`
/// (first occurrence wins). Returns `None` when no items result (so the caller
/// omits the field). `serde_json::to_string` of a `Vec<Value>` never fails, so
/// the fallback arm only satisfies the type.
fn build_ref_array(file_csvs: &[Option<&str>], folder_csv: Option<&str>) -> Option<String> {
    let mut items: Vec<Value> = Vec::new();
    let mut seen: Vec<(&'static str, String)> = Vec::new();

    for csv in file_csvs.iter().copied().flatten() {
        for entry in csv.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            // `nodeId` or `nodeId:versionId` — empty/absent version → "".
            let (node, version) = match entry.split_once(':') {
                Some((n, v)) => (n.trim(), v.trim()),
                None => (entry, ""),
            };
            if node.is_empty() {
                continue;
            }
            if seen.iter().any(|(t, id)| *t == "file" && id == node) {
                continue;
            }
            seen.push(("file", node.to_owned()));
            items.push(serde_json::json!({
                "type": "file",
                "id": node,
                "file_details": { "node_id": node, "version_id": version },
            }));
        }
    }

    // Folders from `folder_csv`; the `:depth` half (if any) is dropped.
    if let Some(csv) = folder_csv {
        for entry in csv.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let node = match entry.split_once(':') {
                Some((n, _depth)) => n.trim(),
                None => entry,
            };
            if node.is_empty() {
                continue;
            }
            if seen.iter().any(|(t, id)| *t == "folder" && id == node) {
                continue;
            }
            seen.push(("folder", node.to_owned()));
            items.push(serde_json::json!({
                "type": "folder",
                "id": node,
                "folder_details": { "node_id": node },
            }));
        }
    }

    if items.is_empty() {
        return None;
    }
    Some(serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_owned()))
}

/// Build the `/ai/agent/` **`references`** field (SCOPE — context the message
/// text discusses) from a [`ChatScope`]: `files_scope` (files) + `folders_scope`
/// (folders). ATTACHED files go to `subjects` instead (see [`build_subjects`]).
/// The platform routes `references` to the gRPC `ChatQuery.content[]`.
#[must_use]
pub fn build_references(scope: &ChatScope) -> Option<String> {
    build_ref_array(
        &[scope.files_scope.as_deref()],
        scope.folders_scope.as_deref(),
    )
}

/// Build the `/ai/agent/` **`subjects`** field (ATTACHED subject files) from a
/// [`ChatScope`]'s `files_attach`. The platform routes `subjects` to the gRPC
/// `ChatQuery.focus.subjects[]` — the intended surface for attachments — whereas
/// `references` becomes inline `content[]`. Same item shape as a reference
/// (see [`build_ref_array`]); folders are not attachable, so files only.
#[must_use]
pub fn build_subjects(scope: &ChatScope) -> Option<String> {
    build_ref_array(&[scope.files_attach.as_deref()], None)
}

/// Send a message to an existing AI chat.
///
/// `POST /workspace/{workspace_id}/ai/agent/{chat_id}/message/`. The body is
/// **form-encoded** per the `/ai/agent/` contract
/// (`~/vividengine/llms/ai.txt:563-601`). The chat type is inherited from
/// the chat, so `type` is not resent; `personality` is dead on the migrated
/// endpoint and also not sent. File/folder context uses the single structured
/// `references` field (see [`ChatScope`] / [`build_references`]).
pub async fn send_message(
    client: &ApiClient,
    workspace_id: &str,
    chat_id: &str,
    question: &str,
    personality: Option<&str>,
    scope: &ChatScope,
) -> Result<Value, CliError> {
    // `personality` is dead on /ai/agent; the param is kept for signature
    // compat (callers still pass it) but no longer inserted into the form.
    let _ = personality;
    let mut form = HashMap::new();
    form.insert("question".to_owned(), question.to_owned());
    apply_scope(&mut form, scope);
    let path = format!(
        "/workspace/{}/ai/agent/{}/message/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(chat_id),
    );
    client.post(&path, &form).await
}

/// Build the path for the AI chat cancel endpoint.
///
/// `/{context_type}/{context_id}/ai/agent/{chat_id}/cancel/`
///
/// `context_type` must be either `workspace` or `share`. Per the
/// `/ai/agent/` contract (`~/vividengine/llms/ai.txt:617`) the path carries
/// a **trailing slash**. Both segment values are URL-encoded; higher-level
/// validation (whitelist on `context_type`, non-empty IDs) is the caller's
/// responsibility — see `cancel_message`.
fn build_cancel_path(context_type: &str, context_id: &str, chat_id: &str) -> String {
    format!(
        "/{}/{}/ai/agent/{}/cancel/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(chat_id),
    )
}

/// Defensive 2xx-with-error guard for the cancel endpoint.
///
/// The cancel endpoint's HTTP 200 success bodies are
/// `{"success": true, "message": {...}}` or
/// `{"success": true, "no_pending_message": true}` — neither carries a
/// `result` field, so they pass through unchanged. Wire errors (HTTP 406
/// with the flat `{"result": false, "error_message": "...",
/// "error_id": ...}` shape) are converted into `CliError::Api` upstream
/// by `handle_response_raw` + `extract_error`'s flat-envelope fallback,
/// so this function does not normally see them.
///
/// This guard exists for the (currently undocumented) edge case where
/// the server returns HTTP 200 with `result: false` — we recognize all
/// three forms the standard envelope uses (`Bool(false)`, `String("no")`,
/// `Number(0)`) and surface the same `CliError::Api` we'd raise on the
/// wire path, so a future server-side normalization toward the standard
/// envelope cannot silently leak an error body to the renderer as if it
/// were a successful cancel.
fn parse_cancel_response(body: Value) -> Result<Value, CliError> {
    let signals_failure = match body.get("result") {
        Some(Value::Bool(false)) => true,
        Some(Value::String(s)) => s == "no",
        Some(Value::Number(n)) => {
            n.as_u64() == Some(0) || n.as_i64() == Some(0) || n.as_f64() == Some(0.0)
        }
        _ => false,
    };
    if !signals_failure {
        return Ok(body);
    }

    let message = body
        .get("error_message")
        .and_then(Value::as_str)
        .unwrap_or("AI chat cancel rejected by server")
        .to_owned();
    // `error_id` is documented as a numeric ID; accept either a number
    // or a numeric string for forward-compatibility.
    let code = body
        .get("error_id")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    Err(CliError::Api(ApiError {
        code,
        error_code: None,
        message,
        http_status: 406,
        details: None,
    }))
}

/// Cancel an in-progress AI chat message.
///
/// `POST /workspace/{workspace_id}/ai/agent/{chat_id}/cancel/`
/// `POST /share/{share_id}/ai/agent/{chat_id}/cancel/`
///
/// `context_type` must be either `"workspace"` or `"share"`; any other
/// value is rejected before a request is issued so a typo cannot mis-route
/// and silently hit the wrong endpoint. All three IDs are trimmed of
/// surrounding whitespace and rejected if empty after trimming. The
/// endpoint is idempotent — when no non-terminal message exists the
/// server still returns HTTP 200 with `no_pending_message: true` (success
/// from the user's perspective, not an error), and that success body is
/// returned to the caller verbatim.
pub async fn cancel_message(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    chat_id: &str,
) -> Result<Value, CliError> {
    if !matches!(context_type, "workspace" | "share") {
        return Err(CliError::Parse(format!(
            "context_type must be \"workspace\" or \"share\", got {context_type:?}",
        )));
    }
    let context_id = context_id.trim();
    let chat_id = chat_id.trim();
    if context_id.is_empty() {
        return Err(CliError::Parse("context_id must not be empty".to_owned()));
    }
    if chat_id.is_empty() {
        return Err(CliError::Parse("chat_id must not be empty".to_owned()));
    }
    let path = build_cancel_path(context_type, context_id, chat_id);
    // The cancel endpoint's body is documented as **Empty** (ai.txt:625), so
    // send a body-less POST rather than a JSON `{}` — `post_empty_raw` sets
    // no `Content-Type` and no payload. Its HTTP-200 success bodies don't
    // carry a `result` field, so the standard envelope-unwrap (`post_json` →
    // `handle_response`) would reject them as errors. The raw helper returns
    // the body verbatim on 2xx and routes non-2xx through `extract_error`
    // (which also recognizes the cancel endpoint's flat
    // `error_message` / `error_id` shape).
    let body: Value = client.post_empty_raw(&path).await?;
    parse_cancel_response(body)
}

/// Get message details (used for polling).
///
/// `GET /workspace/{workspace_id}/ai/agent/{chat_id}/message/{message_id}/details/`
pub async fn get_message_details(
    client: &ApiClient,
    workspace_id: &str,
    chat_id: &str,
    message_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/ai/agent/{}/message/{}/details/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(chat_id),
        urlencoding::encode(message_id),
    );
    client.get(&path).await
}

/// List messages in a chat.
///
/// `GET /workspace/{workspace_id}/ai/agent/{chat_id}/messages/list/`
pub async fn list_messages(
    client: &ApiClient,
    workspace_id: &str,
    chat_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/ai/agent/{}/messages/list/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(chat_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Optional filters for [`list_chats`].
///
/// Mirrors the `/ai/agent/list/` query surface
/// (`~/vividengine/llms/ai.txt:320-333`):
/// - `kind` — `user` (default), `agent`, or `all` (`ai.txt:331`).
/// - `deleted` — when `true`, the `/deleted` path variant is requested
///   instead (`ai.txt:333`).
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ListChatsOptions {
    /// Maximum number of chats to return.
    pub limit: Option<u32>,
    /// Number of chats to skip for pagination.
    pub offset: Option<u32>,
    /// Filter by chat kind: `user`, `agent`, or `all`. `None` defers to the
    /// server default (`user`).
    pub kind: Option<String>,
    /// List soft-deleted chats via the `/deleted` path variant.
    pub deleted: bool,
}

impl ListChatsOptions {
    /// Construct options carrying only `limit`/`offset` (no `kind` filter,
    /// not deleted). Provided because the `#[non_exhaustive]` attribute
    /// prevents struct-literal construction from other crates.
    #[must_use]
    pub fn paged(limit: Option<u32>, offset: Option<u32>) -> Self {
        Self {
            limit,
            offset,
            kind: None,
            deleted: false,
        }
    }
}

/// List chats in a workspace.
///
/// `GET /workspace/{workspace_id}/ai/agent/list/` (or
/// `.../ai/agent/list/deleted` when `options.deleted` is set —
/// `~/vividengine/llms/ai.txt:333`). The `kind` filter
/// (`user`/`agent`/`all`, `ai.txt:331`) is threaded through as a query param.
pub async fn list_chats(
    client: &ApiClient,
    workspace_id: &str,
    options: &ListChatsOptions,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = options.limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = options.offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if let Some(k) = &options.kind {
        params.insert("kind".to_owned(), k.clone());
    }
    let suffix = if options.deleted {
        "list/deleted"
    } else {
        "list/"
    };
    let path = format!(
        "/workspace/{}/ai/agent/{suffix}",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// List AI token-usage transactions for a workspace.
///
/// `GET /workspace/{workspace_id}/ai/transactions/`
///
/// Returns up to 40 most-recent transactions. **Workspace-only** — there is
/// no share equivalent (`~/vividengine/llms/ai.txt:935-981`); the caller is
/// responsible for rejecting a share context before calling this.
pub async fn transactions(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/ai/transactions/",
        urlencoding::encode(workspace_id),
    );
    client.get(&path).await
}

/// AI-generate a title, description, and display type for a share.
///
/// `POST /share/{share_id}/ai/autotitle/`
///
/// **Share-only** (`~/vividengine/llms/ai.txt:1079-1112`); the caller must
/// reject a workspace context before calling this. The optional
/// `user_context` form field guides generation. Generated values are applied
/// directly to the share server-side.
pub async fn autotitle(
    client: &ApiClient,
    share_id: &str,
    user_context: Option<&str>,
) -> Result<Value, CliError> {
    let path = format!("/share/{}/ai/autotitle/", urlencoding::encode(share_id),);
    let mut form = HashMap::new();
    if let Some(c) = user_context {
        form.insert("user_context".to_owned(), c.to_owned());
    }
    client.post(&path, &form).await
}

/// Get full details (and history) for a single chat.
///
/// `GET /{context_type}/{context_id}/ai/agent/{chat_id}/details/`
///
/// `context_type` must be `workspace` or `share`; any other value is rejected
/// before a request is issued so a typo cannot mis-route.
pub async fn chat_details(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    chat_id: &str,
) -> Result<Value, CliError> {
    let path = agent_chat_path(context_type, context_id, chat_id, "details/")?;
    client.get(&path).await
}

/// Rename a chat.
///
/// `POST /{context_type}/{context_id}/ai/agent/{chat_id}/update/` with a
/// **form-encoded** `name` field. The chat `kind` is immutable and any `kind`
/// in the body is ignored server-side (`~/vividengine/llms/ai.txt:499`).
pub async fn update_chat(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    chat_id: &str,
    name: &str,
) -> Result<Value, CliError> {
    let path = agent_chat_path(context_type, context_id, chat_id, "update/")?;
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    client.post(&path, &form).await
}

/// Publish a private chat (make it public; one-way).
///
/// `POST /{context_type}/{context_id}/ai/agent/{chat_id}/publish/`. The body
/// is empty — an empty form is sent.
pub async fn publish_chat(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    chat_id: &str,
) -> Result<Value, CliError> {
    let path = agent_chat_path(context_type, context_id, chat_id, "publish/")?;
    let form: HashMap<String, String> = HashMap::new();
    client.post(&path, &form).await
}

/// Soft-delete a chat.
///
/// `DELETE /{context_type}/{context_id}/ai/agent/{chat_id}/`. Deleted chats
/// can still be listed via the `/deleted` list variant.
pub async fn delete_chat(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    chat_id: &str,
) -> Result<Value, CliError> {
    let path = agent_chat_path(context_type, context_id, chat_id, "")?;
    client.delete(&path).await
}

/// Build a `/{context_type}/{context_id}/ai/agent/{chat_id}/{suffix}` path.
///
/// `context_type` is whitelisted to `workspace`/`share` so a typo cannot
/// mis-route to an unintended endpoint; the IDs are trimmed and rejected if
/// empty, then URL-encoded. `suffix` is appended verbatim (already a trusted
/// literal at every call site, e.g. `details/`, `update/`, `publish/`, or
/// `""` for the bare chat path).
fn agent_chat_path(
    context_type: &str,
    context_id: &str,
    chat_id: &str,
    suffix: &str,
) -> Result<String, CliError> {
    if !matches!(context_type, "workspace" | "share") {
        return Err(CliError::Parse(format!(
            "context_type must be \"workspace\" or \"share\", got {context_type:?}",
        )));
    }
    let context_id = context_id.trim();
    let chat_id = chat_id.trim();
    if context_id.is_empty() {
        return Err(CliError::Parse("context_id must not be empty".to_owned()));
    }
    if chat_id.is_empty() {
        return Err(CliError::Parse("chat_id must not be empty".to_owned()));
    }
    Ok(format!(
        "/{}/{}/ai/agent/{}/{suffix}",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(chat_id),
    ))
}

// Semantic search over indexed workspace files formerly lived here as
// `search` (`GET /workspace/{id}/ai/search/`). Phase 3 retired it: the
// deprecated `/ai/search/` endpoint and the duplicate builder are gone, and
// `ripley search` / the MCP search action now forward to the single
// `api::storage::search_files` builder (`/storage/search/`), which performs
// semantic search automatically when workspace intelligence is enabled. There
// is intentionally NO second search builder in this module.

/// Generate a shareable AI summary from specific workspace files.
///
/// `POST /workspace/{workspace_id}/ai/share/`
///
/// Requires 1-25 file opaque IDs. The endpoint reads **form-encoded** input
/// and the `files` field must be a **JSON-encoded array string** of node
/// opaque IDs, e.g. `files=["id1","id2"]` (NOT a comma-separated `nodes`
/// CSV — see `~/vividengine/llms/ai.txt:890-904`). The API generates
/// AI-powered markdown with temporary download URLs that can be pasted into
/// an external chatbot.
pub async fn summarize(
    client: &ApiClient,
    workspace_id: &str,
    file_ids: &[String],
) -> Result<Value, CliError> {
    let path = format!("/workspace/{}/ai/share/", urlencoding::encode(workspace_id),);
    client.post(&path, &build_share_form(file_ids)).await
}

/// Build the form body for the `ai/share/` (share-generate) endpoint:
/// a single `files` field whose value is a JSON-array string of node IDs.
///
/// Shared by the CLI `summarize` builder and the MCP `share-generate`
/// handler so both contexts (workspace + share) emit identical, correct
/// bodies. `serde_json::to_string` of a `Vec<String>` never fails, so the
/// empty/fallback arm exists only to satisfy the type.
///
/// This builder does NOT enforce the API's 1-25 `files` bound
/// (`~/vividengine/llms/ai.txt:894`) — callers are responsible for the
/// client-side length check before calling this; both the CLI `summary`
/// handler and the MCP `share-generate` handler reject `> 25` (and empty)
/// before the network round-trip.
#[must_use]
pub fn build_share_form(file_ids: &[String]) -> HashMap<String, String> {
    let mut form = HashMap::new();
    let json = serde_json::to_string(file_ids).unwrap_or_else(|_| "[]".to_owned());
    form.insert("files".to_owned(), json);
    form
}

/// Generic AI API call that supports both workspace and share context.
///
/// Routes to `/{context_type}/{context_id}/ai/{sub_path}`. POST bodies sent
/// through this helper are **JSON**; the `/ai/agent/` create/send and
/// `ai/share/` endpoints require form encoding, so those callers must use
/// [`ai_api_form`] instead. This helper remains for the GET/DELETE actions
/// (list/details/messages/delete) and the genuinely-JSON-free POSTs
/// (publish/autotitle) where the server tolerates an empty/JSON body.
#[allow(clippy::implicit_hasher)]
pub async fn ai_api(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    sub_path: &str,
    method: &str,
    body: Option<&Value>,
    params: Option<&HashMap<String, String>>,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/ai/{}",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        sub_path,
    );
    match method {
        "POST" => {
            if let Some(b) = body {
                client.post_json(&path, b).await
            } else {
                client.post_json(&path, &serde_json::json!({})).await
            }
        }
        "DELETE" => client.delete(&path).await,
        _ => {
            if let Some(p) = params {
                client.get_with_params(&path, p).await
            } else {
                client.get(&path).await
            }
        }
    }
}

/// Form-encoded POST variant of [`ai_api`] for the workspace/share-context
/// `/ai/agent/` create + follow-up message endpoints and the `ai/share/`
/// share-generate endpoint, all of which read
/// `application/x-www-form-urlencoded` bodies (`~/vividengine/llms/ai.txt`).
#[allow(clippy::implicit_hasher)]
pub async fn ai_api_form(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    sub_path: &str,
    form: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/ai/{}",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        sub_path,
    );
    client.post(&path, form).await
}

/// Per-call-site `error.code` values the agent send/create endpoints return
/// when the conversation exceeds the size cap (`Result::STATE_TOO_LARGE` →
/// `APP_CONFLICT` → HTTP 409): workspace continue-message, workspace
/// new-thread, share continue-message, share new-thread. All four are HTTP 409
/// (source: the Ripley SSE wire contract, §4). Shared by the CLI
/// `map_ai_send_error` and the MCP `ai_send_err_to_result` so the two
/// error-mapping paths cannot drift out of sync.
pub const CONVERSATION_TOO_LARGE_CODES: [u32; 4] = [168_116, 153_795, 148_135, 144_657];

/// Return the message-detail object inside a message-details body, handling
/// BOTH the workspace `message` wrapper and the share `turn` wrapper.
///
/// A workspace ask returns `{message: {state, text, response, actions, …}}`
/// while a SHARE ask returns `{turn: {state, …}}`
/// (`~/vividengine/llms/ai.txt:771`: "The message detail is returned under a
/// `message` object (a `turn` object on the share endpoint)."). When neither
/// wrapper is present (an already-unwrapped or bare detail) the top-level body
/// is returned unchanged. Centralizing the wrapper rule here keeps the CLI
/// `ask`/render and MCP `ask` paths from each re-deriving — and drifting on —
/// it; without it a share `needs_input` turn's `state` is never read (no
/// `message` key), so the wait loop polls to a misleading timeout and the
/// clarification is missed.
#[must_use]
pub fn message_detail(msg_data: &Value) -> &Value {
    msg_data
        .get("message")
        .or_else(|| msg_data.get("turn"))
        .unwrap_or(msg_data)
}

/// Is an agent message/turn `state` terminal (the poll/wait loop should stop)?
///
/// Terminal states are `complete`, `errored`, and `needs_input`. The first two
/// are the documented message-detail states (`~/vividengine/llms/ai.txt:750`);
/// `needs_input` is the additional terminal state a turn reaches when the
/// assistant answers with a clarifying question instead of a full response
/// (`ai.txt:849` — it is terminal, NOT `errored`, and the stream/poll closes).
/// Without `needs_input` here the `ask`/`chat` wait loops would poll such a turn
/// until the wait budget elapses and surface a misleading timeout.
#[must_use]
pub fn is_terminal_state(state: &str) -> bool {
    matches!(state, "complete" | "errored" | "needs_input")
}

/// Extract the clarifying question from a `needs_input` agent message-details
/// body, if present.
///
/// A `needs_input` turn (`~/vividengine/llms/ai.txt:849`) carries a single
/// clarifying question instead of a full answer. The question lives in a
/// `clarification` object (`{type, question}`), present at one of several
/// locations checked in priority order:
/// 1. the envelope-unwrapped top level (`clarification`),
/// 2. the message-detail object — `message` (workspace) or `turn` (share), via
///    [`message_detail`] — at `.clarification`,
/// 3. the service-level `result.clarification` REST-detail recovery path
///    (`vividengine.ripley-sse-contract.md` §2 — distinct from the
///    `{result:"yes"|"no"}` envelope the client already unwraps),
/// 4. the detail's `response.clarification`,
/// 5. a bare `question` field on the turn (top level or nested) — the share
///    `turn` may carry the clarifying question this way.
///
/// Returns the first non-empty `question` string, or `None` when none is
/// present.
///
/// Note: `message.text` is deliberately NOT consulted — that is the user's
/// ORIGINAL question, not the assistant's clarifying question.
#[must_use]
pub fn extract_clarification_question(msg_data: &Value) -> Option<String> {
    /// Pull a non-empty `clarification.question` string off a JSON object.
    fn clarification_q(v: &Value) -> Option<&str> {
        v.get("clarification")
            .and_then(|c| c.get("question"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }
    /// Pull a non-empty bare `question` string off a JSON object.
    fn bare_q(v: &Value) -> Option<&str> {
        v.get("question")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }

    let msg = message_detail(msg_data);
    clarification_q(msg_data)
        .or_else(|| clarification_q(msg))
        // Service-level `result.clarification` (REST-detail recovery), distinct
        // from the already-unwrapped `{result:"yes"|"no"}` transport envelope.
        .or_else(|| msg_data.get("result").and_then(clarification_q))
        .or_else(|| msg.get("response").and_then(clarification_q))
        // Fallback: a bare `question` field on the turn (top level or nested).
        .or_else(|| bare_q(msg_data))
        .or_else(|| bare_q(msg))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        CONVERSATION_TOO_LARGE_CODES, ChatCreateOptions, ChatScope, agent_chat_path,
        build_cancel_path, build_references, build_share_form, build_subjects,
        extract_clarification_question, is_terminal_state, message_detail, parse_cancel_response,
    };
    use crate::error::CliError;
    use serde_json::{Value, json};

    #[test]
    fn is_terminal_state_includes_needs_input() {
        // `needs_input` (ai.txt:849) is terminal alongside complete/errored —
        // without it the wait loops would poll a clarifying-question turn to a
        // misleading timeout.
        for s in ["complete", "errored", "needs_input"] {
            assert!(is_terminal_state(s), "{s} must be terminal");
        }
        for s in ["ready", "in_progress", "processing", ""] {
            assert!(!is_terminal_state(s), "{s} must NOT be terminal");
        }
    }

    #[test]
    fn clarification_from_top_level_object() {
        // The contract's `result.clarification = {type, question}` lands at the
        // envelope-unwrapped top level.
        let body = json!({
            "message": {"state": "needs_input"},
            "clarification": {"type": "clarification", "question": "Which workspace?"},
        });
        assert_eq!(
            extract_clarification_question(&body).as_deref(),
            Some("Which workspace?")
        );
    }

    #[test]
    fn clarification_from_nested_message_object() {
        // A share `turn`/`message`-nested clarification is also found.
        let body = json!({
            "message": {
                "state": "needs_input",
                "clarification": {"question": "Which file version?"},
            },
        });
        assert_eq!(
            extract_clarification_question(&body).as_deref(),
            Some("Which file version?")
        );
    }

    #[test]
    fn clarification_bare_question_fallback() {
        // The turn may carry a bare `question` field (no clarification object).
        let body = json!({"message": {"state": "needs_input"}, "question": "Clarify scope?"});
        assert_eq!(
            extract_clarification_question(&body).as_deref(),
            Some("Clarify scope?")
        );
    }

    #[test]
    fn clarification_ignores_user_question_text_and_returns_none() {
        // `message.text` is the USER's original question, never the clarifying
        // question — it must NOT be surfaced as a clarification.
        let body = json!({
            "message": {"state": "needs_input", "text": "What were Q3 figures?"},
        });
        assert!(extract_clarification_question(&body).is_none());
    }

    #[test]
    fn message_detail_unwraps_message_turn_and_bare() {
        // Workspace detail → `message`; share detail → `turn`; an already
        // unwrapped/bare detail falls back to the top-level body itself.
        let ws = json!({"message": {"state": "complete", "text": "ws"}});
        assert_eq!(
            message_detail(&ws),
            &json!({"state": "complete", "text": "ws"})
        );

        let share = json!({"turn": {"state": "needs_input"}});
        assert_eq!(message_detail(&share), &json!({"state": "needs_input"}));

        // `message` wins over a stray `turn` if (improbably) both are present.
        let both = json!({"message": {"state": "complete"}, "turn": {"state": "x"}});
        assert_eq!(message_detail(&both), &json!({"state": "complete"}));

        let bare = json!({"state": "errored"});
        assert_eq!(message_detail(&bare), &bare);
    }

    #[test]
    fn share_turn_needs_input_is_detected_terminal_via_helpers() {
        // The exact combination the CLI + MCP wait loops compute: unwrap the
        // share `turn` wrapper, then classify its `state`. A share
        // `{turn:{state:"needs_input"}}` must be TERMINAL — without the
        // `message_detail` unwrap, `state` reads empty and the loop polls to a
        // misleading timeout (the bug this fix closes).
        let body = json!({"turn": {"state": "needs_input"}});
        let detail = message_detail(&body);
        let state = detail
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert_eq!(state, "needs_input");
        assert!(
            is_terminal_state(state),
            "a share needs_input turn must terminate the wait loop"
        );
    }

    #[test]
    fn clarification_from_share_turn_wrapper() {
        // SHARE asks return the detail under a `turn` object (ai.txt:771); the
        // clarification nested there must be found just like the `message` case.
        let body = json!({
            "turn": {
                "state": "needs_input",
                "clarification": {"type": "clarification", "question": "Which share folder?"},
            },
        });
        assert_eq!(
            extract_clarification_question(&body).as_deref(),
            Some("Which share folder?")
        );
    }

    #[test]
    fn clarification_from_share_turn_bare_question() {
        // A share `turn` may carry a bare `question` rather than a clarification
        // object — still surfaced.
        let body = json!({"turn": {"state": "needs_input", "question": "Clarify the scope?"}});
        assert_eq!(
            extract_clarification_question(&body).as_deref(),
            Some("Clarify the scope?")
        );
    }

    #[test]
    fn clarification_from_service_level_result_object() {
        // REST-detail recovery path: the service-level `result.clarification`
        // (ripley-sse-contract §2), distinct from the `{result:"yes"|"no"}`
        // transport envelope the client already unwraps.
        let body = json!({
            "turn": {"state": "needs_input"},
            "result": {"clarification": {"type": "clarification", "question": "Which document?"}},
        });
        assert_eq!(
            extract_clarification_question(&body).as_deref(),
            Some("Which document?")
        );
    }

    #[test]
    fn clarification_empty_question_is_skipped() {
        // An empty `question` string is not a usable clarification — fall
        // through to the next location (here, the bare `question`).
        let body = json!({
            "message": {
                "state": "needs_input",
                "clarification": {"question": ""},
                "question": "Real follow-up?",
            },
        });
        assert_eq!(
            extract_clarification_question(&body).as_deref(),
            Some("Real follow-up?")
        );
    }

    #[test]
    fn conversation_too_large_codes_are_the_four_call_sites() {
        // The shared const is the single source of truth for both the CLI and
        // MCP send-error mappers (ripley-sse-contract §4).
        assert_eq!(
            CONVERSATION_TOO_LARGE_CODES,
            [168_116, 153_795, 148_135, 144_657]
        );
    }

    #[test]
    fn agent_chat_path_builds_details_with_trailing_slash() {
        let p = agent_chat_path("workspace", "WS1", "C1", "details/").expect("valid");
        assert_eq!(p, "/workspace/WS1/ai/agent/C1/details/");
    }

    #[test]
    fn agent_chat_path_bare_delete_path() {
        // `delete_chat` passes an empty suffix → bare `/ai/agent/{id}/`.
        let p = agent_chat_path("share", "S1", "C1", "").expect("valid");
        assert_eq!(p, "/share/S1/ai/agent/C1/");
    }

    #[test]
    fn agent_chat_path_rejects_bad_context_type() {
        let err = agent_chat_path("org", "O1", "C1", "details/").expect_err("must reject");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn agent_chat_path_rejects_empty_ids() {
        assert!(agent_chat_path("workspace", "  ", "C1", "update/").is_err());
        assert!(agent_chat_path("workspace", "WS1", "", "update/").is_err());
    }

    #[test]
    fn agent_chat_path_url_encodes_segments() {
        let p = agent_chat_path("workspace", "ws id", "c/2", "publish/").expect("valid");
        assert_eq!(p, "/workspace/ws%20id/ai/agent/c%2F2/publish/");
    }

    #[test]
    fn cancel_path_workspace_scope_uses_agent_with_trailing_slash() {
        // Migrated path: /ai/chat/ -> /ai/agent/, AND a trailing slash on
        // cancel per ai.txt:617.
        let p = build_cancel_path("workspace", "4687730903718774523", "AIC_abc123");
        assert_eq!(
            p,
            "/workspace/4687730903718774523/ai/agent/AIC_abc123/cancel/"
        );
    }

    #[test]
    fn cancel_path_share_scope_uses_agent_with_trailing_slash() {
        let p = build_cancel_path("share", "S_xyz", "AIC_abc123");
        assert_eq!(p, "/share/S_xyz/ai/agent/AIC_abc123/cancel/");
    }

    #[test]
    fn cancel_path_url_encodes_segments() {
        // Chat IDs are opaque strings — defensively encode any caller-supplied
        // value so a stray reserved character can't break the path or smuggle
        // extra segments past the router.
        let p = build_cancel_path("workspace", "ws id", "chat/with slash");
        assert_eq!(p, "/workspace/ws%20id/ai/agent/chat%2Fwith%20slash/cancel/");
    }

    #[test]
    fn cancel_path_url_encodes_bidi_and_control_chars() {
        // RLO (U+202E) and newline must encode rather than land in the path
        // verbatim — Trojan-Source / smuggling defense.
        let p = build_cancel_path("workspace", "ws", "chat\u{202E}\nrest");
        assert_eq!(p, "/workspace/ws/ai/agent/chat%E2%80%AE%0Arest/cancel/");
    }

    #[test]
    fn create_chat_form_carries_references_not_legacy_scope_or_type() {
        // On the migrated /ai/agent/ contract the form carries thread-level
        // fields (`question`/`privacy`/`name`/`kind`) plus the SINGLE structured
        // `references` field — NOT the retired `type`/`personality` or the flat
        // `files_scope`/`folders_scope`/`files_attach` form fields, and not the
        // older `nodes`/`folder_id`/`intelligence` fields.
        let mut form = std::collections::HashMap::new();
        form.insert("question".to_owned(), "hi".to_owned());
        let opts = ChatCreateOptions {
            privacy: Some("private".to_owned()),
            name: Some("My chat".to_owned()),
            personality: Some("detailed".to_owned()),
            kind: Some("user".to_owned()),
            scope: ChatScope {
                files_scope: Some("n1:".to_owned()),
                folders_scope: Some("f1:10".to_owned()),
                files_attach: None,
            },
        };
        super::apply_scope(&mut form, &opts.scope);
        if let Some(p) = &opts.privacy {
            form.insert("privacy".to_owned(), p.clone());
        }
        if let Some(n) = &opts.name {
            form.insert("name".to_owned(), n.clone());
        }
        if let Some(k) = &opts.kind {
            form.insert("kind".to_owned(), k.clone());
        }
        // Thread-level + references fields present.
        for key in ["question", "privacy", "name", "kind", "references"] {
            assert!(form.contains_key(key), "missing {key}");
        }
        // Retired thread fields AND the flat scope fields must be absent.
        for key in [
            "type",
            "personality",
            "files_scope",
            "folders_scope",
            "files_attach",
            "nodes",
            "folder_id",
            "intelligence",
        ] {
            assert!(!form.contains_key(key), "should not emit {key}");
        }
        // The references array collapses `n1:` (empty version → "") into a file
        // item and `f1:10` (depth dropped) into a folder item.
        let refs: Value = serde_json::from_str(form.get("references").expect("references present"))
            .expect("json");
        assert_eq!(
            refs,
            json!([
                {"type": "file", "id": "n1", "file_details": {"node_id": "n1", "version_id": ""}},
                {"type": "folder", "id": "f1", "folder_details": {"node_id": "f1"}},
            ])
        );
    }

    #[test]
    fn build_references_file_with_version() {
        let scope = ChatScope {
            files_scope: Some("n1:v9".to_owned()),
            ..ChatScope::default()
        };
        let refs: Value =
            serde_json::from_str(&build_references(&scope).expect("some")).expect("json");
        assert_eq!(
            refs,
            json!([
                {"type": "file", "id": "n1", "file_details": {"node_id": "n1", "version_id": "v9"}},
            ])
        );
    }

    #[test]
    fn build_references_file_empty_version_becomes_empty_string() {
        // Both a trailing-colon (`abc:`) and a bare id (`abc`) yield `""`, which
        // the backend auto-resolves to the current version.
        for input in ["abc:", "abc"] {
            let scope = ChatScope {
                files_scope: Some(input.to_owned()),
                ..ChatScope::default()
            };
            let refs: Value =
                serde_json::from_str(&build_references(&scope).expect("some")).expect("json");
            assert_eq!(
                refs,
                json!([
                    {"type": "file", "id": "abc", "file_details": {"node_id": "abc", "version_id": ""}},
                ]),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn build_references_folder_drops_depth() {
        let scope = ChatScope {
            folders_scope: Some("f1:7".to_owned()),
            ..ChatScope::default()
        };
        let refs: Value =
            serde_json::from_str(&build_references(&scope).expect("some")).expect("json");
        // No depth field on the folder item.
        assert_eq!(
            refs,
            json!([{"type": "folder", "id": "f1", "folder_details": {"node_id": "f1"}}])
        );
    }

    #[test]
    fn build_references_and_subjects_are_separate_arrays() {
        // files_scope → references; files_attach → subjects. They are DISTINCT
        // arrays (no cross-merge/dedup), so a node named in both appears in each
        // with its own version.
        let scope = ChatScope {
            files_scope: Some("n1:v1, n2:v2".to_owned()),
            files_attach: Some("n1:vX, n3".to_owned()),
            ..ChatScope::default()
        };
        let refs: Value =
            serde_json::from_str(&build_references(&scope).expect("some")).expect("json");
        assert_eq!(
            refs,
            json!([
                {"type": "file", "id": "n1", "file_details": {"node_id": "n1", "version_id": "v1"}},
                {"type": "file", "id": "n2", "file_details": {"node_id": "n2", "version_id": "v2"}},
            ]),
            "references carries files_scope only"
        );
        let subs: Value =
            serde_json::from_str(&build_subjects(&scope).expect("some")).expect("json");
        assert_eq!(
            subs,
            json!([
                {"type": "file", "id": "n1", "file_details": {"node_id": "n1", "version_id": "vX"}},
                {"type": "file", "id": "n3", "file_details": {"node_id": "n3", "version_id": ""}},
            ]),
            "subjects carries files_attach only (n1 keeps its own vX — no cross-dedup)"
        );
    }

    #[test]
    fn build_references_empty_is_none() {
        // No scope at all, and all-whitespace/empty entries, both yield None so
        // the caller omits the `references` field entirely.
        assert!(build_references(&ChatScope::default()).is_none());
        let scope = ChatScope {
            files_scope: Some("  , ,".to_owned()),
            folders_scope: Some(String::new()),
            files_attach: Some("   ".to_owned()),
        };
        assert!(build_references(&scope).is_none());
    }

    #[test]
    fn build_references_files_and_folders_subjects_separate() {
        // files_scope + folders_scope → references; files_attach → subjects.
        let scope = ChatScope {
            files_scope: Some("f1:v1".to_owned()),
            folders_scope: Some("d1:3,d2".to_owned()),
            files_attach: Some("f2".to_owned()),
        };
        let refs: Value =
            serde_json::from_str(&build_references(&scope).expect("some")).expect("json");
        assert_eq!(
            refs,
            json!([
                {"type": "file", "id": "f1", "file_details": {"node_id": "f1", "version_id": "v1"}},
                {"type": "folder", "id": "d1", "folder_details": {"node_id": "d1"}},
                {"type": "folder", "id": "d2", "folder_details": {"node_id": "d2"}},
            ])
        );
        let subs: Value =
            serde_json::from_str(&build_subjects(&scope).expect("some")).expect("json");
        assert_eq!(
            subs,
            json!([{"type": "file", "id": "f2", "file_details": {"node_id": "f2", "version_id": ""}}])
        );
    }

    #[test]
    fn build_subjects_only_from_files_attach() {
        // subjects come ONLY from files_attach; files_scope/folders_scope do not
        // feed subjects, and no files_attach → None.
        assert!(build_subjects(&ChatScope::default()).is_none());
        let scope_only_scope = ChatScope {
            files_scope: Some("n1:v1".to_owned()),
            folders_scope: Some("d1".to_owned()),
            files_attach: None,
        };
        assert!(build_subjects(&scope_only_scope).is_none());
        let scope_attach = ChatScope {
            files_attach: Some("a1:v1".to_owned()),
            ..ChatScope::default()
        };
        let subs: Value =
            serde_json::from_str(&build_subjects(&scope_attach).expect("some")).expect("json");
        assert_eq!(
            subs,
            json!([{"type": "file", "id": "a1", "file_details": {"node_id": "a1", "version_id": "v1"}}])
        );
    }

    #[test]
    fn share_form_sends_files_json_array_not_nodes_csv() {
        let ids = vec!["aBc".to_owned(), "dEf".to_owned()];
        let form = build_share_form(&ids);
        assert_eq!(
            form.get("files").map(String::as_str),
            Some(r#"["aBc","dEf"]"#)
        );
        // Must NOT use the retired `nodes` CSV field.
        assert!(!form.contains_key("nodes"));
    }

    #[test]
    fn share_form_single_file_is_valid_json_array() {
        let form = build_share_form(&["only".to_owned()]);
        assert_eq!(form.get("files").map(String::as_str), Some(r#"["only"]"#));
    }

    #[test]
    fn cancel_response_pending_message_returned_verbatim() {
        let body = json!({"success": true, "message": {"id": "AIJ_abc"}});
        let parsed = parse_cancel_response(body.clone()).expect("should parse");
        assert_eq!(parsed, body);
    }

    #[test]
    fn cancel_response_no_pending_message_returned_verbatim() {
        // The most important test: this body has NO `result` field. The
        // generic `handle_response` envelope-unwrap would reject it; the
        // raw path + `parse_cancel_response` must return it as success.
        let body = json!({"success": true, "no_pending_message": true});
        let parsed = parse_cancel_response(body.clone()).expect("should parse");
        assert_eq!(parsed, body);
    }

    #[test]
    fn cancel_response_flat_error_envelope_surfaces_server_message() {
        let body = json!({
            "result": false,
            "error_message": "Chat not found",
            "error_id": 12_345,
        });
        let err = parse_cancel_response(body).expect_err("should be Err");
        match err {
            CliError::Api(api) => {
                assert_eq!(api.message, "Chat not found");
                assert_eq!(api.code, 12_345);
                assert_eq!(api.http_status, 406);
            }
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[test]
    fn cancel_response_error_with_string_id_is_parsed() {
        // Forward-compat: accept numeric strings for `error_id`.
        let body = json!({
            "result": false,
            "error_message": "permission denied",
            "error_id": "67890",
        });
        let err = parse_cancel_response(body).expect_err("should be Err");
        match err {
            CliError::Api(api) => {
                assert_eq!(api.code, 67_890);
                assert_eq!(api.message, "permission denied");
            }
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[test]
    fn cancel_response_error_without_message_falls_back_to_default() {
        let body = json!({"result": false});
        let err = parse_cancel_response(body).expect_err("should be Err");
        match err {
            CliError::Api(api) => {
                assert_eq!(api.message, "AI chat cancel rejected by server");
                assert_eq!(api.code, 0);
            }
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[test]
    fn cancel_response_recognizes_string_no_as_failure() {
        // Defense-in-depth: if a future server-side normalization sends
        // the standard envelope's `result: "no"` instead of `false`, the
        // 2xx-with-error guard must still classify it as a failure.
        let body = json!({"result": "no", "error_message": "rejected"});
        let err = parse_cancel_response(body).expect_err("should be Err");
        match err {
            CliError::Api(api) => assert_eq!(api.message, "rejected"),
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[test]
    fn cancel_response_recognizes_numeric_zero_as_failure() {
        let body = json!({"result": 0, "error_message": "rejected"});
        let err = parse_cancel_response(body).expect_err("should be Err");
        match err {
            CliError::Api(api) => assert_eq!(api.message, "rejected"),
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[test]
    fn cancel_response_string_yes_envelope_passes_through() {
        // If a future server normalization wraps the body as the standard
        // `{"result": "yes", ...}` envelope, the 2xx-with-error guard
        // must NOT treat it as failure — the renderer can still pull out
        // `success`/`message`/`no_pending_message`.
        let body = json!({
            "result": "yes",
            "success": true,
            "no_pending_message": true,
        });
        let parsed = parse_cancel_response(body.clone()).expect("should parse");
        assert_eq!(parsed, body);
    }
}
