#![allow(clippy::missing_errors_doc)]

/// AI API endpoints for the Fast.io REST API.
///
/// Maps to endpoints documented at `/current/workspace/{id}/ai/`.
/// Supports chat creation, message send/read, semantic search, and summarize.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::{ApiError, CliError};

/// Create a new AI chat session.
///
/// `POST /workspace/{workspace_id}/ai/chat/`
pub async fn create_chat(
    client: &ApiClient,
    workspace_id: &str,
    question: &str,
    chat_type: &str,
    node_ids: Option<&[String]>,
    folder_id: Option<&str>,
    intelligence: Option<bool>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({
        "type": chat_type,
        "question": question,
        "personality": "detailed",
    });
    if let Some(nodes) = node_ids
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("nodes".to_owned(), serde_json::json!(nodes.join(",")));
    }
    if let Some(fid) = folder_id
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("folder_id".to_owned(), serde_json::json!(fid));
    }
    if let Some(intel) = intelligence
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("intelligence".to_owned(), serde_json::json!(intel));
    }
    let path = format!("/workspace/{}/ai/chat/", urlencoding::encode(workspace_id),);
    client.post_json(&path, &body).await
}

/// Send a message to an existing AI chat.
///
/// `POST /workspace/{workspace_id}/ai/chat/{chat_id}/message/`
pub async fn send_message(
    client: &ApiClient,
    workspace_id: &str,
    chat_id: &str,
    question: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({
        "question": question,
    });
    let path = format!(
        "/workspace/{}/ai/chat/{}/message/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(chat_id),
    );
    client.post_json(&path, &body).await
}

/// Build the path for the AI chat cancel endpoint.
///
/// `/{context_type}/{context_id}/ai/chat/{chat_id}/cancel`
///
/// `context_type` must be either `workspace` or `share`. Per the API
/// contract the path has no trailing slash. Both segment values are
/// URL-encoded; higher-level validation (whitelist on `context_type`,
/// non-empty IDs) is the caller's responsibility — see `cancel_message`.
fn build_cancel_path(context_type: &str, context_id: &str, chat_id: &str) -> String {
    format!(
        "/{}/{}/ai/chat/{}/cancel",
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
    }))
}

/// Cancel an in-progress AI chat message.
///
/// `POST /workspace/{workspace_id}/ai/chat/{chat_id}/cancel`
/// `POST /share/{share_id}/ai/chat/{chat_id}/cancel`
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
    // The cancel endpoint's HTTP-200 success bodies don't carry a
    // `result` field, so the standard envelope-unwrap (`post_json` →
    // `handle_response`) would reject them as errors. Use the raw
    // helper, which returns the body verbatim on 2xx and routes
    // non-2xx through `extract_error` (which now also recognizes the
    // cancel endpoint's flat `error_message` / `error_id` shape).
    let body: Value = client.post_json_raw(&path, &serde_json::json!({})).await?;
    parse_cancel_response(body)
}

/// Get message details (used for polling).
///
/// `GET /workspace/{workspace_id}/ai/chat/{chat_id}/message/{message_id}/details/`
pub async fn get_message_details(
    client: &ApiClient,
    workspace_id: &str,
    chat_id: &str,
    message_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/ai/chat/{}/message/{}/details/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(chat_id),
        urlencoding::encode(message_id),
    );
    client.get(&path).await
}

/// List messages in a chat.
///
/// `GET /workspace/{workspace_id}/ai/chat/{chat_id}/messages/list/`
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
        "/workspace/{}/ai/chat/{}/messages/list/",
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
/// `GET /workspace/{workspace_id}/ai/chat/list/`
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
        "/workspace/{}/ai/chat/list/",
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
/// `GET /workspace/{workspace_id}/ai/search/?question=<query>`
pub async fn search(
    client: &ApiClient,
    workspace_id: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("question".to_owned(), query.to_owned());
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
/// Requires at least one node ID. The API generates an AI-powered summary
/// of the specified files that can be shared via a public link.
pub async fn summarize(
    client: &ApiClient,
    workspace_id: &str,
    node_ids: &[String],
) -> Result<Value, CliError> {
    let nodes_csv = node_ids.join(",");
    let body = serde_json::json!({ "nodes": nodes_csv });
    let path = format!("/workspace/{}/ai/share/", urlencoding::encode(workspace_id),);
    client.post_json(&path, &body).await
}

/// Generic AI API call that supports both workspace and share context.
///
/// Routes to `/{context_type}/{context_id}/ai/{sub_path}`.
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

#[cfg(test)]
mod tests {
    use super::{build_cancel_path, parse_cancel_response};
    use crate::error::CliError;
    use serde_json::json;

    #[test]
    fn cancel_path_workspace_scope_has_no_trailing_slash() {
        let p = build_cancel_path("workspace", "4687730903718774523", "AIC_abc123");
        assert_eq!(
            p,
            "/workspace/4687730903718774523/ai/chat/AIC_abc123/cancel"
        );
    }

    #[test]
    fn cancel_path_share_scope_has_no_trailing_slash() {
        let p = build_cancel_path("share", "S_xyz", "AIC_abc123");
        assert_eq!(p, "/share/S_xyz/ai/chat/AIC_abc123/cancel");
    }

    #[test]
    fn cancel_path_url_encodes_segments() {
        // Chat IDs are opaque strings — defensively encode any caller-supplied
        // value so a stray reserved character can't break the path or smuggle
        // extra segments past the router.
        let p = build_cancel_path("workspace", "ws id", "chat/with slash");
        assert_eq!(p, "/workspace/ws%20id/ai/chat/chat%2Fwith%20slash/cancel");
    }

    #[test]
    fn cancel_path_url_encodes_bidi_and_control_chars() {
        // RLO (U+202E) and newline must encode rather than land in the path
        // verbatim — Trojan-Source / smuggling defense.
        let p = build_cancel_path("workspace", "ws", "chat\u{202E}\nrest");
        assert_eq!(p, "/workspace/ws/ai/chat/chat%E2%80%AE%0Arest/cancel");
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
