#![allow(clippy::missing_errors_doc)]

/// AI API endpoints for the Fast.io REST API.
///
/// Maps to endpoints documented at `/current/workspace/{id}/ai/`.
/// Supports chat creation, message send/read, semantic search, and summarize.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

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
