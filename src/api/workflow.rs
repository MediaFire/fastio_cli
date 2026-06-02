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

/// Parameters for creating an approval request.
///
/// The approval is created against a **scoped** profile route
/// (`/{profile_type}/{profile_id}/approvals/create/`); the `entity_type` and
/// `entity_id` of the item being approved travel in the JSON body. The scoped
/// route is required because the unscoped `/approvals/{type}/{id}/create/`
/// alias 301-redirects to the scoped form, and the default reqwest redirect
/// policy downgrades the `POST` to a `GET` and drops the body — silently
/// losing the approval payload.
pub struct CreateApprovalParams<'a> {
    /// Profile type the approval is scoped to: `"workspace"` or `"share"`.
    pub profile_type: &'a str,
    /// Workspace or share profile ID (19-digit numeric).
    pub profile_id: &'a str,
    /// Type of entity being approved: `task`, `node`, `worklog_entry`, `share`.
    pub entity_type: &'a str,
    /// Identifier of the entity being approved.
    pub entity_id: &'a str,
    /// What is being approved.
    pub description: &'a str,
    /// Designated approver profile ID; `None` for any admin.
    pub approver_id: Option<&'a str>,
    /// Informational deadline (`YYYY-MM-DD HH:MM:SS UTC`).
    pub deadline: Option<&'a str>,
    /// Associated artifact node ID.
    pub node_id: Option<&'a str>,
    /// Optional metadata properties (a JSON object). Parsed and validated by
    /// the caller; sent verbatim as the `properties` body field when present.
    pub properties: Option<&'a Value>,
}

/// Build the scoped approval-create path for a workspace or share.
fn approval_create_path(profile_type: &str, profile_id: &str) -> String {
    format!(
        "/{}/{}/approvals/create/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    )
}

/// Build the JSON body for an approval-create request.
fn build_create_approval_body(params: &CreateApprovalParams<'_>) -> Value {
    let mut body = serde_json::json!({
        "entity_type": params.entity_type,
        "entity_id": params.entity_id,
        "description": params.description,
        "profile_id": params.profile_id,
    });
    if let Some(a) = params.approver_id {
        body["approver_id"] = Value::String(a.to_owned());
    }
    if let Some(d) = params.deadline {
        body["deadline"] = Value::String(d.to_owned());
    }
    if let Some(n) = params.node_id {
        body["node_id"] = Value::String(n.to_owned());
    }
    if let Some(p) = params.properties {
        body["properties"] = p.clone();
    }
    body
}

/// Create an approval request scoped to a workspace or share.
///
/// `POST /{profile_type}/{profile_id}/approvals/create/`
pub async fn create_approval(
    client: &ApiClient,
    params: &CreateApprovalParams<'_>,
) -> Result<Value, CliError> {
    let body = build_create_approval_body(params);
    let path = approval_create_path(params.profile_type, params.profile_id);
    client.post_json(&path, &body).await
}

/// Build a scoped per-approval action path
/// (`details`/`resolve`/`update`/`delete`).
fn approval_action_path(
    profile_type: &str,
    profile_id: &str,
    approval_id: &str,
    action: &str,
) -> String {
    format!(
        "/{}/{}/approvals/{}/{action}/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
        urlencoding::encode(approval_id),
    )
}

/// Build the legacy **unscoped** per-approval action path
/// (`/approvals/{approval_id}/{action}/`).
///
/// Used only as a backward-compatibility fallback when the caller does not
/// supply a workspace/share scope. The scoped form ([`approval_action_path`])
/// is preferred and is what every freshly-issued command uses; this exists so
/// the historical `approval approve <id>` syntax (no scope flag) keeps working.
fn approval_action_path_unscoped(approval_id: &str, action: &str) -> String {
    format!("/approvals/{}/{action}/", urlencoding::encode(approval_id),)
}

/// Get approval details.
///
/// When `scope` is `Some((profile_type, profile_id))` the scoped route
/// `GET /{profile_type}/{profile_id}/approvals/{approval_id}/details/` is used.
/// When `scope` is `None` the legacy unscoped route
/// `GET /approvals/{approval_id}/details/` is used for backward compatibility.
pub async fn get_approval(
    client: &ApiClient,
    scope: Option<(&str, &str)>,
    approval_id: &str,
) -> Result<Value, CliError> {
    let path = match scope {
        Some((profile_type, profile_id)) => {
            approval_action_path(profile_type, profile_id, approval_id, "details")
        }
        None => approval_action_path_unscoped(approval_id, "details"),
    };
    client.get(&path).await
}

/// Build the JSON body for an approval resolve request.
fn build_resolve_approval_body(action: &str, comment: Option<&str>) -> Value {
    let mut body = serde_json::json!({ "action": action });
    if let Some(c) = comment {
        body["comment"] = Value::String(c.to_owned());
    }
    body
}

/// Resolve (approve or reject) an approval.
///
/// When `scope` is `Some((profile_type, profile_id))` the scoped route
/// `POST /{profile_type}/{profile_id}/approvals/{approval_id}/resolve/` is used.
/// When `scope` is `None` the legacy unscoped route
/// `POST /approvals/{approval_id}/resolve/` is used for backward compatibility
/// (the historical no-scope `approval approve <id>` syntax).
pub async fn resolve_approval(
    client: &ApiClient,
    scope: Option<(&str, &str)>,
    approval_id: &str,
    action: &str,
    comment: Option<&str>,
) -> Result<Value, CliError> {
    let body = build_resolve_approval_body(action, comment);
    let path = match scope {
        Some((profile_type, profile_id)) => {
            approval_action_path(profile_type, profile_id, approval_id, "resolve")
        }
        None => approval_action_path_unscoped(approval_id, "resolve"),
    };
    client.post_json(&path, &body).await
}

/// Parameters for updating a pending approval.
pub struct UpdateApprovalParams<'a> {
    /// Scope the approval is addressed through, as
    /// `Some((profile_type, profile_id))`. `None` selects the legacy unscoped
    /// route for backward compatibility (no scope flag supplied).
    pub scope: Option<(&'a str, &'a str)>,
    /// Approval ID to update.
    pub approval_id: &'a str,
    /// Updated description.
    pub description: Option<&'a str>,
    /// Updated designated approver profile ID.
    pub approver_id: Option<&'a str>,
    /// Updated deadline (`YYYY-MM-DD HH:MM:SS UTC`).
    pub deadline: Option<&'a str>,
    /// Updated associated node ID.
    pub node_id: Option<&'a str>,
    /// Updated metadata properties (a JSON object). Parsed and validated by
    /// the caller; sent verbatim as the `properties` body field when present.
    pub properties: Option<&'a Value>,
}

/// Update a pending approval, scoped to a workspace or share.
///
/// `POST /{profile_type}/{profile_id}/approvals/{approval_id}/update/`
///
/// Only pending approvals can be updated; at least one mutable field must be
/// supplied (validated by the caller).
pub async fn update_approval(
    client: &ApiClient,
    params: &UpdateApprovalParams<'_>,
) -> Result<Value, CliError> {
    let mut body = serde_json::Map::new();
    if let Some(d) = params.description {
        body.insert("description".to_owned(), Value::String(d.to_owned()));
    }
    if let Some(a) = params.approver_id {
        body.insert("approver_id".to_owned(), Value::String(a.to_owned()));
    }
    if let Some(d) = params.deadline {
        body.insert("deadline".to_owned(), Value::String(d.to_owned()));
    }
    if let Some(n) = params.node_id {
        body.insert("node_id".to_owned(), Value::String(n.to_owned()));
    }
    if let Some(p) = params.properties {
        body.insert("properties".to_owned(), p.clone());
    }
    let path = match params.scope {
        Some((profile_type, profile_id)) => {
            approval_action_path(profile_type, profile_id, params.approval_id, "update")
        }
        None => approval_action_path_unscoped(params.approval_id, "update"),
    };
    client.post_json(&path, &Value::Object(body)).await
}

/// Delete an approval.
///
/// When `scope` is `Some((profile_type, profile_id))` the scoped route
/// `POST /{profile_type}/{profile_id}/approvals/{approval_id}/delete/` is used.
/// When `scope` is `None` the legacy unscoped route
/// `POST /approvals/{approval_id}/delete/` is used for backward compatibility.
///
/// Both pending and resolved approvals can be deleted; deletion is permanent.
pub async fn delete_approval(
    client: &ApiClient,
    scope: Option<(&str, &str)>,
    approval_id: &str,
) -> Result<Value, CliError> {
    let path = match scope {
        Some((profile_type, profile_id)) => {
            approval_action_path(profile_type, profile_id, approval_id, "delete")
        }
        None => approval_action_path_unscoped(approval_id, "delete"),
    };
    client.post_json(&path, &serde_json::json!({})).await
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

// ─── Filtered Lists & Summaries ──────────────────────────────────────────────
//
// All four workflow primitives expose profile-scoped filtered list endpoints
// (`/{profile_type}/{profile_id}/{kind}/list/{filter}/`) and count summaries
// (`/{profile_type}/{profile_id}/{kind}/summary/`). On a workspace these are
// the personal view (filtered to the current user); on a share they are the
// group view for owners and a scoped view for guests.

/// Common pagination + filter query parameters for the filtered-list endpoints.
///
/// `status` applies to approvals/tasks, `entry_type` to worklogs; both are
/// optional and only honored by the filters that document them.
#[derive(Default)]
pub struct FilterQuery<'a> {
    /// Max results (server default 50, max 100).
    pub limit: Option<u32>,
    /// Number of results to skip.
    pub offset: Option<u32>,
    /// Status filter (`pending`/`approved`/`rejected` for approvals;
    /// task status values for tasks). Only honored by filters that document it.
    pub status: Option<&'a str>,
    /// Worklog entry-type filter (only honored by the `authored` worklog filter).
    pub entry_type: Option<&'a str>,
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
        if let Some(t) = self.entry_type {
            params.insert("entry_type".to_owned(), t.to_owned());
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

/// Build a profile-scoped summary path. Approvals nest the summary under
/// `list/` (`approvals/list/summary/`); the other kinds use `{kind}/summary/`.
fn summary_path(profile_type: &str, profile_id: &str, kind: &str) -> String {
    let tail = if kind == "approvals" {
        "approvals/list/summary"
    } else {
        return format!(
            "/{}/{}/{kind}/summary/",
            urlencoding::encode(profile_type),
            urlencoding::encode(profile_id),
        );
    };
    format!(
        "/{}/{}/{tail}/",
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

/// Filtered todo list for a workspace or share (personal / group view).
///
/// `GET /{profile_type}/{profile_id}/todos/list/{filter}/`
///
/// Filters: `assigned`, `created`, `done`, `pending`.
pub async fn list_todos_filtered(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    filter: &str,
    query: &FilterQuery<'_>,
) -> Result<Value, CliError> {
    let path = filtered_list_path(profile_type, profile_id, "todos", filter);
    get_maybe_params(client, &path, &query.to_params()).await
}

/// Todo count summary for a workspace or share.
///
/// `GET /{profile_type}/{profile_id}/todos/summary/`
pub async fn todos_summary(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
) -> Result<Value, CliError> {
    let path = summary_path(profile_type, profile_id, "todos");
    client.get(&path).await
}

/// List all worklog entries in a workspace or share.
///
/// `GET /{profile_type}/{profile_id}/worklogs/`
pub async fn list_worklogs_ctx(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    query: &FilterQuery<'_>,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/worklogs/",
        urlencoding::encode(profile_type),
        urlencoding::encode(profile_id),
    );
    get_maybe_params(client, &path, &query.to_params()).await
}

/// Filtered worklog list for a workspace or share (personal / group view).
///
/// `GET /{profile_type}/{profile_id}/worklogs/list/{filter}/`
///
/// Filters: `authored` (accepts an optional `entry_type`), `interjections`.
pub async fn list_worklogs_filtered(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    filter: &str,
    query: &FilterQuery<'_>,
) -> Result<Value, CliError> {
    let path = filtered_list_path(profile_type, profile_id, "worklogs", filter);
    get_maybe_params(client, &path, &query.to_params()).await
}

/// Worklog entry summary for a workspace or share.
///
/// `GET /{profile_type}/{profile_id}/worklogs/summary/`
pub async fn worklogs_summary(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
) -> Result<Value, CliError> {
    let path = summary_path(profile_type, profile_id, "worklogs");
    client.get(&path).await
}

/// Filtered approval list for a workspace or share (personal / group view).
///
/// `GET /{profile_type}/{profile_id}/approvals/list/{filter}/`
///
/// Filters: `pending`, `created` (accepts optional `status`), `assigned`
/// (accepts optional `status`), `resolved`.
pub async fn list_approvals_filtered(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
    filter: &str,
    query: &FilterQuery<'_>,
) -> Result<Value, CliError> {
    let path = filtered_list_path(profile_type, profile_id, "approvals", filter);
    get_maybe_params(client, &path, &query.to_params()).await
}

/// Approval count summary for a workspace or share.
///
/// `GET /{profile_type}/{profile_id}/approvals/list/summary/`
pub async fn approvals_summary(
    client: &ApiClient,
    profile_type: &str,
    profile_id: &str,
) -> Result<Value, CliError> {
    let path = summary_path(profile_type, profile_id, "approvals");
    client.get(&path).await
}

/// Build the user-approvals dashboard path.
fn user_approvals_path(filter: &str) -> String {
    format!("/user/approvals/list/{}/", urlencoding::encode(filter))
}

/// List approvals for the authenticated user across all profiles.
///
/// `GET /user/approvals/list/{filter}/`
///
/// Filters: `pending` (user is the designated approver, still pending),
/// `created` (user requested; accepts optional `status`), `resolved`
/// (user resolved). No workspace/share membership required.
pub async fn user_approvals(
    client: &ApiClient,
    filter: &str,
    query: &FilterQuery<'_>,
) -> Result<Value, CliError> {
    let path = user_approvals_path(filter);
    get_maybe_params(client, &path, &query.to_params()).await
}

#[cfg(test)]
mod tests {
    use super::{
        CreateApprovalParams, FilterQuery, UpdateApprovalParams, approval_action_path,
        approval_action_path_unscoped, approval_create_path, build_create_approval_body,
        build_resolve_approval_body, filtered_list_path, summary_path, user_approvals_path,
    };
    use serde_json::json;

    #[test]
    fn create_approval_uses_scoped_path_workspace() {
        // Must be the SCOPED route — the unscoped /approvals/{type}/{id}/create/
        // redirects and the default reqwest redirect policy drops the POST body.
        let p = approval_create_path("workspace", "1234567890123456789");
        assert_eq!(p, "/workspace/1234567890123456789/approvals/create/");
    }

    #[test]
    fn create_approval_uses_scoped_path_share() {
        let p = approval_create_path("share", "9876543210987654321");
        assert_eq!(p, "/share/9876543210987654321/approvals/create/");
    }

    #[test]
    fn create_approval_body_carries_entity_and_optional_fields() {
        // Regression: the body must travel intact (entity_type/entity_id were
        // previously only in the path; now they must be in the JSON body).
        let params = CreateApprovalParams {
            profile_type: "share",
            profile_id: "111",
            entity_type: "task",
            entity_id: "abc",
            description: "review",
            approver_id: Some("222"),
            deadline: Some("2025-06-15 23:59:59"),
            node_id: Some("node9"),
            properties: None,
        };
        let body = build_create_approval_body(&params);
        assert_eq!(body["entity_type"], "task");
        assert_eq!(body["entity_id"], "abc");
        assert_eq!(body["description"], "review");
        assert_eq!(body["profile_id"], "111");
        assert_eq!(body["approver_id"], "222");
        assert_eq!(body["deadline"], "2025-06-15 23:59:59");
        assert_eq!(body["node_id"], "node9");
    }

    #[test]
    fn create_approval_body_omits_unset_optionals() {
        let params = CreateApprovalParams {
            profile_type: "workspace",
            profile_id: "111",
            entity_type: "node",
            entity_id: "n1",
            description: "d",
            approver_id: None,
            deadline: None,
            node_id: None,
            properties: None,
        };
        let body = build_create_approval_body(&params);
        assert!(body.get("approver_id").is_none());
        assert!(body.get("deadline").is_none());
        assert!(body.get("node_id").is_none());
        assert!(body.get("properties").is_none());
    }

    #[test]
    fn resolve_approval_uses_scoped_path_and_keeps_body() {
        let p = approval_action_path("share", "111", "appr1", "resolve");
        assert_eq!(p, "/share/111/approvals/appr1/resolve/");
        let body = build_resolve_approval_body("approve", Some("looks good"));
        assert_eq!(body["action"], "approve");
        assert_eq!(body["comment"], "looks good");
    }

    #[test]
    fn resolve_approval_body_omits_empty_comment() {
        let body = build_resolve_approval_body("reject", None);
        assert_eq!(body["action"], "reject");
        assert!(body.get("comment").is_none());
    }

    #[test]
    fn approval_action_paths_cover_details_update_delete() {
        assert_eq!(
            approval_action_path("workspace", "1", "a", "details"),
            "/workspace/1/approvals/a/details/"
        );
        assert_eq!(
            approval_action_path("workspace", "1", "a", "update"),
            "/workspace/1/approvals/a/update/"
        );
        assert_eq!(
            approval_action_path("share", "1", "a", "delete"),
            "/share/1/approvals/a/delete/"
        );
    }

    #[test]
    fn approval_paths_url_encode_segments() {
        let p = approval_action_path("workspace", "w s", "a/b", "resolve");
        assert!(p.contains("w%20s"), "{p}");
        assert!(p.contains("a%2Fb"), "{p}");
    }

    #[test]
    fn approval_action_path_unscoped_is_legacy_route() {
        // Backward-compat: with no scope the legacy unscoped route is used so
        // the historical `approval approve <id>` syntax keeps working.
        assert_eq!(
            approval_action_path_unscoped("appr1", "resolve"),
            "/approvals/appr1/resolve/"
        );
        assert_eq!(
            approval_action_path_unscoped("appr1", "details"),
            "/approvals/appr1/details/"
        );
        // Segments are URL-encoded.
        assert!(approval_action_path_unscoped("a/b", "delete").contains("a%2Fb"));
    }

    #[test]
    fn create_approval_body_carries_properties_object() {
        let props = json!({"region": "us-east", "tier": 2});
        let params = CreateApprovalParams {
            profile_type: "workspace",
            profile_id: "111",
            entity_type: "task",
            entity_id: "abc",
            description: "review",
            approver_id: None,
            deadline: None,
            node_id: None,
            properties: Some(&props),
        };
        let body = build_create_approval_body(&params);
        assert_eq!(body["properties"]["region"], "us-east");
        assert_eq!(body["properties"]["tier"], 2);
    }

    #[test]
    fn update_approval_params_carry_properties_and_scope() {
        let props = json!({"note": "bump"});
        let params = UpdateApprovalParams {
            scope: None,
            approval_id: "a",
            description: None,
            approver_id: None,
            deadline: None,
            node_id: None,
            properties: Some(&props),
        };
        // With no scope, the legacy unscoped update route is selected.
        assert!(params.scope.is_none());
        assert_eq!(
            approval_action_path_unscoped(params.approval_id, "update"),
            "/approvals/a/update/"
        );
        // The properties payload is preserved on the params for the body build.
        assert_eq!(params.properties.expect("set")["note"], "bump");
    }

    #[test]
    fn update_approval_body_built_from_params() {
        // Exercise the public UpdateApprovalParams path-building indirectly:
        // the path helper is shared, so assert the path here.
        let params = UpdateApprovalParams {
            scope: Some(("share", "1")),
            approval_id: "a",
            description: Some("new"),
            approver_id: None,
            deadline: None,
            node_id: None,
            properties: None,
        };
        let (profile_type, profile_id) = params.scope.expect("scope set");
        let p = approval_action_path(profile_type, profile_id, params.approval_id, "update");
        assert_eq!(p, "/share/1/approvals/a/update/");
    }

    #[test]
    fn filtered_list_paths_per_kind() {
        assert_eq!(
            filtered_list_path("workspace", "1", "tasks", "assigned"),
            "/workspace/1/tasks/list/assigned/"
        );
        assert_eq!(
            filtered_list_path("share", "1", "todos", "pending"),
            "/share/1/todos/list/pending/"
        );
        assert_eq!(
            filtered_list_path("workspace", "1", "worklogs", "authored"),
            "/workspace/1/worklogs/list/authored/"
        );
        assert_eq!(
            filtered_list_path("workspace", "1", "approvals", "created"),
            "/workspace/1/approvals/list/created/"
        );
    }

    #[test]
    fn summary_path_approvals_nests_under_list() {
        // Approvals summary lives at approvals/list/summary/ (not approvals/summary/).
        assert_eq!(
            summary_path("workspace", "1", "approvals"),
            "/workspace/1/approvals/list/summary/"
        );
    }

    #[test]
    fn summary_path_other_kinds_are_flat() {
        assert_eq!(
            summary_path("workspace", "1", "tasks"),
            "/workspace/1/tasks/summary/"
        );
        assert_eq!(
            summary_path("share", "1", "todos"),
            "/share/1/todos/summary/"
        );
        assert_eq!(
            summary_path("workspace", "1", "worklogs"),
            "/workspace/1/worklogs/summary/"
        );
    }

    #[test]
    fn user_approvals_path_correct() {
        assert_eq!(
            user_approvals_path("pending"),
            "/user/approvals/list/pending/"
        );
        assert_eq!(
            user_approvals_path("resolved"),
            "/user/approvals/list/resolved/"
        );
    }

    #[test]
    fn filter_query_builds_only_set_params() {
        let q = FilterQuery {
            limit: Some(10),
            offset: None,
            status: Some("pending"),
            entry_type: None,
        };
        let params = q.to_params();
        assert_eq!(params.get("limit").map(String::as_str), Some("10"));
        assert_eq!(params.get("status").map(String::as_str), Some("pending"));
        assert!(!params.contains_key("offset"));
        assert!(!params.contains_key("entry_type"));
    }

    #[test]
    fn filter_query_empty_is_empty() {
        let q = FilterQuery::default();
        assert!(q.to_params().is_empty());
    }
}
