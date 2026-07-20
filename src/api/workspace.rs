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

/// Default join permission when the `workspace create` command does not expose
/// a flag for it (the server has no default and hard-requires the field).
const DEFAULT_PERM_JOIN: &str = "Member or above";
/// Default member-management permission (see [`DEFAULT_PERM_JOIN`]).
const DEFAULT_PERM_MEMBER_MANAGE: &str = "Admin or above";

/// Create a workspace in an organization.
///
/// `POST /org/{org_id}/create/workspace/` — `org_id` is the URL path part. The
/// server hard-requires `folder_name`, `name`, `perm_join`,
/// `perm_member_manage`, and `intelligence` (no server defaults), so this
/// builder always sends `perm_join` / `perm_member_manage` (defaulted, since the
/// `workspace create` command does not expose flags for them) and `intelligence`
/// (defaulting to `false` when unset). The fully-flagged variant is
/// [`crate::api::org::create_workspace`].
pub async fn create_workspace(
    client: &ApiClient,
    params: &CreateWorkspaceParams<'_>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("folder_name".to_owned(), params.folder_name.to_owned());
    form.insert("name".to_owned(), params.name.to_owned());
    form.insert("perm_join".to_owned(), DEFAULT_PERM_JOIN.to_owned());
    form.insert(
        "perm_member_manage".to_owned(),
        DEFAULT_PERM_MEMBER_MANAGE.to_owned(),
    );
    form.insert(
        "intelligence".to_owned(),
        params.intelligence.unwrap_or(false).to_string(),
    );
    if let Some(v) = params.description {
        form.insert("description".to_owned(), v.to_owned());
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

// `search_workspace` (`GET /workspace/{id}/storage/search/`) was removed in
// Phase 3: it duplicated `api::storage::search_files`. Both the CLI
// `workspace search` command and the MCP `workspace search` action now forward
// to `api::search::unified_search_workspace` (`/search/`, grouped buckets) so
// they share the same API/shape/semantics. See `api/search.rs`.

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

/// Create a markdown note in a workspace.
///
/// `POST /workspace/{workspace_id}/storage/{parent_id}/createnote/`
///
/// The body is **form-encoded** (not JSON) per
/// `~/vividengine/llms/storage.txt:534-561`. Both `name` (must end in `.md`)
/// and `content` (≤100 KB markdown) are **required** by the server.
pub async fn create_note(
    client: &ApiClient,
    workspace_id: &str,
    parent_id: &str,
    name: &str,
    content: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    form.insert("content".to_owned(), content.to_owned());
    client
        .post(&note_path(workspace_id, parent_id, "createnote"), &form)
        .await
}

/// Build a workspace note endpoint path (`createnote` / `updatenote` /
/// `readnote`). Path params are URL-encoded.
fn note_path(workspace_id: &str, node_id: &str, action: &str) -> String {
    format!(
        "/workspace/{}/storage/{}/{action}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    )
}

/// Build the form body for `updatenote/`: `name`/`content` (at least one) plus
/// the optional `if_version_id` CAS precondition. Extracted so the field set
/// is unit-testable without an HTTP client.
fn build_update_note_form(
    name: Option<&str>,
    content: Option<&str>,
    if_version_id: Option<&str>,
) -> HashMap<String, String> {
    let mut form = HashMap::new();
    if let Some(n) = name {
        form.insert("name".to_owned(), n.to_owned());
    }
    if let Some(c) = content {
        form.insert("content".to_owned(), c.to_owned());
    }
    if let Some(v) = if_version_id {
        form.insert("if_version_id".to_owned(), v.to_owned());
    }
    form
}

/// Update a markdown note in a workspace.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/updatenote/`
///
/// The body is **form-encoded** per `~/vividengine/llms/storage.txt:594-626`.
/// At least one of `name`/`content` must be supplied. When `if_version_id` is
/// passed it is a compare-and-swap precondition: the update only proceeds if
/// the note's current version matches, otherwise the server returns
/// `409 Conflict` (code `1660`) with the current state under
/// `error.params.current`.
pub async fn update_note(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    name: Option<&str>,
    content: Option<&str>,
    if_version_id: Option<&str>,
) -> Result<Value, CliError> {
    let form = build_update_note_form(name, content, if_version_id);
    client
        .post(&note_path(workspace_id, node_id, "updatenote"), &form)
        .await
}

/// Read a note's content as JSON.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/readnote/`
///
/// Returns the structured `{result, content, note}` envelope per
/// `~/vividengine/llms/storage.txt:684-740`: `content` is the sanitized
/// markdown string and `note` is the full node resource. An optional
/// `version_id` reads a specific version.
pub async fn read_note(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    version_id: Option<&str>,
) -> Result<Value, CliError> {
    let path = note_path(workspace_id, node_id, "readnote");
    read_note_at(client, &path, version_id).await
}

/// Issue the `readnote/` GET at `path`, threading the optional `version_id`
/// query parameter. Shared by the workspace and share read paths.
async fn read_note_at(
    client: &ApiClient,
    path: &str,
    version_id: Option<&str>,
) -> Result<Value, CliError> {
    if let Some(v) = version_id {
        let mut params = HashMap::new();
        params.insert("version_id".to_owned(), v.to_owned());
        client.get_with_params(path, &params).await
    } else {
        client.get(path).await
    }
}

/// Read a note's content as JSON from a **share**.
///
/// `GET /share/{share_id}/storage/{node_id}/readnote/`
///
/// Share-scoped sibling of [`read_note`] (`storage.txt:685`). Used by the
/// deferred `fastio view share` surface; available now so the share path is
/// not re-implemented later.
pub async fn read_note_share(
    client: &ApiClient,
    share_id: &str,
    node_id: &str,
    version_id: Option<&str>,
) -> Result<Value, CliError> {
    let path = format!(
        "/share/{}/storage/{}/readnote/",
        urlencoding::encode(share_id),
        urlencoding::encode(node_id),
    );
    read_note_at(client, &path, version_id).await
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

/// How a [`metadata_api`] call is dispatched onto the HTTP client, decided
/// purely from `(method, has_form, has_body, has_params)`.
///
/// Extracted as a pure enum so the contract-driven encoding decision (form vs
/// JSON on POST; query-param forwarding on DELETE) can be unit-tested without
/// a live HTTP client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataRequestKind {
    /// `GET` with no query parameters.
    Get,
    /// `GET` forwarding `params` as a query string.
    GetWithParams,
    /// `POST` with a form-encoded (`application/x-www-form-urlencoded`) body.
    PostForm,
    /// `POST` with a JSON body (`body`, or an empty object when absent).
    PostJson,
    /// `DELETE` with no query parameters (deliberate delete-all for the
    /// node-metadata endpoint).
    Delete,
    /// `DELETE` forwarding `params` as a query string (e.g. `keys`,
    /// `template_id`).
    DeleteWithParams,
}

/// Decide how a metadata request is dispatched.
///
/// Encoding rules (driven by the metadata contract in `ai.txt` /
/// `storage.txt`):
///
/// - **`POST`** is **form-encoded** when a `form` is supplied — every metadata
///   mutation endpoint (`templates/.../settings/`, `templates/.../update/`,
///   `storage/{n}/metadata/update/{tid}/`, `metadata/view/`) requires
///   `application/x-www-form-urlencoded` and returns `406` for a JSON body.
///   When no `form` is supplied it falls back to a JSON body for the rare POST
///   endpoint whose contract is genuinely JSON.
/// - **`DELETE`** forwards `params` as a query string so callers can send the
///   documented query parameters (e.g. `keys`, `template_id`). Dropping them
///   previously turned a targeted metadata delete into a delete-all.
/// - Any unrecognized method falls back to a bare `GET`.
pub(crate) fn plan_metadata_request(
    method: &str,
    has_form: bool,
    has_body: bool,
    has_params: bool,
) -> MetadataRequestKind {
    match method {
        "GET" if has_params => MetadataRequestKind::GetWithParams,
        "POST" if has_form => MetadataRequestKind::PostForm,
        "POST" if has_body => MetadataRequestKind::PostJson,
        "POST" => MetadataRequestKind::PostJson,
        "DELETE" if has_params => MetadataRequestKind::DeleteWithParams,
        "DELETE" => MetadataRequestKind::Delete,
        _ => MetadataRequestKind::Get,
    }
}

/// Generic metadata API call helper.
///
/// Provides a passthrough for various metadata endpoints. The wire shape
/// (form vs JSON on POST; query-param forwarding on DELETE) is decided by
/// [`plan_metadata_request`]; see that function for the contract citations.
#[allow(clippy::implicit_hasher)]
pub async fn metadata_api(
    client: &ApiClient,
    workspace_id: &str,
    sub_path: &str,
    method: &str,
    body: Option<&Value>,
    form: Option<&HashMap<String, String>>,
    params: Option<&HashMap<String, String>>,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/{}",
        urlencoding::encode(workspace_id),
        sub_path,
    );
    // `plan_metadata_request` decides the wire shape; the `Some` payloads it
    // implies are pattern-matched here so no `unwrap`/temporary is needed.
    let empty = HashMap::new();
    match plan_metadata_request(method, form.is_some(), body.is_some(), params.is_some()) {
        MetadataRequestKind::Get => client.get(&path).await,
        MetadataRequestKind::GetWithParams => {
            client
                .get_with_params(&path, params.unwrap_or(&empty))
                .await
        }
        MetadataRequestKind::PostForm => client.post(&path, form.unwrap_or(&empty)).await,
        MetadataRequestKind::PostJson => {
            if let Some(b) = body {
                client.post_json(&path, b).await
            } else {
                client.post_json(&path, &serde_json::json!({})).await
            }
        }
        MetadataRequestKind::Delete => client.delete(&path).await,
        MetadataRequestKind::DeleteWithParams => {
            client
                .delete_with_params(&path, params.unwrap_or(&empty))
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MetadataRequestKind, build_update_note_form, note_path, plan_metadata_request};

    #[test]
    fn metadata_get_with_params_forwards_query() {
        // metadata-list / templates-in-use pass query params on GET.
        assert_eq!(
            plan_metadata_request("GET", false, false, true),
            MetadataRequestKind::GetWithParams
        );
        assert_eq!(
            plan_metadata_request("GET", false, false, false),
            MetadataRequestKind::Get
        );
    }

    #[test]
    fn metadata_post_mutations_are_form_encoded() {
        // FIX 3: settings / template-update / key_values-update / view-save
        // all build a `form`, which must dispatch as form-encoded (NOT JSON —
        // a JSON body returns 406 for the form-only mutation endpoints).
        assert_eq!(
            plan_metadata_request("POST", true, false, false),
            MetadataRequestKind::PostForm
        );
        // A form always wins over a stray JSON body.
        assert_eq!(
            plan_metadata_request("POST", true, true, false),
            MetadataRequestKind::PostForm
        );
        // No form supplied → JSON fallback (a no-body / genuinely-JSON POST
        // endpoint keeps JSON).
        assert_eq!(
            plan_metadata_request("POST", false, true, false),
            MetadataRequestKind::PostJson
        );
        assert_eq!(
            plan_metadata_request("POST", false, false, false),
            MetadataRequestKind::PostJson
        );
    }

    #[test]
    fn metadata_delete_forwards_keys_query() {
        // FIX 1: when `keys` is supplied it rides as a query parameter on the
        // DELETE so a targeted delete stays targeted.
        assert_eq!(
            plan_metadata_request("DELETE", false, false, true),
            MetadataRequestKind::DeleteWithParams
        );
        // Omitting `keys` is a DELIBERATE delete-all (ai.txt:2600-2612): the
        // DELETE carries no query parameters and the server removes all keys.
        assert_eq!(
            plan_metadata_request("DELETE", false, false, false),
            MetadataRequestKind::Delete
        );
    }

    #[test]
    fn metadata_unknown_method_falls_back_to_get() {
        assert_eq!(
            plan_metadata_request("PATCH", true, true, true),
            MetadataRequestKind::Get
        );
    }

    #[test]
    fn note_paths_use_correct_endpoints() {
        // The retired `notes/` and `notes/update/` paths must NOT appear; the
        // correct endpoints are `createnote/`, `updatenote/`, `readnote/`.
        assert_eq!(
            note_path("123", "root", "createnote"),
            "/workspace/123/storage/root/createnote/"
        );
        assert_eq!(
            note_path("123", "n1", "updatenote"),
            "/workspace/123/storage/n1/updatenote/"
        );
        assert_eq!(
            note_path("123", "n1", "readnote"),
            "/workspace/123/storage/n1/readnote/"
        );
    }

    #[test]
    fn note_path_url_encodes_params() {
        // A node id with a slash/space must be percent-encoded so it can't
        // break out of the path segment.
        let p = note_path("ws id", "a/b", "readnote");
        assert!(p.contains("ws%20id"), "{p}");
        assert!(p.contains("a%2Fb"), "{p}");
    }

    #[test]
    fn update_note_form_carries_if_version_id() {
        let form = build_update_note_form(Some("x.md"), Some("body"), Some("v9"));
        assert_eq!(form.get("name").map(String::as_str), Some("x.md"));
        assert_eq!(form.get("content").map(String::as_str), Some("body"));
        assert_eq!(form.get("if_version_id").map(String::as_str), Some("v9"));
    }

    #[test]
    fn update_note_form_omits_unset_fields() {
        let form = build_update_note_form(None, Some("only content"), None);
        assert!(!form.contains_key("name"));
        assert!(!form.contains_key("if_version_id"));
        assert_eq!(
            form.get("content").map(String::as_str),
            Some("only content")
        );
    }
}
