#![allow(clippy::missing_errors_doc)]

/// Metadata extraction and template management API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for metadata-eligible files, template node management,
/// AI-based file matching, batch extraction, and single-node extraction.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List files eligible for metadata extraction in a workspace.
///
/// `GET /workspace/{workspace_id}/metadata/eligible/`
pub async fn list_eligible(
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
        "/workspace/{}/metadata/eligible/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add files to a metadata template.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/nodes/add/`
pub async fn add_nodes_to_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/nodes/add/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let mut form = HashMap::new();
    form.insert("node_ids".to_owned(), node_ids.to_owned());
    client.post(&path, &form).await
}

/// Remove files from a metadata template.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/nodes/remove/`
pub async fn remove_nodes_from_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/nodes/remove/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let mut form = HashMap::new();
    form.insert("node_ids".to_owned(), node_ids.to_owned());
    client.post(&path, &form).await
}

/// List files mapped to a metadata template.
///
/// `GET /workspace/{workspace_id}/metadata/templates/{template_id}/nodes/`
pub async fn list_template_nodes(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
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
        "/workspace/{}/metadata/templates/{}/nodes/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// AI-based file matching for a metadata template.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/auto-match/`
pub async fn auto_match_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/auto-match/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let form = HashMap::new();
    client.post(&path, &form).await
}

/// Batch-extract metadata for all files mapped to a template.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/extract-all/`
pub async fn extract_all(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/extract-all/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let form = HashMap::new();
    client.post(&path, &form).await
}

/// Extract metadata from a single storage node using a template.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/metadata/extract/`
pub async fn extract_node_metadata(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    template_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/metadata/extract/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    let mut form = HashMap::new();
    form.insert("template_id".to_owned(), template_id.to_owned());
    client.post(&path, &form).await
}
