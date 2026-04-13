#![allow(clippy::missing_errors_doc)]

/// Workspace API endpoints for the Fast.io REST API.
///
/// Maps to the endpoints documented in `/current/workspace/`.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List all workspaces the user has access to.
///
/// `GET /workspaces/all/` or `GET /org/{org_id}/list/workspaces/` when filtered.
pub async fn list_workspaces(
    client: &ApiClient,
    org_id: Option<&str>,
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
    let path = if let Some(oid) = org_id {
        format!("/org/{}/list/workspaces/", urlencoding::encode(oid))
    } else {
        "/workspaces/all/".to_owned()
    };
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Parameters for [`create_workspace`].
pub struct CreateWorkspaceParams<'a> {
    /// Parent organization for the new workspace.
    pub org_id: &'a str,
    /// Root folder name on the storage backend.
    pub folder_name: &'a str,
    /// Human-readable workspace name.
    pub name: &'a str,
    /// Optional workspace description.
    pub description: Option<&'a str>,
    /// Enable AI-powered intelligence features.
    pub intelligence: Option<bool>,
}

/// Create a workspace in an organization.
///
/// `POST /org/{org_id}/create/workspace/`
pub async fn create_workspace(
    client: &ApiClient,
    params: &CreateWorkspaceParams<'_>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("folder_name".to_owned(), params.folder_name.to_owned());
    form.insert("name".to_owned(), params.name.to_owned());
    if let Some(v) = params.description {
        form.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = params.intelligence {
        form.insert("intelligence".to_owned(), v.to_string());
    }
    let path = format!(
        "/org/{}/create/workspace/",
        urlencoding::encode(params.org_id),
    );
    client.post(&path, &form).await
}

/// Get workspace details.
///
/// `GET /workspace/{workspace_id}/details/`
pub async fn get_workspace(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!("/workspace/{}/details/", urlencoding::encode(workspace_id),);
    client.get(&path).await
}

/// Update workspace settings.
///
/// `POST /workspace/{workspace_id}/update/`
#[allow(clippy::implicit_hasher)]
pub async fn update_workspace(
    client: &ApiClient,
    workspace_id: &str,
    fields: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = format!("/workspace/{}/update/", urlencoding::encode(workspace_id),);
    client.post(&path, fields).await
}

/// Delete a workspace.
///
/// `DELETE /workspace/{workspace_id}/delete/`
pub async fn delete_workspace(
    client: &ApiClient,
    workspace_id: &str,
    confirm: &str,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("confirm".to_owned(), confirm.to_owned());
    let path = format!("/workspace/{}/delete/", urlencoding::encode(workspace_id),);
    client.delete_with_params(&path, &params).await
}

/// Search workspace content.
///
/// `GET /workspace/{workspace_id}/storage/search/`
pub async fn search_workspace(
    client: &ApiClient,
    workspace_id: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("search".to_owned(), query.to_owned());
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/storage/search/",
        urlencoding::encode(workspace_id),
    );
    client.get_with_params(&path, &params).await
}

/// Get workspace limits/usage.
///
/// `GET /workspace/{workspace_id}/limits/`
pub async fn get_workspace_limits(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    let path = format!("/workspace/{}/limits/", urlencoding::encode(workspace_id),);
    client.get(&path).await
}

/// List workspace members.
///
/// `GET /workspace/{workspace_id}/members/list/`
pub async fn list_workspace_members(
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
        "/workspace/{}/members/list/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Enable workflow features on a workspace.
///
/// `POST /workspace/{workspace_id}/update/` with `intelligence=true`.
pub async fn enable_workflow(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("intelligence".to_owned(), "true".to_owned());
    let path = format!("/workspace/{}/update/", urlencoding::encode(workspace_id),);
    client.post(&path, &form).await
}

/// Archive a workspace.
///
/// `POST /workspace/{workspace_id}/archive/`
pub async fn archive_workspace(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!("/workspace/{}/archive/", urlencoding::encode(workspace_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// Unarchive a workspace.
///
/// `POST /workspace/{workspace_id}/unarchive/`
pub async fn unarchive_workspace(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/unarchive/",
        urlencoding::encode(workspace_id)
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List shares in a workspace.
///
/// `GET /workspace/{workspace_id}/list/shares/`
pub async fn list_workspace_shares(
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
        "/workspace/{}/list/shares/",
        urlencoding::encode(workspace_id)
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Import a share into a workspace.
///
/// `POST /workspace/{workspace_id}/import/share/{share_id}/`
pub async fn import_share(
    client: &ApiClient,
    workspace_id: &str,
    share_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/import/share/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(share_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List available workspaces for the current user.
///
/// `GET /workspaces/available/`
pub async fn available_workspaces(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/workspaces/available/").await
}

/// Check workspace name availability.
///
/// `GET /workspaces/check/name/{name}/`
pub async fn check_workspace_name(client: &ApiClient, name: &str) -> Result<Value, CliError> {
    let path = format!("/workspaces/check/name/{}/", urlencoding::encode(name));
    client.get(&path).await
}

/// Create a note in a workspace.
///
/// `POST /workspace/{workspace_id}/storage/{parent_id}/notes/`
pub async fn create_note(
    client: &ApiClient,
    workspace_id: &str,
    parent_id: &str,
    name: &str,
    content: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({ "name": name });
    if let Some(c) = content {
        body["content"] = Value::String(c.to_owned());
    }
    let path = format!(
        "/workspace/{}/storage/{}/notes/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    client.post_json(&path, &body).await
}

/// Update a note in a workspace.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/notes/update/`
pub async fn update_note(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    name: Option<&str>,
    content: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::Map::new();
    if let Some(n) = name {
        body.insert("name".to_owned(), Value::String(n.to_owned()));
    }
    if let Some(c) = content {
        body.insert("content".to_owned(), Value::String(c.to_owned()));
    }
    let path = format!(
        "/workspace/{}/storage/{}/notes/update/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post_json(&path, &Value::Object(body)).await
}

/// Read a note's content.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/notes/`
pub async fn read_note(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/notes/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Get quickshare details.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/quickshare/`
pub async fn quickshare_get(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/quickshare/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Delete a quickshare.
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/quickshare/`
pub async fn quickshare_delete(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/quickshare/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.delete(&path).await
}

/// List quickshares in a workspace.
///
/// `GET /workspace/{workspace_id}/quickshares/`
pub async fn quickshares_list(
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
        "/workspace/{}/quickshares/",
        urlencoding::encode(workspace_id)
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Disable workflow on a workspace.
///
/// `POST /workspace/{workspace_id}/workflow/disable/`
pub async fn disable_workflow(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/workflow/disable/",
        urlencoding::encode(workspace_id)
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Enable import on a workspace.
///
/// `POST /workspace/{workspace_id}/import/enable/`
pub async fn enable_import(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/import/enable/",
        urlencoding::encode(workspace_id)
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Disable import on a workspace.
///
/// `POST /workspace/{workspace_id}/import/disable/`
pub async fn disable_import(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/import/disable/",
        urlencoding::encode(workspace_id)
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List active background jobs for a workspace.
///
/// Returns the workspace's job status snapshot under a top-level `jobs`
/// object with these children (each is either an object/`null` for
/// singleton sweeps or an array for per-resource jobs):
///
/// - `jobs.intelligence` — object or `null`. Workspace-wide AI-indexing
///   sweep status.
/// - `jobs.summarize` — object or `null`. AI-summary generation sweep.
/// - `jobs.upsert_file` — object or `null`. File upsert / bulk-write sweep.
/// - `jobs.metadata_extract` — array of active and recently-completed
///   metadata extraction jobs.
/// - `jobs.template_match` — array of active and recently-completed
///   `auto-match` jobs.
///
/// Each entry in `metadata_extract` carries a `kind` discriminator:
/// `"single"` for per-node jobs (match on `node_id` / `template_id`) and
/// `"batch"` for template-wide `extract-all` runs (match on `template_id`).
/// Single-job `status` values are one of `"queued"`, `"in_progress"`,
/// `"completed"`, `"errored"`; on `"errored"`, surface `error_message`.
///
/// Callers must poll this endpoint after enqueueing an asynchronous
/// extraction via [`crate::api::metadata::extract_node_metadata`] or
/// `POST /metadata/templates/{template_id}/extract-all/` — the extraction
/// response does not carry values; read them from `/metadata/details/`
/// after `status == "completed"`. Stale entries (completed or errored
/// more than one hour ago) are hidden by the server.
///
/// `GET /workspace/{workspace_id}/jobs/status/`
pub async fn jobs_status(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/jobs/status/",
        urlencoding::encode(workspace_id),
    );
    client.get(&path).await
}

/// Generic metadata API call helper.
///
/// Provides a passthrough for various metadata endpoints.
#[allow(clippy::implicit_hasher)]
pub async fn metadata_api(
    client: &ApiClient,
    workspace_id: &str,
    sub_path: &str,
    method: &str,
    body: Option<&Value>,
    params: Option<&HashMap<String, String>>,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/{}",
        urlencoding::encode(workspace_id),
        sub_path,
    );
    match method {
        "GET" => {
            if let Some(p) = params {
                client.get_with_params(&path, p).await
            } else {
                client.get(&path).await
            }
        }
        "POST" => {
            if let Some(b) = body {
                client.post_json(&path, b).await
            } else {
                client.post_json(&path, &serde_json::json!({})).await
            }
        }
        "DELETE" => client.delete(&path).await,
        _ => client.get(&path).await,
    }
}
