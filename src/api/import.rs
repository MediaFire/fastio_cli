#![allow(clippy::missing_errors_doc)]

/// Cloud import API endpoints for the Fast.io REST API.
///
/// Manages external cloud storage provider integrations: identities,
/// sources, sync jobs, and write-back operations.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List available cloud import providers.
///
/// `GET /imports/workspace/{workspace_id}/providers/`
pub async fn list_providers(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/imports/workspace/{}/providers/",
        urlencoding::encode(workspace_id),
    );
    client.get(&path).await
}

/// List provider identities for a workspace.
///
/// `GET /imports/workspace/{workspace_id}/identities/`
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
        "/imports/workspace/{}/identities/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Provision a new provider identity.
///
/// `POST /imports/workspace/{workspace_id}/identities/provision/`
pub async fn provision_identity(
    client: &ApiClient,
    workspace_id: &str,
    provider: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "provider": provider });
    let path = format!(
        "/imports/workspace/{}/identities/provision/",
        urlencoding::encode(workspace_id),
    );
    client.post_json(&path, &body).await
}

/// Get identity details.
///
/// `GET /imports/workspace/{workspace_id}/identities/{identity_id}/`
pub async fn identity_details(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/workspace/{}/identities/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(identity_id),
    );
    client.get(&path).await
}

/// Revoke a provider identity.
///
/// `POST /imports/workspace/{workspace_id}/identities/{identity_id}/revoke/`
pub async fn revoke_identity(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/workspace/{}/identities/{}/revoke/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(identity_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List import sources for a workspace.
///
/// `GET /imports/workspace/{workspace_id}/sources/`
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
        "/imports/workspace/{}/sources/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Discover shared folders for a provider identity.
///
/// `POST /imports/workspace/{workspace_id}/sources/discover/`
pub async fn discover(
    client: &ApiClient,
    workspace_id: &str,
    identity_id: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "identity_id": identity_id });
    let path = format!(
        "/imports/workspace/{}/sources/discover/",
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
    /// Access level: `read` or `readwrite`.
    pub access_mode: Option<&'a str>,
}

/// Create an import source.
///
/// `POST /imports/workspace/{workspace_id}/sources/create/`
pub async fn create_source(
    client: &ApiClient,
    params: &CreateSourceParams<'_>,
) -> Result<Value, CliError> {
    let mut body = serde_json::json!({
        "identity_id": params.identity_id,
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
    let path = format!(
        "/imports/workspace/{}/sources/create/",
        urlencoding::encode(params.workspace_id),
    );
    client.post_json(&path, &body).await
}

/// Get import source details.
///
/// `GET /imports/details/{source_id}/`
pub async fn source_details(client: &ApiClient, source_id: &str) -> Result<Value, CliError> {
    let path = format!("/imports/details/{}/", urlencoding::encode(source_id),);
    client.get(&path).await
}

/// Update source settings.
///
/// `POST /imports/details/{source_id}/update/`
pub async fn update_source(
    client: &ApiClient,
    source_id: &str,
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
    if status == Some("paused") {
        body.insert("action".to_owned(), Value::String("pause".to_owned()));
    } else if status == Some("synced") {
        body.insert("action".to_owned(), Value::String("resume".to_owned()));
    }
    let path = format!(
        "/imports/details/{}/update/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &Value::Object(body)).await
}

/// Delete an import source.
///
/// `POST /imports/details/{source_id}/delete/`
pub async fn delete_source(client: &ApiClient, source_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/delete/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Disconnect an import source.
///
/// `POST /imports/details/{source_id}/disconnect/`
pub async fn disconnect_source(
    client: &ApiClient,
    source_id: &str,
    action: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "action": action });
    let path = format!(
        "/imports/details/{}/disconnect/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &body).await
}

/// Trigger immediate refresh sync.
///
/// `POST /imports/details/{source_id}/refresh/`
pub async fn refresh_source(client: &ApiClient, source_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/refresh/",
        urlencoding::encode(source_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List jobs for a source.
///
/// `GET /imports/details/{source_id}/jobs/`
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
    let path = format!("/imports/details/{}/jobs/", urlencoding::encode(source_id),);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get job details.
///
/// `GET /imports/details/{source_id}/jobs/{job_id}/`
pub async fn job_details(
    client: &ApiClient,
    source_id: &str,
    job_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/jobs/{}/",
        urlencoding::encode(source_id),
        urlencoding::encode(job_id),
    );
    client.get(&path).await
}

/// Cancel a running job.
///
/// `POST /imports/details/{source_id}/jobs/{job_id}/cancel/`
pub async fn cancel_job(
    client: &ApiClient,
    source_id: &str,
    job_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/jobs/{}/cancel/",
        urlencoding::encode(source_id),
        urlencoding::encode(job_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// List write-back jobs for a source.
///
/// `GET /imports/details/{source_id}/writebacks/`
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
    let path = format!(
        "/imports/details/{}/writebacks/",
        urlencoding::encode(source_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get write-back job details.
///
/// `GET /imports/details/{source_id}/writebacks/{writeback_id}/`
pub async fn writeback_details(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/writebacks/{}/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.get(&path).await
}

/// Push a file to remote storage.
///
/// `POST /imports/details/{source_id}/writebacks/push/{node_id}/`
pub async fn push_writeback(
    client: &ApiClient,
    source_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/writebacks/push/{}/",
        urlencoding::encode(source_id),
        urlencoding::encode(node_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Retry a failed write-back.
///
/// `POST /imports/details/{source_id}/writebacks/{writeback_id}/retry/`
pub async fn retry_writeback(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/writebacks/{}/retry/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Resolve a write-back conflict.
///
/// `POST /imports/details/{source_id}/writebacks/{writeback_id}/resolve/`
pub async fn resolve_conflict(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
    resolution: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "resolution": resolution });
    let path = format!(
        "/imports/details/{}/writebacks/{}/resolve/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.post_json(&path, &body).await
}

/// Cancel a pending write-back.
///
/// `POST /imports/details/{source_id}/writebacks/{writeback_id}/cancel/`
pub async fn cancel_writeback(
    client: &ApiClient,
    source_id: &str,
    writeback_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/imports/details/{}/writebacks/{}/cancel/",
        urlencoding::encode(source_id),
        urlencoding::encode(writeback_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}
