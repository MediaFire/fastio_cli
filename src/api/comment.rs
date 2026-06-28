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
    /// Sort order for results: `asc` or `desc` (server default `asc`). Forwarded
    /// verbatim as the `sort` query param.
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

/// Parameters for [`add_comment`].
///
/// `body` (and optionally `parent_id`) are the only fields a plain comment or
/// reply needs; the remaining fields carry the optional create extensions
/// documented in comments.txt:
///
/// - `reference` — an anchoring reference object (see Reference Anchoring).
/// - `properties` — arbitrary key-value metadata.
/// - `target_id` / `target_ids` — inline attachment(s) on a **new** comment
///   (single or batch, ≤25). The server ignores these on an update; the
///   command/MCP layer only sends them on create.
pub struct AddCommentParams<'a> {
    /// Kind of parent entity (`workspace` or `share`).
    pub entity_type: &'a str,
    /// Unique identifier of the parent workspace or share.
    pub entity_id: &'a str,
    /// File/folder node the comment is anchored to.
    pub node_id: &'a str,
    /// Comment text content.
    pub body: &'a str,
    /// Parent comment ID for a single-level threaded reply.
    pub parent_id: Option<&'a str>,
    /// Optional anchoring reference (JSON object) into a file position.
    pub reference: Option<&'a Value>,
    /// Optional arbitrary key-value metadata (JSON object).
    pub properties: Option<&'a Value>,
    /// Inline single attachment (new comment only).
    pub target_id: Option<&'a str>,
    /// Inline multiple attachments (new comment only, ≤25).
    pub target_ids: Option<&'a [String]>,
}

/// Build the request body for [`add_comment`].
///
/// Extracted as a pure function so the body construction is testable without a
/// network round-trip. Only set fields are emitted, matching the server's
/// "omit to leave unset" convention.
fn build_add_comment_body(params: &AddCommentParams<'_>) -> Value {
    let mut body = serde_json::json!({ "body": params.body });
    if let Some(parent_id) = params.parent_id {
        body["parent_id"] = Value::String(parent_id.to_owned());
    }
    if let Some(reference) = params.reference {
        body["reference"] = reference.clone();
    }
    if let Some(properties) = params.properties {
        body["properties"] = properties.clone();
    }
    if let Some(target_id) = params.target_id {
        body["target_id"] = Value::String(target_id.to_owned());
    }
    if let Some(target_ids) = params.target_ids {
        body["target_ids"] = serde_json::json!(target_ids);
    }
    body
}

/// Add a comment to a file (or reply to one via `parent_id`).
///
/// `POST /comments/{entity_type}/{entity_id}/{node_id}/`
pub async fn add_comment(
    client: &ApiClient,
    params: &AddCommentParams<'_>,
) -> Result<Value, CliError> {
    let body = build_add_comment_body(params);
    let path = format!(
        "/comments/{}/{}/{}/",
        urlencoding::encode(params.entity_type),
        urlencoding::encode(params.entity_id),
        urlencoding::encode(params.node_id),
    );
    client.post_json(&path, &body).await
}

/// Edit an existing comment by ID.
///
/// `POST /comments/{comment_id}/update/`
/// Author-only. Works for every comment surface — workspace, share, node,
/// File Share, and task comments (the entity-scoped create/update route cannot
/// address a task comment). The edit cannot move the comment; entity, scope,
/// threading, and any workflow link are immutable.
pub async fn update_comment(
    client: &ApiClient,
    comment_id: &str,
    text: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "body": text });
    let path = format!("/comments/{}/update/", urlencoding::encode(comment_id));
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
/// `POST /comments/{comment_id}/link/`. The CLI offers the linked entity types
/// `task` and `workflow_review` (the legacy `approval` type is deprecated and no
/// longer offered by the CLI, though the server still accepts it for
/// back-compat); `entity_type` is forwarded verbatim.
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
/// `GET /comments/linked/{entity_type}/{entity_id}/` (optional `limit`/`offset`).
/// The CLI offers the linked entity types `task` and `workflow_review` (the
/// legacy `approval` type is deprecated and no longer offered by the CLI, though
/// the server still accepts it for back-compat).
pub async fn linked_comments(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
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
        "/comments/linked/{}/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

// ─── Comment Attachments ──────────────────────────────────────────────────────

/// Selector for which object(s) to attach to a comment.
///
/// Mirrors the server's `target_id` (single) / `target_ids` (batch, ≤25) body
/// fields on `POST /comments/{comment_id}/attachments/`. The mutual exclusion
/// (and the "at least one" requirement) is enforced by the CLI/MCP layer.
pub enum CommentAttachTargets<'a> {
    /// Attach a single object (`target_id`).
    Single(&'a str),
    /// Attach multiple objects (`target_ids`, ≤25 per comment).
    Multiple(&'a [String]),
}

/// Build the request body for [`attach_comment`].
///
/// Extracted as a pure function so the body construction is testable without a
/// network round-trip.
fn build_attach_body(targets: &CommentAttachTargets<'_>) -> Value {
    match targets {
        CommentAttachTargets::Single(id) => serde_json::json!({ "target_id": id }),
        CommentAttachTargets::Multiple(ids) => serde_json::json!({ "target_ids": ids }),
    }
}

/// List the objects attached to a comment (hydrated, access-gated).
///
/// `GET /comments/{comment_id}/attachments/`
pub async fn list_comment_attachments(
    client: &ApiClient,
    comment_id: &str,
) -> Result<Value, CliError> {
    let path = format!("/comments/{}/attachments/", urlencoding::encode(comment_id),);
    client.get(&path).await
}

/// Attach one object (`target_id`) or many (`target_ids`) to a comment.
///
/// `POST /comments/{comment_id}/attachments/`. Idempotent (already-attached
/// objects are skipped) and atomic (a partial-batch failure attaches nothing);
/// author-only and rejected on workflow-review comments — the server enforces
/// both. Returns the full updated hydrated attachment list.
pub async fn attach_comment(
    client: &ApiClient,
    comment_id: &str,
    targets: &CommentAttachTargets<'_>,
) -> Result<Value, CliError> {
    let body = build_attach_body(targets);
    let path = format!("/comments/{}/attachments/", urlencoding::encode(comment_id),);
    client.post_json(&path, &body).await
}

/// Build the request body for [`detach_comment`].
fn build_detach_body(target_id: &str) -> Value {
    serde_json::json!({ "target_id": target_id })
}

/// Detach a single object from a comment by its `target_id`.
///
/// `POST /comments/{comment_id}/attachments/detach/`. Idempotent — detaching an
/// object that is not attached returns success with `removed: false`. Returns
/// the updated hydrated attachment list.
pub async fn detach_comment(
    client: &ApiClient,
    comment_id: &str,
    target_id: &str,
) -> Result<Value, CliError> {
    let body = build_detach_body(target_id);
    let path = format!(
        "/comments/{}/attachments/detach/",
        urlencoding::encode(comment_id),
    );
    client.post_json(&path, &body).await
}

#[cfg(test)]
mod tests {
    use super::{
        AddCommentParams, CommentAttachTargets, build_add_comment_body, build_attach_body,
        build_detach_body,
    };
    use serde_json::json;

    #[test]
    fn add_comment_body_minimal_is_body_only() {
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: None,
            reference: None,
            properties: None,
            target_id: None,
            target_ids: None,
        });
        assert_eq!(body, json!({ "body": "hi" }));
    }

    #[test]
    fn add_comment_body_includes_reference_and_properties() {
        let reference = json!({ "type": "page", "page": 3 });
        let properties = json!({ "k": "v" });
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: Some("p1"),
            reference: Some(&reference),
            properties: Some(&properties),
            target_id: None,
            target_ids: None,
        });
        assert_eq!(body["parent_id"], json!("p1"));
        assert_eq!(body["reference"], reference);
        assert_eq!(body["properties"], properties);
    }

    #[test]
    fn add_comment_body_inline_single_target() {
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: None,
            reference: None,
            properties: None,
            target_id: Some("t1"),
            target_ids: None,
        });
        assert_eq!(body["target_id"], json!("t1"));
        assert!(body.get("target_ids").is_none());
    }

    #[test]
    fn add_comment_body_inline_multiple_targets() {
        let ids = vec!["a".to_owned(), "b".to_owned()];
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: None,
            reference: None,
            properties: None,
            target_id: None,
            target_ids: Some(&ids),
        });
        assert_eq!(body["target_ids"], json!(["a", "b"]));
        assert!(body.get("target_id").is_none());
    }

    #[test]
    fn attach_body_single_uses_target_id() {
        let body = build_attach_body(&CommentAttachTargets::Single("t1"));
        assert_eq!(body, json!({ "target_id": "t1" }));
    }

    #[test]
    fn attach_body_multiple_uses_target_ids() {
        let ids = vec!["a".to_owned(), "b".to_owned()];
        let body = build_attach_body(&CommentAttachTargets::Multiple(&ids));
        assert_eq!(body, json!({ "target_ids": ["a", "b"] }));
    }

    #[test]
    fn detach_body_uses_target_id() {
        let body = build_detach_body("t1");
        assert_eq!(body, json!({ "target_id": "t1" }));
    }
}
