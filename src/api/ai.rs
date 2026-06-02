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
/// All fields are pre-formatted strings matching the `/ai/agent/` form
/// contract (see `~/vividengine/llms/ai.txt:271-273,579-581`):
/// - `files_scope` / `folders_scope` — comma-separated `nodeId:versionId`
///   (files) or `nodeId:depth` (folders) pairs.
/// - `files_attach` — comma-separated file `nodeId:versionId` pairs.
///
/// The shared builder forwards whatever is set verbatim; per-context
/// validation (e.g. share chats rejecting `privacy`) lives at the caller.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct ChatScope {
    /// Comma-separated `nodeId:versionId` file scope pairs (max 100).
    pub files_scope: Option<String>,
    /// Comma-separated `nodeId:depth` folder scope pairs (max 100).
    pub folders_scope: Option<String>,
    /// Comma-separated file `nodeId:versionId` attachment pairs (max 20).
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
    /// `concise` or `detailed` (defaults to `detailed` server-side).
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
/// `/ai/agent/` contract (`~/vividengine/llms/ai.txt:251-283`); the legacy
/// JSON `nodes`/`folder_id`/`intelligence` fields are gone — use
/// `files_scope`/`folders_scope`/`files_attach` instead.
pub async fn create_chat(
    client: &ApiClient,
    workspace_id: &str,
    question: &str,
    chat_type: &str,
    options: &ChatCreateOptions,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("type".to_owned(), chat_type.to_owned());
    form.insert("question".to_owned(), question.to_owned());
    if let Some(p) = &options.privacy {
        form.insert("privacy".to_owned(), p.clone());
    }
    if let Some(n) = &options.name {
        form.insert("name".to_owned(), n.clone());
    }
    if let Some(p) = &options.personality {
        form.insert("personality".to_owned(), p.clone());
    }
    if let Some(k) = &options.kind {
        form.insert("kind".to_owned(), k.clone());
    }
    apply_scope(&mut form, &options.scope);
    let path = format!("/workspace/{}/ai/agent/", urlencoding::encode(workspace_id),);
    client.post(&path, &form).await
}

/// Insert any populated scope/attach fields into a form body.
fn apply_scope(form: &mut HashMap<String, String>, scope: &ChatScope) {
    if let Some(v) = &scope.files_scope {
        form.insert("files_scope".to_owned(), v.clone());
    }
    if let Some(v) = &scope.folders_scope {
        form.insert("folders_scope".to_owned(), v.clone());
    }
    if let Some(v) = &scope.files_attach {
        form.insert("files_attach".to_owned(), v.clone());
    }
}

/// Send a message to an existing AI chat.
///
/// `POST /workspace/{workspace_id}/ai/agent/{chat_id}/message/`. The body is
/// **form-encoded** per the `/ai/agent/` contract
/// (`~/vividengine/llms/ai.txt:563-591`). The chat type is inherited from
/// the chat, so `type` is not resent.
pub async fn send_message(
    client: &ApiClient,
    workspace_id: &str,
    chat_id: &str,
    question: &str,
    personality: Option<&str>,
    scope: &ChatScope,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("question".to_owned(), question.to_owned());
    if let Some(p) = personality {
        form.insert("personality".to_owned(), p.to_owned());
    }
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

/// List all chats in a workspace.
///
/// `GET /workspace/{workspace_id}/ai/agent/list/`
pub async fn list_chats(
    client: &ApiClient,
    workspace_id: &str,
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
        "/workspace/{}/ai/agent/list/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Semantic search over indexed workspace files.
///
/// `GET /workspace/{workspace_id}/ai/search/?query_text=<query>`
///
/// The endpoint's required query parameter is `query_text`
/// (`~/vividengine/llms/ai.txt:1647`), not `question`.
///
/// NOTE: this stays on the **deprecated** `/ai/search/` endpoint for now —
/// it is NOT migrated to `/ai/agent/search/` (no such endpoint). Phase 3
/// re-points it to `/storage/search/`.
pub async fn search(
    client: &ApiClient,
    workspace_id: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("query_text".to_owned(), query.to_owned());
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/ai/search/",
        urlencoding::encode(workspace_id),
    );
    client.get_with_params(&path, &params).await
}

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

#[cfg(test)]
mod tests {
    use super::{
        ChatCreateOptions, ChatScope, build_cancel_path, build_share_form, parse_cancel_response,
    };
    use crate::error::CliError;
    use serde_json::json;

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
    fn create_chat_form_carries_only_documented_fields() {
        // The form body must NOT emit the retired `nodes`/`folder_id`/
        // `intelligence` fields — those are the cause of the live outage.
        let mut form = std::collections::HashMap::new();
        form.insert("type".to_owned(), "chat_with_files".to_owned());
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
        if let Some(p) = &opts.personality {
            form.insert("personality".to_owned(), p.clone());
        }
        if let Some(k) = &opts.kind {
            form.insert("kind".to_owned(), k.clone());
        }
        // Documented field set present.
        for key in [
            "type",
            "question",
            "privacy",
            "name",
            "personality",
            "kind",
            "files_scope",
            "folders_scope",
        ] {
            assert!(form.contains_key(key), "missing {key}");
        }
        // Retired fields absent.
        for key in ["nodes", "folder_id", "intelligence"] {
            assert!(!form.contains_key(key), "should not emit {key}");
        }
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
