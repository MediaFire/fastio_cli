#![allow(clippy::missing_errors_doc)]

/// Comment API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for comment CRUD and reactions on workspace/share files.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Parameters for [`list_comments`].
pub struct ListCommentsParams<'a> {
    /// Kind of parent entity (`workspace` or `share`).
    pub entity_type: &'a str,
    /// Unique identifier of the parent workspace or share.
    pub entity_id: &'a str,
    /// File/folder node within the entity to list comments for.
    pub node_id: &'a str,
    /// Sort order for results (e.g. `newest`, `oldest`).
    pub sort: Option<&'a str>,
    /// Maximum number of comments to return.
    pub limit: Option<u32>,
    /// Number of comments to skip for pagination.
    pub offset: Option<u32>,
}

/// List comments on a specific file.
///
/// `GET /comments/{entity_type}/{entity_id}/{node_id}/`
pub async fn list_comments(
    client: &ApiClient,
    params: &ListCommentsParams<'_>,
) -> Result<Value, CliError> {
    let mut query = HashMap::new();
    if let Some(v) = params.sort {
        query.insert("sort".to_owned(), v.to_owned());
    }
    if let Some(l) = params.limit {
        query.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = params.offset {
        query.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/comments/{}/{}/{}/",
        urlencoding::encode(params.entity_type),
        urlencoding::encode(params.entity_id),
        urlencoding::encode(params.node_id),
    );
    if query.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &query).await
    }
}

/// Add a comment to a file.
///
/// `POST /comments/{entity_type}/{entity_id}/{node_id}/`
pub async fn add_comment(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    node_id: &str,
    text: &str,
    parent_comment_id: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({
        "body": text,
    });
    if let Some(parent_id) = parent_comment_id {
        body["parent_id"] = serde_json::Value::String(parent_id.to_owned());
    }
    let path = format!(
        "/comments/{}/{}/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(node_id),
    );
    client.post_json(&path, &body).await
}

/// Delete a comment.
///
/// `DELETE /comments/{comment_id}/delete/`
pub async fn delete_comment(client: &ApiClient, comment_id: &str) -> Result<Value, CliError> {
    let path = format!("/comments/{}/delete/", urlencoding::encode(comment_id),);
    client.delete(&path).await
}

/// Get comment details.
///
/// `GET /comments/{comment_id}/details/`
pub async fn get_comment_details(client: &ApiClient, comment_id: &str) -> Result<Value, CliError> {
    let path = format!("/comments/{}/details/", urlencoding::encode(comment_id),);
    client.get(&path).await
}

/// List all comments across a workspace or share.
///
/// `GET /comments/{entity_type}/{entity_id}/`
pub async fn list_all_comments(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    sort: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(v) = sort {
        params.insert("sort".to_owned(), v.to_owned());
    }
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/comments/{}/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add an emoji reaction to a comment.
///
/// `POST /comments/{comment_id}/reactions/`
pub async fn add_reaction(
    client: &ApiClient,
    comment_id: &str,
    emoji: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "emoji": emoji });
    let path = format!("/comments/{}/reactions/", urlencoding::encode(comment_id));
    client.post_json(&path, &body).await
}

/// Remove an emoji reaction from a comment.
///
/// `DELETE /comments/{comment_id}/reactions/`
pub async fn remove_reaction(client: &ApiClient, comment_id: &str) -> Result<Value, CliError> {
    let path = format!("/comments/{}/reactions/", urlencoding::encode(comment_id));
    client.delete(&path).await
}

/// Bulk-delete multiple comments.
///
/// `POST /comments/bulk/delete/`
pub async fn bulk_delete_comments(
    client: &ApiClient,
    comment_ids: &[String],
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "comment_ids": comment_ids });
    client.post_json("/comments/bulk/delete/", &body).await
}

/// Link a comment to a workflow entity.
///
/// `POST /comments/{comment_id}/link/`
pub async fn link_comment(
    client: &ApiClient,
    comment_id: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({
        "entity_type": entity_type,
        "entity_id": entity_id,
    });
    let path = format!("/comments/{}/link/", urlencoding::encode(comment_id));
    client.post_json(&path, &body).await
}

/// Unlink a comment from its workflow entity.
///
/// `POST /comments/{comment_id}/unlink/`
pub async fn unlink_comment(client: &ApiClient, comment_id: &str) -> Result<Value, CliError> {
    let path = format!("/comments/{}/unlink/", urlencoding::encode(comment_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// Find comments linked to a workflow entity.
///
/// `GET /comments/linked/{entity_type}/{entity_id}/`
pub async fn linked_comments(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/comments/linked/{}/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    client.get(&path).await
}
