#![allow(clippy::missing_errors_doc)]

/// System health and status API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for health checks and system status.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Perform a health check against the API.
///
/// `GET /ping/`
///
/// This endpoint does not require authentication.
pub async fn ping(client: &ApiClient) -> Result<Value, CliError> {
    let params = HashMap::new();
    client.get_no_auth_with_params("/ping/", &params).await
}

/// Get system status from the API.
///
/// `GET /system/status/`
///
/// This endpoint does not require authentication.
pub async fn system_status(client: &ApiClient) -> Result<Value, CliError> {
    let params = HashMap::new();
    client
        .get_no_auth_with_params("/system/status/", &params)
        .await
}
