#![allow(clippy::missing_errors_doc)]

/// File locking API endpoints for the Fast.io REST API.
///
/// Acquire, check, and release exclusive locks on files in workspaces or shares.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Acquire an exclusive lock on a file.
///
/// `POST /{context_type}/{context_id}/storage/{node_id}/lock/` — optional
/// `duration` (60-3600 seconds) sets the lock lifetime and `client_info`
/// (a JSON object, e.g. `{"device_name":"…","client_version":"…"}`) records
/// client metadata.
pub async fn lock_acquire(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    duration: Option<u32>,
    client_info: Option<&str>,
) -> Result<Value, CliError> {
    let form = crate::api::storage::lock_acquire_form(duration, client_info);
    let path = format!(
        "/{}/{}/storage/{}/lock/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Check lock status for a file.
///
/// `GET /{context_type}/{context_id}/storage/{node_id}/lock/`
pub async fn lock_status(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/lock/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Renew (heartbeat) an existing lock on a file.
///
/// `POST /{context_type}/{context_id}/storage/{node_id}/lock/heartbeat/`
///
/// Extends the lock's expiry timer. The `lock_token` is the token returned
/// by `lock_acquire` and must be provided to prove ownership of the lock.
pub async fn lock_heartbeat(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    lock_token: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("lock_token".to_owned(), lock_token.to_owned());
    let path = format!(
        "/{}/{}/storage/{}/lock/heartbeat/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Release a lock on a file.
///
/// `DELETE /{context_type}/{context_id}/storage/{node_id}/lock/`
///
/// The `lock_token` is the token returned by `lock_acquire` and must be
/// provided to prove ownership of the lock.
pub async fn lock_release(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    lock_token: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("lock_token".to_owned(), lock_token.to_owned());
    let path = format!(
        "/{}/{}/storage/{}/lock/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.delete_with_form(&path, &form).await
}
