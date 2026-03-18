#![allow(clippy::missing_errors_doc)]

/// Asset management API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for uploading and deleting brand assets
/// (logos, banners, etc.) on organizations, workspaces, shares, and users.
use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List available asset types for an entity type.
///
/// `GET /{entity_type}/assets/`
pub async fn list_asset_types(client: &ApiClient, entity_type: &str) -> Result<Value, CliError> {
    let path = format!("/{}/assets/", urlencoding::encode(entity_type),);
    client.get(&path).await
}

/// List assets on a specific entity.
///
/// `GET /{entity_type}/{entity_id}/assets/`
pub async fn list_assets(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/assets/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    client.get(&path).await
}

/// Delete an asset from an entity.
///
/// `DELETE /{entity_type}/{entity_id}/assets/{asset_name}/`
pub async fn delete_asset(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    asset_name: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/assets/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(asset_name),
    );
    client.delete(&path).await
}
