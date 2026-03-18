#![allow(clippy::missing_errors_doc)]

/// Apps API endpoints for the Fast.io REST API.
///
/// Provides app/widget discovery, metadata, and launch operations.
use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List all available apps/widgets.
///
/// `GET /apps/list/`
pub async fn list_apps(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/apps/list/").await
}

/// Get details for a specific app.
///
/// `GET /apps/{app_id}/details/`
pub async fn app_details(client: &ApiClient, app_id: &str) -> Result<Value, CliError> {
    let path = format!("/apps/{}/details/", urlencoding::encode(app_id));
    client.get(&path).await
}

/// Launch an app (get widget HTML content).
///
/// `POST /apps/{app_id}/launch/`
pub async fn launch_app(
    client: &ApiClient,
    app_id: &str,
    context_type: &str,
    context_id: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({
        "context_type": context_type,
        "context_id": context_id,
    });
    let path = format!("/apps/{}/launch/", urlencoding::encode(app_id));
    client.post_json(&path, &body).await
}

/// List apps available for a specific tool.
///
/// `GET /apps/tool/{tool_name}/`
pub async fn get_tool_apps(client: &ApiClient, tool_name: &str) -> Result<Value, CliError> {
    let path = format!("/apps/tool/{}/", urlencoding::encode(tool_name));
    client.get(&path).await
}
