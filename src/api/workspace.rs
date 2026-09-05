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
///
/// `archived` is documented on the **org-scoped** route only (see the published
/// API docs), where it defaults to `"false"` — so an org listing hides archived
/// workspaces unless the filter is sent.
/// `/workspaces/all/` declares no request parameters at all; it returns every
/// workspace with an `archived` flag on each item instead of filtering, so the
/// filter is deliberately NOT sent there rather than sent and swallowed.
pub async fn list_workspaces(
    client: &ApiClient,
    org_id: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
    archived: Option<bool>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = if let Some(oid) = org_id {
        if let Some(a) = archived {
            params.insert("archived".to_owned(), a.to_string());
        }
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
    /// Automatic metadata extraction for newly uploaded files.
    ///
    /// Like `intelligence` it **defaults to `true`** server-side, so `None`
    /// omits the field rather than sending `false`. It is an opt-OUT layered
    /// under `intelligence` and the plan: automatic extraction runs only while
    /// `intelligence` is on, the plan includes the `metadata` feature, and this
    /// is not `false`. It can withhold extraction; it can never enable it where
    /// the intelligence setting or the plan does not allow it.
    pub metadata_extraction: Option<bool>,
}

/// Default join permission when the `workspace create` command does not expose
/// a flag for it (the server has no default and hard-requires the field).
const DEFAULT_PERM_JOIN: &str = "Member or above";
/// Default member-management permission (see [`DEFAULT_PERM_JOIN`]).
const DEFAULT_PERM_MEMBER_MANAGE: &str = "Admin or above";

/// Create a workspace in an organization.
///
/// `POST /org/{org_id}/create/workspace/` — `org_id` is the URL path part.
///
/// `folder_name`, `name`, `perm_join` and `perm_member_manage` are hard-required
/// (see the published API docs), so this builder always sends the two perms
/// defaulted, since the `workspace create` command exposes no flags for them.
///
/// `intelligence` is **NOT** required and **defaults to `true`** (the published
/// API docs: "omitting the field means on, and is not an error"). Sending
/// `unwrap_or(false)` unconditionally would give every workspace created through
/// the CLI *or* MCP AI indexing OFF unless the caller opted in — the inverse of
/// the platform default. Undoing that later is not free: re-enabling re-indexes
/// every file and burns AI credits. It is therefore sent ONLY when the caller
/// states a preference.
///
/// The fully-flagged variant is [`crate::api::org::create_workspace`].
/// Build the form body for the minimal create-workspace path.
///
/// Extracted for the same reason as [`crate::api::org::build_create_workspace_form`]:
/// the hazard is an unconditional `intelligence=false`, and only an assertion
/// about the field's ABSENCE can catch it.
fn build_create_workspace_form(params: &CreateWorkspaceParams<'_>) -> HashMap<String, String> {
    let mut form = HashMap::new();
    form.insert("folder_name".to_owned(), params.folder_name.to_owned());
    form.insert("name".to_owned(), params.name.to_owned());
    form.insert("perm_join".to_owned(), DEFAULT_PERM_JOIN.to_owned());
    form.insert(
        "perm_member_manage".to_owned(),
        DEFAULT_PERM_MEMBER_MANAGE.to_owned(),
    );
    if let Some(v) = params.intelligence {
        form.insert("intelligence".to_owned(), v.to_string());
    }
    if let Some(v) = params.metadata_extraction {
        form.insert("metadata_extraction".to_owned(), v.to_string());
    }
    if let Some(v) = params.description {
        form.insert("description".to_owned(), v.to_owned());
    }
    form
}

/// Create a workspace in an organization (minimal flag set).
///
/// See [`build_create_workspace_form`] for what is sent. The fully-flagged
/// variant is [`crate::api::org::create_workspace`].
pub async fn create_workspace(
    client: &ApiClient,
    params: &CreateWorkspaceParams<'_>,
) -> Result<Value, CliError> {
    let form = build_create_workspace_form(params);
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
///
/// Takes the form fields verbatim, so every documented update key travels
/// through here — including `metadata_extraction` (`"true"`/`"false"`), the
/// automatic-extraction toggle whose create-time twin is
/// [`CreateWorkspaceParams::metadata_extraction`]. Unlike `intelligence` it
/// deletes nothing, is not rate-limited, and carries no plan requirement.
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

// `search_workspace` (`GET /workspace/{id}/storage/search/`) was removed
// because it duplicated `api::storage::search_files`. Both the CLI
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
    archived: Option<bool>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    // Per the published API docs this defaults to `"false"`, so
    // omitting keeps today's behaviour and archived shares stay hidden unless
    // asked for.
    if let Some(a) = archived {
        params.insert("archived".to_owned(), a.to_string());
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
/// The body is **form-encoded** (not JSON) per the published API docs. Both
/// `name` (must end in `.md`) and `content` (≤100 KB markdown) are **required**
/// by the server.
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
/// The body is **form-encoded** per the published API docs.
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
/// Returns the structured `{result, content, note}` envelope per the published
/// API docs: `content` is the sanitized
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
/// Share-scoped sibling of [`read_note`]. Used by the
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

/// Build the `/workspace/{id}/cloud-import/{action}/` path.
///
/// The segment is **`cloud-import`**, not `import`: `/import/enable/` and
/// `/import/disable/` are paths the platform does not serve (the published API
/// docs carry a concrete curl example of the correct form).
///
/// Extracted as a builder purely so the literal is pinnable — see
/// `cloud_import_path_uses_the_cloud_import_segment`.
fn cloud_import_path(workspace_id: &str, action: &str) -> String {
    format!(
        "/workspace/{}/cloud-import/{action}/",
        urlencoding::encode(workspace_id)
    )
}

/// Enable cloud sync (cloud import) on a workspace.
///
/// `POST /workspace/{workspace_id}/cloud-import/enable/`
pub async fn enable_import(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = cloud_import_path(workspace_id, "enable");
    client.post_json(&path, &serde_json::json!({})).await
}

/// Disable cloud sync (cloud import) on a workspace.
///
/// `POST /workspace/{workspace_id}/cloud-import/disable/`
pub async fn disable_import(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = cloud_import_path(workspace_id, "disable");
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
/// - `jobs.template_match` — array the server may still emit. It belonged to
///   the retired `auto-match` surface; this client no longer produces such jobs
///   and no caller here should read it.
///
/// Each entry in `metadata_extract` carries a `kind` discriminator: `"single"`
/// for per-node jobs, and `"batch"` for the retired template-wide `extract-all`
/// runs (a historical response shape this client no longer enqueues).
///
/// **Correlate a `"single"` entry on `node_id`, NEVER on `template_id`.**
/// `template_id` is server-populated, so the client's copy and the job's copy
/// come from different places and matching on it cannot work.
///
/// **The `job_id`/`template_id` nullity below is scoped to entries THIS
/// CLIENT enqueued** via [`crate::api::metadata::extract_node_metadata`]: for
/// those, `template_id` is always `null`, and `job_id` is `null` in flight and
/// carries the enqueue response's exact id only once terminal — the stronger
/// key at the end, unusable during the run. **`kind: "single"` alone does NOT
/// establish that provenance.** This endpoint is workspace-wide and can return
/// per-node entries produced by other surfaces, which are not bound by either
/// statement. Do not read these as properties of every `"single"` entry.
///
/// **`node_id` identifies the NODE, not YOUR REQUEST.** The per-node progress
/// slot is keyed by `(instance, node)`, so overlapping extractions of the same
/// node overwrite one another: read an entry as *"the latest extraction state
/// for this node"*, never as *"the state of the run I just started"*.
///
/// The only `status` values a caller should ACT on are the terminal ones —
/// `"completed"` and `"errored"` (on `"errored"`, surface `error_message`).
/// Non-terminal values are the server's to define and change: `"in_progress"`
/// stopped occurring when the templated extract arm was retired (2026-08-27),
/// and enumerating them here would just drift. Treat anything that is not an
/// explicit terminal state as "keep polling", which is what
/// `classify_single_extract_job` already does.
///
/// Callers must poll this endpoint after enqueueing an asynchronous extraction
/// via [`crate::api::metadata::extract_node_metadata`] — the extraction
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
    /// `DELETE` forwarding `params` as a query string (e.g. `keys`).
    DeleteWithParams,
}

/// Decide how a metadata request is dispatched.
///
/// Encoding rules (driven by the metadata contract in `ai.txt` /
/// `storage.txt`):
///
/// - **`POST`** is **form-encoded** when a `form` is supplied: the metadata
///   mutation endpoints require `application/x-www-form-urlencoded` and return
///   `406` for a JSON body. When no `form` is supplied it falls back to a JSON
///   body for the rare POST endpoint whose contract is genuinely JSON.
///
///   The rule is stated without naming example routes on purpose. It used to
///   cite `templates/.../settings/`, `templates/.../update/`,
///   `storage/{n}/metadata/update/{tid}/` and `metadata/view/` — **every one of
///   which has since been retired**, so the justification outlived its
///   examples and read as though those routes were still live. The dispatch
///   logic is generic; it does not need a route list to be correct, and a
///   route list is exactly the part that rots.
/// - **`DELETE`** forwards `params` as a query string so callers can send the
///   documented query parameters — today that is `keys`. Dropping them
///   previously turned a targeted metadata delete into a delete-all.
///   (This named `template_id` as a second example until 2026-08-28; it is
///   retired, and a non-empty one is now refused with `406` / `179390`. Same
///   rot as the POST route list above, one bullet down — which is why the
///   surviving text names the parameter that is actually documented rather
///   than illustrating with a set.)
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
    use super::{
        CreateWorkspaceParams, MetadataRequestKind, build_create_workspace_form,
        build_update_note_form, cloud_import_path, note_path, plan_metadata_request,
    };

    /// The minimal create path must OMIT `intelligence` when unset.
    ///
    /// Same contract as `api::org`'s builder: sending `unwrap_or(false)` would
    /// make `fastio workspace create` — and every MCP workspace creation, which
    /// routes here — produce a workspace with AI indexing OFF against a
    /// platform default of ON.
    #[test]
    fn minimal_create_omits_intelligence_when_unset() {
        let params = CreateWorkspaceParams {
            org_id: "123",
            folder_name: "eng",
            name: "Engineering",
            intelligence: None,
            metadata_extraction: None,
            description: None,
        };
        let form = build_create_workspace_form(&params);
        assert!(
            !form.contains_key("intelligence"),
            "unset `intelligence` must be omitted, got: {form:?}"
        );
        // The two perms ARE required with no server default, so this path keeps
        // supplying them (see the published API docs).
        assert!(form.contains_key("perm_join"));
        assert!(form.contains_key("perm_member_manage"));

        let explicit = build_create_workspace_form(&CreateWorkspaceParams {
            intelligence: Some(false),
            metadata_extraction: None,
            ..params
        });
        assert_eq!(
            explicit.get("intelligence").map(String::as_str),
            Some("false"),
            "an EXPLICIT opt-out must still be transmitted"
        );
    }

    /// `metadata_extraction` follows `intelligence`'s opt-OUT contract on this
    /// path too: absent unless the caller stated a preference.
    #[test]
    fn minimal_create_omits_metadata_extraction_when_unset() {
        let params = CreateWorkspaceParams {
            org_id: "123",
            folder_name: "eng",
            name: "Engineering",
            intelligence: None,
            metadata_extraction: None,
            description: None,
        };
        assert!(
            !build_create_workspace_form(&params).contains_key("metadata_extraction"),
            "unset `metadata_extraction` must be omitted"
        );
        let off = build_create_workspace_form(&CreateWorkspaceParams {
            metadata_extraction: Some(false),
            ..params
        });
        assert_eq!(
            off.get("metadata_extraction").map(String::as_str),
            Some("false"),
            "an EXPLICIT opt-out must still be transmitted"
        );
    }

    /// THE REGRESSION THIS PIN EXISTS FOR.
    ///
    /// Shipped as `/workspace/{id}/import/enable/` until 2026-08-31 — a route
    /// the platform does not serve. Confirmed against the docs with controls
    /// both ways: a bare `import/enable` endpoint occurs **0** times across
    /// `llms/*.txt` + `llms-full.txt`, while `cloud-import/enable` occurs 7.
    ///
    /// Asserting on the FULL string, not `contains("cloud-import")` — a
    /// substring check passes on `/import/enable/` too, since `cloud-import`
    /// was never the thing present. The literal is the only honest assertion.
    #[test]
    fn cloud_import_path_uses_the_cloud_import_segment() {
        assert_eq!(
            cloud_import_path("4687730903718774523", "enable"),
            "/workspace/4687730903718774523/cloud-import/enable/"
        );
        assert_eq!(
            cloud_import_path("4687730903718774523", "disable"),
            "/workspace/4687730903718774523/cloud-import/disable/"
        );
    }

    /// The workspace id is a path segment and must be encoded like every other
    /// one in this module — a caller passing a slash must not escape the route.
    #[test]
    fn cloud_import_path_encodes_the_workspace_id() {
        assert_eq!(
            cloud_import_path("a/b", "enable"),
            "/workspace/a%2Fb/cloud-import/enable/"
        );
    }

    #[test]
    fn metadata_get_with_params_forwards_query() {
        // A GET carrying query params dispatches with them; without, plain.
        // (Named `metadata-list` / `templates-in-use` until 2026-08-28 — both
        // retired. The dispatch rule under test is generic and unchanged.)
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
        // A POST that builds a `form` must dispatch form-encoded, NOT JSON —
        // a JSON body returns 406 on the form-only mutation endpoints.
        // (This listed settings / template-update / key_values-update /
        // view-save as the callers until 2026-08-28; all retired. The rule is
        // about the presence of a `form`, not about which route sends it.)
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
        // When `keys` is supplied it rides as a query parameter on the
        // DELETE so a targeted delete stays targeted.
        assert_eq!(
            plan_metadata_request("DELETE", false, false, true),
            MetadataRequestKind::DeleteWithParams
        );
        // Omitting `keys` is a DELIBERATE delete-all (see the published API docs): the
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
