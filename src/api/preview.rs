#![allow(clippy::missing_errors_doc)]

/// Preview API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for preview URL generation and transform requests.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Get a preauthorized preview URL for a file.
///
/// `GET /{context_type}/{context_id}/storage/{node_id}/preview/{preview_type}/preauthorize/`
pub async fn get_preview_url(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    preview_type: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/preview/{}/preauthorize/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
        urlencoding::encode(preview_type),
    );
    client.get(&path).await
}

/// Parameters for requesting a file transformation.
pub struct TransformUrlParams<'a> {
    /// Context type: workspace or share.
    pub context_type: &'a str,
    /// Context ID (workspace or share ID).
    pub context_id: &'a str,
    /// Storage node ID.
    pub node_id: &'a str,
    /// Transform name (e.g. "image").
    pub transform_name: &'a str,
    /// Target width in pixels.
    pub width: Option<u32>,
    /// Target height in pixels.
    pub height: Option<u32>,
    /// Output format: png, jpg, webp.
    pub output_format: Option<&'a str>,
    /// Size preset.
    pub size: Option<&'a str>,
}

/// Request a file transformation (resize, crop, format conversion).
///
/// `GET /{context_type}/{context_id}/storage/{node_id}/transform/{transform_name}/requestread/`
pub async fn get_transform_url(
    client: &ApiClient,
    params: &TransformUrlParams<'_>,
) -> Result<Value, CliError> {
    let mut query = HashMap::new();
    if let Some(v) = params.width {
        query.insert("width".to_owned(), v.to_string());
    }
    if let Some(v) = params.height {
        query.insert("height".to_owned(), v.to_string());
    }
    if let Some(v) = params.output_format {
        query.insert("output_format".to_owned(), v.to_owned());
    }
    if let Some(v) = params.size {
        query.insert("size".to_owned(), v.to_owned());
    }
    let path = format!(
        "/{}/{}/storage/{}/transform/{}/requestread/",
        urlencoding::encode(params.context_type),
        urlencoding::encode(params.context_id),
        urlencoding::encode(params.node_id),
        urlencoding::encode(params.transform_name),
    );
    if query.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &query).await
    }
}
