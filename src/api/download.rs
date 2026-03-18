#![allow(clippy::missing_errors_doc)]

/// Download API endpoints for the Fast.io REST API.
///
/// Handles download token requests and streaming file downloads.
/// Maps to workspace storage read endpoints.
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// User-Agent string for download requests.
const DOWNLOAD_USER_AGENT: &str = concat!("fastio-cli/", env!("CARGO_PKG_VERSION"));

/// Connection timeout for download requests.
const DOWNLOAD_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Request a download token for a file.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/requestread/`
pub async fn get_download_url(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/requestread/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Get the ZIP download URL for a folder.
///
/// Returns the URL that should be fetched with an Authorization header.
/// `GET /workspace/{workspace_id}/storage/{folder_id}/zip/`
#[must_use]
pub fn get_zip_url(api_base: &str, workspace_id: &str, folder_id: &str) -> String {
    format!(
        "{}/workspace/{}/storage/{}/zip/",
        api_base.trim_end_matches('/'),
        urlencoding::encode(workspace_id),
        urlencoding::encode(folder_id),
    )
}

/// Build the direct download URL from a download token.
#[must_use]
pub fn build_download_url(
    api_base: &str,
    workspace_id: &str,
    node_id: &str,
    download_token: &str,
) -> String {
    format!(
        "{}/workspace/{}/storage/{}/read/?token={}",
        api_base.trim_end_matches('/'),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
        urlencoding::encode(download_token),
    )
}

/// Stream-download a file from a URL to a local path, reporting progress via callback.
///
/// Returns the total number of bytes written.
pub async fn download_file<F>(
    url: &str,
    output_path: &Path,
    token: Option<&str>,
    mut progress_callback: F,
) -> Result<u64, CliError>
where
    F: FnMut(u64, Option<u64>),
{
    // Build a client with a connection timeout but no overall timeout,
    // since downloads may be arbitrarily large and long-running.
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(DOWNLOAD_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;
    let mut req = http_client.get(url).header(USER_AGENT, DOWNLOAD_USER_AGENT);

    if let Some(t) = token {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }

    let resp = req.send().await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(CliError::Api(crate::error::ApiError {
            code: 0,
            error_code: None,
            message: format!("Download failed with HTTP {status}: {body}"),
            http_status: status,
        }));
    }

    let total_size = resp.content_length();
    let mut file = tokio::fs::File::create(output_path).await?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        downloaded += chunk.len() as u64;
        progress_callback(downloaded, total_size);
    }

    Ok(downloaded)
}

/// Get file node details to determine filename for download.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/details/`
pub async fn get_node_details_for_download(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/details/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Retrieve the download token from the API response.
pub fn extract_download_token(response: &Value) -> Option<String> {
    response
        .get("token")
        .and_then(Value::as_str)
        .map(String::from)
}

/// Request a download token for a file in a workspace or share context.
///
/// `GET /{context_type}/{context_id}/storage/{node_id}/requestread/`
pub async fn get_download_url_ctx(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    version_id: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = std::collections::HashMap::new();
    if let Some(v) = version_id {
        params.insert("version_id".to_owned(), v.to_owned());
    }
    let path = format!(
        "/{}/{}/storage/{}/requestread/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Build a context-aware download URL from a download token.
#[must_use]
pub fn build_download_url_ctx(
    api_base: &str,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    download_token: &str,
) -> String {
    format!(
        "{}/{}/{}/storage/{}/read/?token={}",
        api_base.trim_end_matches('/'),
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
        urlencoding::encode(download_token),
    )
}

/// Get a context-aware ZIP download URL.
#[must_use]
pub fn get_zip_url_ctx(
    api_base: &str,
    context_type: &str,
    context_id: &str,
    folder_id: &str,
) -> String {
    format!(
        "{}/{}/{}/storage/{}/zip/",
        api_base.trim_end_matches('/'),
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(folder_id),
    )
}

/// Get quickshare details (no auth required).
///
/// `GET /quickshare/{quickshare_id}/details/`
pub async fn quickshare_details(
    client: &ApiClient,
    quickshare_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/quickshare/{}/details/",
        urlencoding::encode(quickshare_id)
    );
    client.get(&path).await
}

/// Extract filename from node details response.
pub fn extract_filename(details: &Value) -> Option<String> {
    details
        .get("node")
        .and_then(|n| n.get("name"))
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            details
                .get("name")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .map(|name| sanitize_filename(&name))
}

/// Sanitize a server-supplied filename to prevent path traversal.
///
/// Strips directory components, `..` sequences, and leading dots.
/// Falls back to `"download"` if the result is empty.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let basename = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = basename.replace("..", "");
    let trimmed = cleaned.trim_start_matches('.');
    if trimmed.is_empty() {
        "download".to_owned()
    } else {
        trimmed.to_owned()
    }
}
