#![allow(clippy::missing_errors_doc)]

/// Cloud import API endpoints for the Fast.io REST API.
///
/// Manages external cloud storage provider integrations: identities,
/// sources, sync jobs, and write-back operations.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// The only `status` values [`update_source`] accepts.
///
/// They map to the server's `pause` and `resume` actions; no other status is
/// settable through that endpoint. Rejecting the rest here rather than dropping
/// them keeps every caller — CLI, MCP, and library — from sending a request that
/// applies a DIFFERENT change than the one asked for.
pub const UPDATE_SOURCE_STATUS_VALUES: &[&str] = &["paused", "synced"];

/// List available cloud import providers.
///
/// `GET /cloudsync/workspace/{workspace_id}/providers/`
pub async fn list_providers(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/workspace/{}/providers/",
        urlencoding::encode(workspace_id),
    );
    client.get(&path).await
}

/// List provider identities for a workspace.
///
/// `GET /cloudsync/workspace/{workspace_id}/identities/`
pub async fn list_identities(
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
        "/cloudsync/workspace/{}/identities/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Build the request body for [`provision_identity`].
///
/// `account_type` is sent ONLY when supplied, so the default request is
/// byte-identical to one built before the parameter existed. The server
/// defaults it to `work`, which is why a personal Microsoft account cannot
/// connect unless the caller asks for `personal` explicitly.
fn build_provision_body(provider: &str, account_type: Option<&str>) -> Value {
    let mut body = serde_json::json!({ "provider": provider });
    if let Some(v) = account_type {
        body["account_type"] = Value::String(v.to_owned());
    }
    body
}

/// Provision a new provider identity.
///
/// `account_type` applies to `onedrive_business` only and selects which
/// Microsoft account family the consent screen targets: `work` (the server
/// default) or `personal`. Sending it for another provider is accepted and
/// ignored; sending anything other than those two values is rejected with
/// the server message naming the two permitted values.
///
/// `POST /cloudsync/workspace/{workspace_id}/identities/provision/`
pub async fn provision_identity(
    client: &ApiClient,
    workspace_id: &str,
    provider: &str,
    account_type: Option<&str>,
) -> Result<Value, CliError> {
    let body = build_provision_body(provider, account_type);
    let path = format!(
        "/cloudsync/workspace/{}/identities/provision/",
        urlencoding::encode(workspace_id),
    );
    client.post_json(&path, &body).await
}

/// Get identity details.
///
/// `GET /cloudsync/workspace/{workspace_id}/identities/{identity_id}/`
pub async fn identity_details(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/workspace/{}/identities/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(identity_id),
    );
    client.get(&path).await
}

/// Revoke a provider identity.
///
/// `POST /cloudsync/workspace/{workspace_id}/identities/{identity_id}/revoke/`
pub async fn revoke_identity(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/workspace/{}/identities/{}/revoke/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(identity_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List import sources for a workspace.
///
/// `GET /cloudsync/workspace/{workspace_id}/sources/`
pub async fn list_sources(
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
        "/cloudsync/workspace/{}/sources/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// List the stored drives (document libraries) an identity can reach.
///
/// **Reads stored rows only — it never enumerates.** Building or rebuilding the
/// catalog is [`refresh_drives`]'s job, because enumerating would put a provider
/// secret read and a Graph round-trip on a request path.
///
/// `site_path` here is a **filter over already-stored rows**, not an
/// enumeration hint: passing one when the catalog is empty cannot populate it.
/// The site path that resolves a `requires_site_path` state goes to
/// [`refresh_drives`] instead.
///
/// An empty `drives` array is the normal first-connect state and means nothing
/// on its own — `drives_state` says *which* empty it is (one of
/// `never_refreshed`, `refreshing`, `ready`, `empty`, `requires_site_path`,
/// `permission_denied`, `failed`). The response also carries
/// `drives_refreshed_at`, `drives_error` (non-null on a `ready` catalog means
/// truncation, i.e. usable but incomplete) and a `pagination` block.
///
/// `GET /cloudsync/workspace/{workspace_id}/identities/{identity_id}/drives/`
pub async fn list_drives(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
    site_path: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/workspace/{}/identities/{}/drives/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(identity_id),
    );
    let mut params = HashMap::new();
    if let Some(v) = site_path {
        params.insert("site_path".to_owned(), v.to_owned());
    }
    if let Some(v) = limit {
        params.insert("limit".to_owned(), v.to_string());
    }
    if let Some(v) = offset {
        params.insert("offset".to_owned(), v.to_string());
    }
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Build the request body for [`refresh_drives`].
fn build_refresh_drives_body(site_path: Option<&str>) -> Value {
    let mut body = serde_json::json!({});
    if let Some(v) = site_path {
        body["site_path"] = Value::String(v.to_owned());
    }
    body
}

/// Rebuild an identity's drive catalog from the provider.
///
/// Asynchronous, and it returns **no job id**: it stamps the identity
/// `drives_state = refreshing` and enqueues the refresh job, so poll
/// [`list_drives`] until `drives_state` leaves `refreshing`.
///
/// Pass `site_path` when [`list_drives`] reports `requires_site_path` — a
/// tenant whose consent model will not enumerate sites needs the site named
/// here, at refresh time. This is the only call that talks to the provider.
///
/// Returns an error when a refresh is already in flight for the identity; the
/// endpoint deliberately refuses to restart one.
///
/// `POST /cloudsync/workspace/{workspace_id}/identities/{identity_id}/drives/refresh/`
pub async fn refresh_drives(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
    site_path: Option<&str>,
) -> Result<Value, CliError> {
    let body = build_refresh_drives_body(site_path);
    let path = format!(
        "/cloudsync/workspace/{}/identities/{}/drives/refresh/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(identity_id),
    );
    client.post_json(&path, &body).await
}

/// Build the request body for [`discover`].
///
/// The identity is carried as `provider_identity_id` — the server reads that
/// key and hard-fails `provider_identity_id is required` on anything else, so
/// the name is pinned by [`tests::discover_body_uses_provider_identity_id`].
///
/// `drive_id` scopes which library is enumerated. Without it a `OneDrive`
/// identity that reaches several libraries enumerates the default one, so a
/// caller can discover a path in one library and bind a source to another.
///
/// `remote_path` scopes enumeration to a SUBFOLDER. Absent or empty enumerates
/// the provider root, which is the default and the historical behaviour. The
/// server returns ONE LEVEL per call by design — it will not walk a tree whose
/// size it cannot bound — so drilling down is the caller's job: discover the
/// root, pick a folder, discover again with that folder's `remote_path`.
fn build_discover_body(
    identity_id: &str,
    drive_id: Option<&str>,
    remote_path: Option<&str>,
) -> Value {
    let mut body = serde_json::json!({ "provider_identity_id": identity_id });
    if let Some(v) = drive_id {
        body["drive_id"] = Value::String(v.to_owned());
    }
    if let Some(v) = remote_path {
        body["remote_path"] = Value::String(v.to_owned());
    }
    body
}

/// Discover shared folders for a provider identity.
///
/// Discovery is asynchronous: the response carries a `job_id` rather than the
/// folder list. Poll it with [`job_details`] using the literal string
/// `discovery` as the source id (a discovery job has no import source), i.e.
/// `GET /cloudsync/details/discovery/jobs/{job_id}/`.
///
/// Pass `drive_id` on `OneDrive` Business so discovery enumerates the library the
/// source will be bound to; see [`list_drives`].
///
/// `POST /cloudsync/workspace/{workspace_id}/sources/discover/`
pub async fn discover(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
    drive_id: Option<&str>,
    remote_path: Option<&str>,
) -> Result<Value, CliError> {
    let body = build_discover_body(identity_id, drive_id, remote_path);
    let path = format!(
        "/cloudsync/workspace/{}/sources/discover/",
        urlencoding::encode(workspace_id),
    );
    client.post_json(&path, &body).await
}

/// Parameters for [`create_source`].
pub struct CreateSourceParams<'a> {
    /// Target workspace for the import source.
    pub workspace_id: &'a str,
    /// Cloud-storage identity (credential) to use.
    pub identity_id: &'a str,
    /// Path on the remote storage provider.
    pub remote_path: &'a str,
    /// Display name for the remote source.
    pub remote_name: Option<&'a str>,
    /// Polling interval in seconds for sync updates.
    pub sync_interval: Option<u32>,
    /// Access level: `read_only` (server default) or `read_write`.
    pub access_mode: Option<&'a str>,
    /// Drive (`SharePoint` document library) to bind the source to.
    ///
    /// Required for `onedrive_business` and ignored by the other providers.
    /// Create-time only: the graft tree is walked against this library, so
    /// re-pointing a live source is a delete-and-recreate, not an update — the
    /// server rejects it on update and [`update_source`] does not carry it.
    ///
    /// The value is a raw Microsoft Graph drive id (`b!`-prefixed, ~66 chars),
    /// passed through untouched — it is not a Fast.io opaque id.
    pub drive_id: Option<&'a str>,
    /// Storage folder to graft the imported tree under.
    ///
    /// Optional; without it the import lands in the workspace's default
    /// `Imports` folder. Must be a FOLDER, must not sit inside an existing
    /// import tree, and is create-time only — the server rejects it on update
    /// (`destination_immutable`), because moving the imported folder in storage
    /// is the supported way to relocate an existing source.
    pub destination_node_id: Option<&'a str>,
}

/// Build the request body for [`create_source`].
///
/// The identity is carried as `provider_identity_id`, matching the server's
/// create route (it hard-fails `provider_identity_id is required` otherwise).
/// Pinned by [`tests::create_source_body_uses_provider_identity_id`].
fn build_create_source_body(params: &CreateSourceParams<'_>) -> Value {
    let mut body = serde_json::json!({
        "provider_identity_id": params.identity_id,
        "remote_path": params.remote_path,
    });
    if let Some(v) = params.remote_name {
        body["remote_name"] = Value::String(v.to_owned());
    }
    if let Some(v) = params.sync_interval {
        body["sync_interval"] = Value::Number(v.into());
    }
    if let Some(v) = params.access_mode {
        body["access_mode"] = Value::String(v.to_owned());
    }
    if let Some(v) = params.drive_id {
        body["drive_id"] = Value::String(v.to_owned());
    }
    if let Some(v) = params.destination_node_id {
        body["destination_node_id"] = Value::String(v.to_owned());
    }
    body
}

/// Create an import source.
///
/// `POST /cloudsync/workspace/{workspace_id}/sources/create/`
pub async fn create_source(
    client: &ApiClient,
    params: &CreateSourceParams<'_>,
) -> Result<Value, CliError> {
    let body = build_create_source_body(params);
    let path = format!(
        "/cloudsync/workspace/{}/sources/create/",
        urlencoding::encode(params.workspace_id),
    );
    client.post_json(&path, &body).await
}

/// Get import source details.
///
/// `GET /cloudsync/details/{source_id}/`
pub async fn source_details(client: &ApiClient, source_id: &str) -> Result<Value, CliError> {
    let path = format!("/cloudsync/details/{}/", urlencoding::encode(source_id),);
    client.get(&path).await
}

/// Update source settings.
///
/// `POST /cloudsync/details/{source_id}/update/`
///
/// `status` accepts only [`UPDATE_SOURCE_STATUS_VALUES`]: `paused` maps to the
/// server's `pause` action and `synced` to `resume`. Any other value is a
/// [`CliError::Parse`] rather than a silently dropped field.
///
/// Previously an unrecognized status inserted no action and the request was sent
/// anyway. **The outcome then depended on what else was in the call:** alongside
/// another updatable field it returned SUCCESS with the pause/resume silently
/// skipped; on its own it hit the server's "no valid fields to update" refusal.
/// The combined case is the damaging one — a caller who asked to rename *and*
/// pause got the rename, no pause, and no indication of the difference.
pub async fn update_source(
    client: &ApiClient,
    source_id: &str,
    sync_interval: Option<u32>,
    status: Option<&str>,
    remote_name: Option<&str>,
    access_mode: Option<&str>,
) -> Result<Value, CliError> {
    let body = build_update_source_body(sync_interval, status, remote_name, access_mode)?;
    let path = format!(
        "/cloudsync/details/{}/update/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &body).await
}

/// Build the `update-source` request body, rejecting an unsettable `status`.
///
/// Split out from [`update_source`] so the status mapping is testable without a
/// client, matching the other `build_*_body` helpers in this module.
fn build_update_source_body(
    sync_interval: Option<u32>,
    status: Option<&str>,
    remote_name: Option<&str>,
    access_mode: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::Map::new();
    if let Some(v) = sync_interval {
        body.insert("sync_interval".to_owned(), Value::Number(v.into()));
    }
    if let Some(v) = remote_name {
        body.insert("remote_name".to_owned(), Value::String(v.to_owned()));
    }
    if let Some(v) = access_mode {
        body.insert("access_mode".to_owned(), Value::String(v.to_owned()));
    }
    match status {
        None => {}
        Some("paused") => {
            body.insert("action".to_owned(), Value::String("pause".to_owned()));
        }
        Some("synced") => {
            body.insert("action".to_owned(), Value::String("resume".to_owned()));
        }
        Some(v) => {
            return Err(CliError::Parse(format!(
                "status must be one of {} (got `{v}`)",
                UPDATE_SOURCE_STATUS_VALUES.join(", "),
            )));
        }
    }
    Ok(Value::Object(body))
}

/// Delete an import source.
///
/// `POST /cloudsync/details/{source_id}/delete/`
pub async fn delete_source(client: &ApiClient, source_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/delete/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Disconnect an import source.
///
/// `POST /cloudsync/details/{source_id}/disconnect/`
pub async fn disconnect_source(
    client: &ApiClient,
    source_id: &str,
    action: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "action": action });
    let path = format!(
        "/cloudsync/details/{}/disconnect/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &body).await
}

/// Trigger immediate refresh sync.
///
/// `POST /cloudsync/details/{source_id}/refresh/`
pub async fn refresh_source(client: &ApiClient, source_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/refresh/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List jobs for a source.
///
/// `GET /cloudsync/details/{source_id}/jobs/`
pub async fn list_jobs(
    client: &ApiClient,
    source_id: &str,
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
        "/cloudsync/details/{}/jobs/",
        urlencoding::encode(source_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get job details.
///
/// `GET /cloudsync/details/{source_id}/jobs/{job_id}/`
pub async fn job_details(
    client: &ApiClient,
    source_id: &str,
    job_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/jobs/{}/",
        urlencoding::encode(source_id),
        urlencoding::encode(job_id),
    );
    client.get(&path).await
}

/// Cancel a running job.
///
/// `POST /cloudsync/details/{source_id}/jobs/{job_id}/cancel/`
pub async fn cancel_job(
    client: &ApiClient,
    source_id: &str,
    job_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/jobs/{}/cancel/",
        urlencoding::encode(source_id),
        urlencoding::encode(job_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Build the write-back listing path, sending the source id in CANONICAL form.
///
/// This endpoint is the one place we deliberately strip display hyphens before
/// sending. Measured 2026-08-22 against a source with 26 write-back
/// rows: the formatted id returned `total: 0`, the canonical id returned all
/// 26. The sibling `list-jobs` returns the same count for either form, so this
/// is specific to this route rather than a property of the id.
///
/// Canonical is the safe form in both worlds — it is what the stored value
/// looks like, so it keeps working if the route starts canonicalizing what it
/// is given.
///
/// **Only this route is treated specially, and that is deliberate — but the
/// evidence covers four of the six, not all of them.** `writeback_details` was
/// measured the same day and returns the same row for either id form, because
/// it looks a write-back up by its OWN id rather than filtering by source;
/// `retry`, `resolve` and `cancel` share that `/writebacks/{writeback_id}/…`
/// shape and inherit the reasoning.
///
/// `push_writeback` does NOT. It is `/writebacks/push/{node_id}/` and carries
/// no write-back id at all, so the server must resolve the source from the
/// path segment — the filter-shaped case. It is left alone because both id
/// forms were measured against it and returned the SAME failure (HTTP 500,
/// "Failed to create write-back job", reaching the handler either way per the
/// server run log), so that route cannot currently discriminate between the two
/// forms at all. **It is untransformed for want of a discriminating
/// measurement, not because it was shown not to need one.**
fn writebacks_path(source_id: &str) -> String {
    format!(
        "/cloudsync/details/{}/writebacks/",
        urlencoding::encode(&crate::opaque_id::canonicalize(source_id)),
    )
}

/// List write-back jobs for a source.
///
/// `GET /cloudsync/details/{source_id}/writebacks/`
pub async fn list_writebacks(
    client: &ApiClient,
    source_id: &str,
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
    let path = writebacks_path(source_id);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get write-back job details.
///
/// `GET /cloudsync/details/{source_id}/writebacks/{writeback_id}/`
pub async fn writeback_details(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/writebacks/{}/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.get(&path).await
}

/// Push a file to remote storage.
///
/// `POST /cloudsync/details/{source_id}/writebacks/push/{node_id}/`
pub async fn push_writeback(
    client: &ApiClient,
    source_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/writebacks/push/{}/",
        urlencoding::encode(source_id),
        urlencoding::encode(node_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Retry a failed write-back.
///
/// `POST /cloudsync/details/{source_id}/writebacks/{writeback_id}/retry/`
pub async fn retry_writeback(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/writebacks/{}/retry/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Resolve a write-back conflict.
///
/// `POST /cloudsync/details/{source_id}/writebacks/{writeback_id}/resolve/`
pub async fn resolve_conflict(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
    resolution: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "resolution": resolution });
    let path = format!(
        "/cloudsync/details/{}/writebacks/{}/resolve/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.post_json(&path, &body).await
}

/// Cancel a pending write-back.
///
/// `POST /cloudsync/details/{source_id}/writebacks/{writeback_id}/cancel/`
pub async fn cancel_writeback(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/cloudsync/details/{}/writebacks/{}/cancel/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

#[cfg(test)]
mod tests {
    use super::{
        CreateSourceParams, build_create_source_body, build_discover_body, build_provision_body,
        build_update_source_body, writebacks_path,
    };
    use serde_json::json;

    /// `paused`/`synced` are the CLI's spelling; the wire wants `pause`/`resume`
    /// under an `action` key. Pinning the mapping in both directions so the
    /// translation cannot be dropped: the server rejects any `action` other than
    /// `pause`/`resume` outright (`Invalid action. Use "pause" or "resume"`), so
    /// an unmapped spelling is a hard input error on the wire, not a no-op.
    #[test]
    fn update_source_body_maps_status_to_the_wire_action() {
        assert_eq!(
            build_update_source_body(None, Some("paused"), None, None).expect("paused is settable"),
            json!({ "action": "pause" }),
        );
        assert_eq!(
            build_update_source_body(None, Some("synced"), None, None).expect("synced is settable"),
            json!({ "action": "resume" }),
        );
    }

    /// A valid status must not disturb the sibling fields — this is the positive
    /// half of the fail-closed pair below, and the only test covering
    /// `access_mode` in a populated body.
    #[test]
    fn update_source_body_carries_status_alongside_every_other_field() {
        assert_eq!(
            build_update_source_body(Some(900), Some("paused"), Some("Docs"), Some("read_write"))
                .expect("paused is settable"),
            json!({
                "sync_interval": 900,
                "remote_name": "Docs",
                "access_mode": "read_write",
                "action": "pause",
            }),
        );
    }

    /// REGRESSION. An unrecognized `--status` used to fall through the mapping
    /// with no `else`: no `action` was inserted and the request went out anyway.
    /// What happened next depended on the rest of the call — **alongside another
    /// updatable field the server returned SUCCESS with the pause/resume silently
    /// skipped**, while a lone bad status hit "no valid fields to update". Only
    /// `pause` and `resume` exist server-side, so every other value must fail
    /// here rather than on the wire.
    ///
    /// The mapping test above is the positive control for this one — it proves
    /// the builder still returns `Ok` with an action, so these `Err`s are real
    /// rejections and not a builder that fails for everything. `pause`/`resume`
    /// are in the list deliberately: they are the WIRE spellings, and accepting
    /// them here would let a caller bypass the mapping.
    #[test]
    fn update_source_body_rejects_a_status_it_cannot_set() {
        for bad in [
            "disconnected",
            "error",
            "syncing",
            "suspended_plan",
            "pause",
            "resume",
            "Paused",
            " paused",
            "",
        ] {
            let err = build_update_source_body(None, Some(bad), None, None)
                .expect_err("only paused and synced are settable");
            assert!(
                err.to_string().contains("must be one of"),
                "unsettable status `{bad}` must name the accepted values, got: {err}",
            );
        }
    }

    /// FAIL CLOSED. The damaging shape of the old bug was a bad status riding
    /// along with a real field: the field applied, the pause/resume did not, and
    /// the call reported success. Rejecting must therefore discard the WHOLE
    /// body rather than quietly sending the siblings on their own.
    #[test]
    fn update_source_body_rejects_the_whole_call_not_just_the_bad_status() {
        assert!(
            build_update_source_body(Some(3600), Some("disconnected"), Some("Docs"), None).is_err(),
            "a partially-applicable update must fail closed, not apply its siblings",
        );
    }

    /// Omitting `--status` must not synthesise an action — the other fields are
    /// independently updatable and a spurious `action` would pause or resume a
    /// source the caller never asked to touch.
    #[test]
    fn update_source_body_omits_action_when_status_absent() {
        let body = build_update_source_body(Some(3600), None, Some("Docs"), None)
            .expect("no status is valid");
        assert!(body.get("action").is_none());
        assert_eq!(
            body,
            json!({ "sync_interval": 3600, "remote_name": "Docs" })
        );
    }

    /// The server's discover route reads `provider_identity_id` and hard-fails
    /// `provider_identity_id is required` on anything else — a bare
    /// `remote_path` scopes discovery to a subfolder; omitting it enumerates the
    /// provider ROOT, which is the historical default. Pinned because the
    /// omission is silent — a caller that cannot send it simply never sees
    /// nested folders and has no error to notice.
    #[test]
    fn discover_body_carries_remote_path_only_when_given() {
        assert!(
            build_discover_body("ident-1", None, None)
                .get("remote_path")
                .is_none(),
        );
        assert_eq!(
            build_discover_body("ident-1", None, Some("/Projects")),
            json!({ "provider_identity_id": "ident-1", "remote_path": "/Projects" }),
        );
    }

    /// Omitting `--account-type` must produce the body that existed BEFORE the
    /// parameter did — byte-identical, no `account_type: null`. The server
    /// defaults it to `work`, so an accidental explicit null would be a
    /// behaviour change disguised as a no-op.
    #[test]
    fn provision_body_omits_account_type_unless_given() {
        assert_eq!(
            build_provision_body("onedrive_business", None),
            json!({ "provider": "onedrive_business" }),
        );
        assert!(
            build_provision_body("onedrive_business", None)
                .get("account_type")
                .is_none(),
        );
    }

    /// A personal Microsoft account CANNOT complete a `work` consent, so this
    /// value is the only way to connect one. Pinned because dropping it would
    /// not fail any request — it would silently target `work` and be refused
    /// at Microsoft, which looks like a provider problem rather than a client
    /// one.
    #[test]
    fn provision_body_sends_account_type_when_given() {
        assert_eq!(
            build_provision_body("onedrive_business", Some("personal")),
            json!({ "provider": "onedrive_business", "account_type": "personal" }),
        );
    }

    /// `identity_id` is a 400 on every provider, not a silent no-op. Pinning
    /// the wire key so the shorter name cannot creep back in.
    #[test]
    fn discover_body_uses_provider_identity_id() {
        let body = build_discover_body("ident-1", None, None);
        assert_eq!(body, json!({ "provider_identity_id": "ident-1" }));
        assert!(
            body.get("identity_id").is_none(),
            "discover must not send the bare `identity_id` key",
        );
    }

    /// Discovery must be drive-scoped on `OneDrive`: without this, a caller can
    /// enumerate paths in one library and bind the source to another.
    #[test]
    fn discover_body_carries_drive_id_when_given() {
        let body = build_discover_body("ident-1", Some("b!raw-graph-id"), None);
        assert_eq!(
            body,
            json!({ "provider_identity_id": "ident-1", "drive_id": "b!raw-graph-id" }),
        );
    }

    /// A raw Graph id is not a Fast.io opaque id — it must reach the wire
    /// byte-for-byte, with no re-keying, normalisation or validation.
    #[test]
    fn drive_id_is_passed_through_untouched() {
        let raw = "b!X3ZQ8kZzTk-Yh8N1KqLmPQ9wRtYuIoP0aSdFgHjKlZxCvBnM1qWeRtYuIoPaSdFg";
        assert_eq!(
            build_discover_body("ident-1", Some(raw), None)["drive_id"],
            json!(raw)
        );
        let body = build_create_source_body(&CreateSourceParams {
            workspace_id: "ws-1",
            identity_id: "ident-1",
            remote_path: "/Reports",
            remote_name: None,
            sync_interval: None,
            access_mode: None,
            drive_id: Some(raw),
            destination_node_id: None,
        });
        assert_eq!(body["drive_id"], json!(raw));
    }

    /// Same contract on create: `provider_identity_id`, not `identity_id`.
    #[test]
    fn create_source_body_uses_provider_identity_id() {
        let body = build_create_source_body(&CreateSourceParams {
            workspace_id: "ws-1",
            identity_id: "ident-1",
            remote_path: "/Reports",
            remote_name: None,
            sync_interval: None,
            access_mode: None,
            drive_id: None,
            destination_node_id: None,
        });
        assert_eq!(
            body,
            json!({ "provider_identity_id": "ident-1", "remote_path": "/Reports" }),
        );
        assert!(
            body.get("identity_id").is_none(),
            "create-source must not send the bare `identity_id` key",
        );
    }

    /// Optional fields stay omitted rather than being sent as nulls, so the
    /// server applies its own defaults (`access_mode` defaults to `read_only`).
    #[test]
    fn create_source_body_omits_absent_optionals() {
        let body = build_create_source_body(&CreateSourceParams {
            workspace_id: "ws-1",
            identity_id: "ident-1",
            remote_path: "/Reports",
            remote_name: None,
            sync_interval: None,
            access_mode: None,
            drive_id: None,
            destination_node_id: None,
        });
        // POSITIVE CONTROL FIRST. Every assertion below is an ABSENCE, so a
        // body that came back empty — a builder early-return, a fixture that
        // failed to populate — would satisfy all four for a reason that has
        // nothing to do with omitting optionals. Pin what MUST be present
        // before crediting anything to what is not.
        assert_eq!(
            body,
            json!({ "provider_identity_id": "ident-1", "remote_path": "/Reports" }),
            "control: the required keys must be present, or the absences below prove nothing",
        );
        assert!(body.get("remote_name").is_none());
        assert!(body.get("drive_id").is_none());
        assert!(body.get("sync_interval").is_none());
        assert!(body.get("access_mode").is_none());
    }

    #[test]
    fn create_source_body_includes_present_optionals() {
        let body = build_create_source_body(&CreateSourceParams {
            workspace_id: "ws-1",
            identity_id: "ident-1",
            remote_path: "/Reports",
            remote_name: Some("Reports"),
            sync_interval: Some(900),
            access_mode: Some("read_write"),
            drive_id: Some("b!drive"),
            destination_node_id: None,
        });
        assert_eq!(body["remote_name"], json!("Reports"));
        assert_eq!(body["sync_interval"], json!(900));
        assert_eq!(body["access_mode"], json!("read_write"));
        assert_eq!(body["drive_id"], json!("b!drive"));
    }

    #[test]
    fn writebacks_path_strips_display_hyphens() {
        // Measured 2026-08-22: the formatted id returns `total: 0` from
        // a source holding 26 write-back rows; the canonical id returns all 26.
        let formatted = writebacks_path("argw2-322y6-dui3h-4ov3b-zdq7w-nya7");
        assert_eq!(
            formatted, "/cloudsync/details/argw2322y6dui3h4ov3bzdq7wnya7/writebacks/",
            "the formatted id must reach this route with its display hyphens stripped",
        );
    }

    #[test]
    fn writebacks_path_leaves_a_canonical_id_unchanged() {
        // Control: the transform must be idempotent, so a caller that already
        // holds the canonical form is not altered into something else.
        let canonical = writebacks_path("argw2322y6dui3h4ov3bzdq7wnya7");
        assert_eq!(
            canonical, "/cloudsync/details/argw2322y6dui3h4ov3bzdq7wnya7/writebacks/",
            "an already-canonical id must pass through untouched",
        );
        assert_eq!(
            canonical,
            writebacks_path("argw2-322y6-dui3h-4ov3b-zdq7w-nya7"),
            "both input forms must produce the identical path",
        );
    }
}
