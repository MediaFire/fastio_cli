#![allow(clippy::missing_errors_doc)]

/// Tasks API endpoints for the Fast.io REST API.
///
/// Covers task lists, tasks, task comments, and task attachments.
/// All endpoints use JSON request bodies (not form-encoded).
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

// ─── Task Lists ─────────────────────────────────────────────────────────────

/// List task lists in a workspace.
///
/// `GET /workspace/{workspace_id}/tasks/`
pub async fn list_task_lists(
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
    let path = format!("/workspace/{}/tasks/", urlencoding::encode(workspace_id),);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Create a task list.
///
/// `POST /workspace/{workspace_id}/tasks/create/`
pub async fn create_task_list(
    client: &ApiClient,
    workspace_id: &str,
    name: &str,
    description: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "name": name });
    if let Some(d) = description {
        body["description"] = Value::String(d.to_owned());
    }
    let path = format!(
        "/workspace/{}/tasks/create/",
        urlencoding::encode(workspace_id),
    );
    client.post_json(&path, &body).await
}

/// Get task list details.
///
/// `GET /tasks/{list_id}/details/`
#[allow(dead_code)]
pub async fn get_task_list(client: &ApiClient, list_id: &str) -> Result<Value, CliError> {
    let path = format!("/tasks/{}/details/", urlencoding::encode(list_id));
    client.get(&path).await
}

/// Update a task list.
///
/// `POST /tasks/{list_id}/update/`
pub async fn update_task_list(
    client: &ApiClient,
    list_id: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::Map::new();
    if let Some(n) = name {
        body.insert("name".to_owned(), Value::String(n.to_owned()));
    }
    if let Some(d) = description {
        body.insert("description".to_owned(), Value::String(d.to_owned()));
    }
    let path = format!("/tasks/{}/update/", urlencoding::encode(list_id));
    client.post_json(&path, &Value::Object(body)).await
}

/// Delete a task list (soft delete).
///
/// `POST /tasks/{list_id}/delete/`
pub async fn delete_task_list(client: &ApiClient, list_id: &str) -> Result<Value, CliError> {
    let path = format!("/tasks/{}/delete/", urlencoding::encode(list_id));
    client.post_json(&path, &serde_json::json!({})).await
}

// ─── Tasks ──────────────────────────────────────────────────────────────────

/// List tasks in a task list.
///
/// `GET /tasks/{list_id}/items/`
pub async fn list_tasks(
    client: &ApiClient,
    list_id: &str,
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
    let path = format!("/tasks/{}/items/", urlencoding::encode(list_id));
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Parameters for creating a task.
pub struct CreateTaskParams<'a> {
    /// Task list ID.
    pub list_id: &'a str,
    /// Task title.
    pub title: &'a str,
    /// Task description.
    pub description: Option<&'a str>,
    /// Task status.
    pub status: Option<&'a str>,
    /// Priority (0-4).
    pub priority: Option<u8>,
    /// Assignee profile ID.
    pub assignee_id: Option<&'a str>,
    /// Primary linked storage node ID (single-node link on the task).
    pub node_id: Option<&'a str>,
}

/// Create a task in a task list.
///
/// `POST /tasks/{list_id}/items/create/`
pub async fn create_task(
    client: &ApiClient,
    params: &CreateTaskParams<'_>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "title": params.title });
    if let Some(d) = params.description {
        body["description"] = Value::String(d.to_owned());
    }
    if let Some(s) = params.status {
        body["status"] = Value::String(s.to_owned());
    }
    if let Some(p) = params.priority {
        body["priority"] = Value::Number(serde_json::Number::from(p));
    }
    if let Some(a) = params.assignee_id {
        body["assignee_id"] = Value::String(a.to_owned());
    }
    if let Some(n) = params.node_id {
        body["node_id"] = Value::String(n.to_owned());
    }
    let path = format!(
        "/tasks/{}/items/create/",
        urlencoding::encode(params.list_id)
    );
    client.post_json(&path, &body).await
}

/// Get task details.
///
/// `GET /tasks/{list_id}/items/{task_id}/`
pub async fn get_task(client: &ApiClient, list_id: &str, task_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/tasks/{}/items/{}/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.get(&path).await
}

/// Parameters for updating a task.
pub struct UpdateTaskParams<'a> {
    /// Task list ID.
    pub list_id: &'a str,
    /// Task ID.
    pub task_id: &'a str,
    /// New title.
    pub title: Option<&'a str>,
    /// New description.
    pub description: Option<&'a str>,
    /// New status.
    pub status: Option<&'a str>,
    /// New priority.
    pub priority: Option<u8>,
    /// New assignee.
    pub assignee_id: Option<&'a str>,
    /// New primary linked storage node ID (single-node link on the task).
    pub node_id: Option<&'a str>,
}

/// Update a task.
///
/// `POST /tasks/{list_id}/items/{task_id}/update/`
pub async fn update_task(
    client: &ApiClient,
    params: &UpdateTaskParams<'_>,
) -> Result<Value, CliError> {
    let mut body = serde_json::Map::new();
    if let Some(t) = params.title {
        body.insert("title".to_owned(), Value::String(t.to_owned()));
    }
    if let Some(d) = params.description {
        body.insert("description".to_owned(), Value::String(d.to_owned()));
    }
    if let Some(s) = params.status {
        body.insert("status".to_owned(), Value::String(s.to_owned()));
    }
    if let Some(p) = params.priority {
        body.insert(
            "priority".to_owned(),
            Value::Number(serde_json::Number::from(p)),
        );
    }
    if let Some(a) = params.assignee_id {
        body.insert("assignee_id".to_owned(), Value::String(a.to_owned()));
    }
    if let Some(n) = params.node_id {
        // An empty string clears the link (the API accepts `node_id: null`);
        // a non-empty value sets it.
        body.insert(
            "node_id".to_owned(),
            if n.is_empty() {
                Value::Null
            } else {
                Value::String(n.to_owned())
            },
        );
    }
    let path = format!(
        "/tasks/{}/items/{}/update/",
        urlencoding::encode(params.list_id),
        urlencoding::encode(params.task_id),
    );
    client.post_json(&path, &Value::Object(body)).await
}

/// Delete a task (soft delete).
///
/// `POST /tasks/{list_id}/items/{task_id}/delete/`
pub async fn delete_task(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/tasks/{}/items/{}/delete/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Assign a task.
///
/// `POST /tasks/{list_id}/items/{task_id}/assign/`
pub async fn assign_task(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
    assignee_id: Option<&str>,
) -> Result<Value, CliError> {
    let body = match assignee_id {
        Some(id) => serde_json::json!({ "assignee_id": id }),
        None => serde_json::json!({ "assignee_id": null }),
    };
    let path = format!(
        "/tasks/{}/items/{}/assign/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.post_json(&path, &body).await
}

/// Change task status.
///
/// `POST /tasks/{list_id}/items/{task_id}/status/`
pub async fn change_task_status(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
    status: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "status": status });
    let path = format!(
        "/tasks/{}/items/{}/status/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.post_json(&path, &body).await
}

// ─── Task Comments ────────────────────────────────────────────────────────────

/// List a task's comment thread.
///
/// `GET /tasks/{list_id}/items/{task_id}/comments/`
/// Task comments are private to the task — they are not returned by the generic
/// comment-listing endpoints and do not appear in comment search.
pub async fn list_task_comments(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
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
        "/tasks/{}/items/{}/comments/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Parameters for [`post_task_comment`].
pub struct PostTaskCommentParams<'a> {
    /// Task list ID.
    pub list_id: &'a str,
    /// Task ID.
    pub task_id: &'a str,
    /// Comment body (1–8192 chars; may include `@[user:{id}:{name}]` mentions).
    pub body: &'a str,
    /// Parent comment ID for a single-level threaded reply.
    pub parent_id: Option<&'a str>,
    /// Optional anchoring reference (JSON object) into a file position.
    pub reference: Option<&'a Value>,
    /// Optional arbitrary key-value metadata (JSON object).
    pub properties: Option<&'a Value>,
}

/// Post a comment (or threaded reply) on a task.
///
/// `POST /tasks/{list_id}/items/{task_id}/comments/` (JSON body). This endpoint
/// never edits — to edit or delete a task comment use the generic comment
/// endpoints by comment ID (`comment::update_comment` / `comment::delete_comment`).
pub async fn post_task_comment(
    client: &ApiClient,
    params: &PostTaskCommentParams<'_>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "body": params.body });
    if let Some(p) = params.parent_id {
        body["parent_id"] = Value::String(p.to_owned());
    }
    if let Some(r) = params.reference {
        body["reference"] = r.clone();
    }
    if let Some(p) = params.properties {
        body["properties"] = p.clone();
    }
    let path = format!(
        "/tasks/{}/items/{}/comments/",
        urlencoding::encode(params.list_id),
        urlencoding::encode(params.task_id),
    );
    client.post_json(&path, &body).await
}

// ─── Task Attachments ─────────────────────────────────────────────────────────

/// List a task's attachments (hydrated).
///
/// `GET /tasks/{list_id}/items/{task_id}/attachments/`
pub async fn list_task_attachments(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/tasks/{}/items/{}/attachments/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.get(&path).await
}

/// Attach one or more objects to a task.
///
/// `POST /tasks/{list_id}/items/{task_id}/attachments/` (JSON body). Sends the
/// `target_ids` array form (1–100 ids), which the server accepts for both a
/// single and a bulk attach. The operation is atomic (a partial-batch failure
/// attaches nothing), idempotent (re-attaching is a no-op), and capped at 100
/// attachments per task. Returns the full updated attachment list.
pub async fn attach_to_task(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
    target_ids: &[String],
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "target_ids": target_ids });
    let path = format!(
        "/tasks/{}/items/{}/attachments/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.post_json(&path, &body).await
}

/// Detach a single object from a task.
///
/// `POST /tasks/{list_id}/items/{task_id}/attachments/detach/` (JSON body).
/// Detach is single-object only — there is no batch detach; call once per
/// object. Idempotent: detaching a non-attached object returns `removed: false`.
pub async fn detach_from_task(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
    target_id: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "target_id": target_id });
    let path = format!(
        "/tasks/{}/items/{}/attachments/detach/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.post_json(&path, &body).await
}

// ─── Task Extensions ───────────────────────────────────────────────────────

/// List task lists in a context (workspace or share).
///
/// `GET /{profile_type}/{profile_id}/tasks/`
pub async fn list_task_lists_ctx(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
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
        "/{}/{}/tasks/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Create a task list in a context (workspace or share).
///
/// `POST /{profile_type}/{profile_id}/tasks/create/`
pub async fn create_task_list_ctx(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    name: &str,
    description: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "name": name });
    if let Some(d) = description {
        body["description"] = Value::String(d.to_owned());
    }
    let path = format!(
        "/{}/{}/tasks/create/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    );
    client.post_json(&path, &body).await
}

/// Bulk update task statuses.
///
/// `POST /tasks/{list_id}/items/bulk-status/`
pub async fn bulk_status_tasks(
    client: &ApiClient,
    list_id: &str,
    task_ids: &[String],
    status: &str,
) -> Result<Value, CliError> {
    let tasks: Vec<Value> = task_ids
        .iter()
        .map(|id| serde_json::json!({ "task_id": id, "status": status }))
        .collect();
    let body = serde_json::json!({ "tasks": tasks });
    let path = format!("/tasks/{}/items/bulk-status/", urlencoding::encode(list_id));
    client.post_json(&path, &body).await
}

/// Move a task to a different list.
///
/// `POST /tasks/{list_id}/items/{task_id}/move/`
pub async fn move_task(
    client: &ApiClient,
    list_id: &str,
    task_id: &str,
    target_list_id: &str,
    sort_order: Option<u32>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "target_task_list_id": target_list_id });
    if let Some(s) = sort_order {
        body["sort_order"] = Value::Number(serde_json::Number::from(s));
    }
    let path = format!(
        "/tasks/{}/items/{}/move/",
        urlencoding::encode(list_id),
        urlencoding::encode(task_id),
    );
    client.post_json(&path, &body).await
}

/// Reorder tasks within a list.
///
/// `POST /tasks/{list_id}/items/reorder/`
pub async fn reorder_tasks(
    client: &ApiClient,
    list_id: &str,
    task_ids: &[String],
) -> Result<Value, CliError> {
    let order: Vec<Value> = task_ids
        .iter()
        .enumerate()
        .map(|(idx, id)| serde_json::json!({ "id": id, "sort_order": idx }))
        .collect();
    let body = serde_json::json!({ "order": order });
    let path = format!("/tasks/{}/items/reorder/", urlencoding::encode(list_id));
    client.post_json(&path, &body).await
}

/// Reorder task lists.
///
/// `POST /{profile_type}/{profile_id}/tasks/reorder/`
pub async fn reorder_task_lists(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    list_ids: &[String],
) -> Result<Value, CliError> {
    let order: Vec<Value> = list_ids
        .iter()
        .enumerate()
        .map(|(idx, id)| serde_json::json!({ "id": id, "sort_order": idx }))
        .collect();
    let body = serde_json::json!({ "order": order });
    let path = format!(
        "/{}/{}/tasks/reorder/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    );
    client.post_json(&path, &body).await
}

// ─── Filtered Lists & Summaries ──────────────────────────────────────────────
//
// The Tasks API exposes profile-scoped filtered list endpoints
// (`/{profile_type}/{profile_id}/{kind}/list/{filter}/`) and count summaries
// (`/{profile_type}/{profile_id}/{kind}/summary/`). On a workspace these are
// the personal view (filtered to the current user); on a share they are the
// group view for owners and a scoped view for guests.

/// Common pagination + filter query parameters for the filtered-list endpoints.
///
/// `status` is optional and only honored by the task filters that document it.
#[derive(Default)]
pub struct FilterQuery<'a> {
    /// Max results (server default 50, max 100).
    pub limit: Option<u32>,
    /// Number of results to skip.
    pub offset: Option<u32>,
    /// Status filter (task status values). Only honored by the task filters
    /// that document it.
    pub status: Option<&'a str>,
}

impl FilterQuery<'_> {
    /// Build the query-parameter map for this filter.
    fn to_params(&self) -> HashMap<String, String> {
        let mut params = HashMap::new();
        if let Some(l) = self.limit {
            params.insert("limit".to_owned(), l.to_string());
        }
        if let Some(o) = self.offset {
            params.insert("offset".to_owned(), o.to_string());
        }
        if let Some(s) = self.status {
            params.insert("status".to_owned(), s.to_owned());
        }
        params
    }
}

/// Issue a GET, omitting the query string entirely when no params are set.
async fn get_maybe_params(
    client: &ApiClient,
    path: &str,
    params: &HashMap<String, String>,
) -> Result<Value, CliError> {
    if params.is_empty() {
        client.get(path).await
    } else {
        client.get_with_params(path, params).await
    }
}

/// Build a profile-scoped filtered-list path
/// (`/{profile_type}/{profile_id}/{kind}/list/{filter}/`).
fn filtered_list_path(profile_type: &str, profile_id: &str, kind: &str, filter: &str) -> String {
    format!(
        "/{}/{}/{kind}/list/{}/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
        urlencoding::encode(filter),
    )
}

/// Build a profile-scoped summary path (`/{profile_type}/{profile_id}/{kind}/summary/`).
fn summary_path(profile_type: &str, profile_id: &str, kind: &str) -> String {
    format!(
        "/{}/{}/{kind}/summary/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    )
}

/// Filtered task list for a workspace or share (personal / group view).
///
/// `GET /{profile_type}/{profile_id}/tasks/list/{filter}/`
///
/// Filters: `assigned`, `created` (both accept an optional `status`), `status`
/// (requires `status`).
pub async fn list_tasks_filtered(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    filter: &str,
    query: &FilterQuery<'_>,
) -> Result<Value, CliError> {
    let path = filtered_list_path(profile_type, profile_id, "tasks", filter);
    get_maybe_params(client, &path, &query.to_params()).await
}

/// Task count summary for a workspace or share.
///
/// `GET /{profile_type}/{profile_id}/tasks/summary/`
pub async fn tasks_summary(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
) -> Result<Value, CliError> {
    let path = summary_path(profile_type, profile_id, "tasks");
    client.get(&path).await
}

#[cfg(test)]
mod tests {
    use super::{FilterQuery, filtered_list_path, summary_path};

    #[test]
    fn filtered_list_path_builds_task_route() {
        assert_eq!(
            filtered_list_path("workspace", "1", "tasks", "assigned"),
            "/workspace/1/tasks/list/assigned/"
        );
        assert_eq!(
            filtered_list_path("share", "1", "tasks", "status"),
            "/share/1/tasks/list/status/"
        );
    }

    #[test]
    fn filtered_list_path_url_encodes_segments() {
        let p = filtered_list_path("workspace", "w s", "tasks", "a/b");
        assert!(p.contains("w%20s"), "{p}");
        assert!(p.contains("a%2Fb"), "{p}");
    }

    #[test]
    fn summary_path_is_flat() {
        assert_eq!(
            summary_path("workspace", "1", "tasks"),
            "/workspace/1/tasks/summary/"
        );
        assert_eq!(
            summary_path("share", "1", "tasks"),
            "/share/1/tasks/summary/"
        );
    }

    #[test]
    fn filter_query_builds_only_set_params() {
        let q = FilterQuery {
            limit: Some(10),
            offset: None,
            status: Some("pending"),
        };
        let params = q.to_params();
        assert_eq!(params.get("limit").map(String::as_str), Some("10"));
        assert_eq!(params.get("status").map(String::as_str), Some("pending"));
        assert!(!params.contains_key("offset"));
    }

    #[test]
    fn filter_query_empty_is_empty() {
        let q = FilterQuery::default();
        assert!(q.to_params().is_empty());
    }
}
