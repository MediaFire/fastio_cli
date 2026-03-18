#![allow(clippy::missing_errors_doc)]

/// Workflow API endpoints for the Fast.io REST API.
///
/// Covers task lists, tasks, worklogs, approvals, and todos.
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

// ─── Worklogs ───────────────────────────────────────────────────────────────

/// List worklog entries.
///
/// `GET /worklogs/{entity_type}/{entity_id}/`
pub async fn list_worklogs(
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
        "/worklogs/{}/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Append a worklog entry.
///
/// `POST /worklogs/{entity_type}/{entity_id}/append/`
pub async fn append_worklog(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "content": content });
    let path = format!(
        "/worklogs/{}/{}/append/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    client.post_json(&path, &body).await
}

/// Create a worklog interjection.
///
/// `POST /worklogs/{entity_type}/{entity_id}/interjection/`
pub async fn interject_worklog(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "content": content });
    let path = format!(
        "/worklogs/{}/{}/interjection/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    client.post_json(&path, &body).await
}

// ─── Approvals ──────────────────────────────────────────────────────────────

/// List approvals in a workspace.
///
/// `GET /workspace/{workspace_id}/approvals/`
pub async fn list_approvals(
    client: &ApiClient,
    workspace_id: &str,
    status: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(s) = status {
        params.insert("status".to_owned(), s.to_owned());
    }
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/approvals/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Create an approval request.
///
/// `POST /approvals/{entity_type}/{entity_id}/create/`
pub async fn create_approval(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    description: &str,
    profile_id: &str,
    approver_id: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({
        "description": description,
        "profile_id": profile_id,
    });
    if let Some(a) = approver_id {
        body["approver_id"] = Value::String(a.to_owned());
    }
    let path = format!(
        "/approvals/{}/{}/create/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    client.post_json(&path, &body).await
}

/// Get approval details.
///
/// `GET /approvals/{approval_id}/details/`
#[allow(dead_code)]
pub async fn get_approval(client: &ApiClient, approval_id: &str) -> Result<Value, CliError> {
    let path = format!("/approvals/{}/details/", urlencoding::encode(approval_id),);
    client.get(&path).await
}

/// Resolve (approve or reject) an approval.
///
/// `POST /approvals/{approval_id}/resolve/`
pub async fn resolve_approval(
    client: &ApiClient,
    approval_id: &str,
    action: &str,
    comment: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "action": action });
    if let Some(c) = comment {
        body["comment"] = Value::String(c.to_owned());
    }
    let path = format!("/approvals/{}/resolve/", urlencoding::encode(approval_id),);
    client.post_json(&path, &body).await
}

// ─── Todos ──────────────────────────────────────────────────────────────────

/// List todos in a workspace.
///
/// `GET /workspace/{workspace_id}/todos/`
pub async fn list_todos(
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
    let path = format!("/workspace/{}/todos/", urlencoding::encode(workspace_id),);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Create a todo.
///
/// `POST /workspace/{workspace_id}/todos/create/`
pub async fn create_todo(
    client: &ApiClient,
    workspace_id: &str,
    title: &str,
    assignee_id: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "title": title });
    if let Some(a) = assignee_id {
        body["assignee_id"] = Value::String(a.to_owned());
    }
    let path = format!(
        "/workspace/{}/todos/create/",
        urlencoding::encode(workspace_id),
    );
    client.post_json(&path, &body).await
}

/// Update a todo.
///
/// `POST /todos/{todo_id}/details/update/`
pub async fn update_todo(
    client: &ApiClient,
    todo_id: &str,
    title: Option<&str>,
    done: Option<bool>,
    assignee_id: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::Map::new();
    if let Some(t) = title {
        body.insert("title".to_owned(), Value::String(t.to_owned()));
    }
    if let Some(d) = done {
        body.insert("done".to_owned(), Value::Bool(d));
    }
    if let Some(a) = assignee_id {
        body.insert("assignee_id".to_owned(), Value::String(a.to_owned()));
    }
    let path = format!("/todos/{}/details/update/", urlencoding::encode(todo_id));
    client.post_json(&path, &Value::Object(body)).await
}

/// Delete a todo (soft delete).
///
/// `POST /todos/{todo_id}/details/delete/`
pub async fn delete_todo(client: &ApiClient, todo_id: &str) -> Result<Value, CliError> {
    let path = format!("/todos/{}/details/delete/", urlencoding::encode(todo_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// Toggle a todo's completion state.
///
/// `POST /todos/{todo_id}/details/toggle/`
pub async fn toggle_todo(client: &ApiClient, todo_id: &str) -> Result<Value, CliError> {
    let path = format!("/todos/{}/details/toggle/", urlencoding::encode(todo_id),);
    client.post_json(&path, &serde_json::json!({})).await
}

/// Get todo details.
///
/// `GET /todos/{todo_id}/details/`
pub async fn get_todo_details(client: &ApiClient, todo_id: &str) -> Result<Value, CliError> {
    let path = format!("/todos/{}/details/", urlencoding::encode(todo_id));
    client.get(&path).await
}

/// Bulk toggle todos.
///
/// `POST /{profile_type}/{profile_id}/todos/bulk-toggle/`
pub async fn bulk_toggle_todos(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    todo_ids: &[String],
    done: bool,
) -> Result<Value, CliError> {
    let body = serde_json::json!({
        "todo_ids": todo_ids,
        "done": done,
    });
    let path = format!(
        "/{}/{}/todos/bulk-toggle/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    );
    client.post_json(&path, &body).await
}

/// List todos in a context (workspace or share).
///
/// `GET /{profile_type}/{profile_id}/todos/`
pub async fn list_todos_ctx(
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
        "/{}/{}/todos/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Create a todo in a context (workspace or share).
///
/// `POST /{profile_type}/{profile_id}/todos/create/`
pub async fn create_todo_ctx(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    title: &str,
    assignee_id: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "title": title });
    if let Some(a) = assignee_id {
        body["assignee_id"] = Value::String(a.to_owned());
    }
    let path = format!(
        "/{}/{}/todos/create/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    );
    client.post_json(&path, &body).await
}

// ─── Worklog Extensions ────────────────────────────────────────────────────

/// Get worklog entry details.
///
/// `GET /worklogs/{entry_id}/details/`
pub async fn worklog_details(client: &ApiClient, entry_id: &str) -> Result<Value, CliError> {
    let path = format!("/worklogs/{}/details/", urlencoding::encode(entry_id));
    client.get(&path).await
}

/// Acknowledge a worklog interjection.
///
/// `POST /worklogs/{entry_id}/acknowledge/`
pub async fn acknowledge_worklog(client: &ApiClient, entry_id: &str) -> Result<Value, CliError> {
    let path = format!("/worklogs/{}/acknowledge/", urlencoding::encode(entry_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// List unacknowledged interjections.
///
/// `GET /worklogs/{entity_type}/{entity_id}/interjections/`
pub async fn unacknowledged_worklogs(
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
        "/worklogs/{}/{}/interjections/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
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
