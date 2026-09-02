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
///
/// A GET that ACTS: it mints a download token. Routed through
/// [`ApiClient::get_side_effecting`] so a lost response body is never recovered
/// by re-sending — that would mint a second token.
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
    client.get_side_effecting(&path).await
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

    // SCRUB THE URL. This URL carries the scoped read token in `?token=`, and a
    // `reqwest::Error` renders the full URL in its Display — so an unscrubbed
    // transport failure prints a LIVE bearer capability into the user's terminal
    // and into any log or CI transcript that captures stderr. `without_url()`
    // consumes the owned error and keeps the kind/source chain (the real cause,
    // e.g. "connection refused"), which never contains the URL.
    let resp = req
        .send()
        .await
        .map_err(|e| CliError::Http(e.without_url()))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(CliError::Api(crate::error::ApiError {
            code: 0,
            error_code: None,
            message: format!("Download failed with HTTP {status}: {body}"),
            http_status: status,
            details: None,
        }));
    }

    let total_size = resp.content_length();
    let mut file = tokio::fs::File::create(output_path).await?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(chunk_result) = stream.next().await {
        // Same scrub as the send above: a mid-stream failure is also a
        // `reqwest::Error` and also renders the token-bearing URL.
        let chunk = chunk_result.map_err(|e| CliError::Http(e.without_url()))?;
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
///
/// `version_id` is sent at MINT time as well as on the `/read/` URL built by
/// [`build_download_url_ctx`]. Today the token and the version are read
/// independently, so the `/read/` parameter is what selects the version and this
/// one is ignored — verified against the API: `requestread?version_id=…` returns
/// `{"result": true}` with no error.
///
/// It is sent anyway because the token is expected to start PINNING a version.
/// When it does, a token minted without one pins CURRENT, and a historical
/// `/read/` request would then ask for a version its own token contradicts.
/// Sending it at both steps is correct against both servers: an old server
/// ignores it here and honours it there; a new one mints the token for the
/// version actually wanted. **The client has to change BEFORE the server does**,
/// which is why this ships ahead of the need.
///
/// A GET that ACTS: it mints a download token. Routed through the
/// side-effecting path so a lost response body is never recovered by re-sending
/// — that would mint a second token.
pub async fn get_download_url_ctx(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    version_id: Option<&str>,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/requestread/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    match version_id.map(str::trim).filter(|v| !v.is_empty()) {
        Some(version) => {
            let mut params = std::collections::HashMap::new();
            params.insert("version_id".to_owned(), version.to_owned());
            client.get_with_params_side_effecting(&path, &params).await
        }
        None => client.get_side_effecting(&path).await,
    }
}

/// Build a context-aware download URL from a download token.
///
/// When `version_id` is supplied it is appended as a `version_id` query
/// parameter on the `/read/` URL to download that specific version (the
/// `read` handler reads `token` and `version_id` independently).
#[must_use]
pub fn build_download_url_ctx(
    api_base: &str,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    download_token: &str,
    version_id: Option<&str>,
) -> String {
    let mut url = format!(
        "{}/{}/{}/storage/{}/read/?token={}",
        api_base.trim_end_matches('/'),
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
        urlencoding::encode(download_token),
    );
    if let Some(v) = version_id {
        url.push_str("&version_id=");
        url.push_str(&urlencoding::encode(v));
    }
    url
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

#[cfg(test)]
mod tests {
    /// A transport failure must NOT print the download URL: that URL carries the
    /// scoped read token in `?token=`, so an unscrubbed `reqwest::Error` leaks a
    /// live bearer capability into the user's terminal and any log.
    #[tokio::test]
    async fn download_transport_error_does_not_leak_the_url_token() {
        // Port 9 (discard) refuses fast; no network dependency.
        let url = "http://127.0.0.1:9/read/f/node?token=SUPERSECRETREADTOKEN";
        let dir = std::env::temp_dir().join(format!("fastio-leak-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("x.bin");
        let err = super::download_file(url, &out, None, |_, _| {})
            .await
            .expect_err("connection to port 9 must fail");
        let rendered = err.to_string();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !rendered.contains("SUPERSECRETREADTOKEN"),
            "the read token must NOT appear in a transport error: {rendered}"
        );
    }

    use super::{build_download_url_ctx, sanitize_filename};

    #[test]
    fn ctx_url_workspace_no_version() {
        // No version → plain token URL, no version_id query param.
        let url = build_download_url_ctx(
            "https://api.fast.io/current",
            "workspace",
            "19",
            "node1",
            "tok",
            None,
        );
        assert_eq!(
            url,
            "https://api.fast.io/current/workspace/19/storage/node1/read/?token=tok"
        );
        assert!(!url.contains("version_id"));
    }

    /// The version must be sent at MINT time as well as on the read URL.
    ///
    /// Today the token and the version are read independently, so the read URL
    /// is what selects the version. That changes if tokens begin pinning one:
    /// a token minted WITHOUT a version pins current, and a historical read
    /// would contradict its own token. Sending it at both steps is correct
    /// against an old server (ignores it at mint) and a new one (mints for the
    /// right version) — and the client must change FIRST.
    #[test]
    fn read_url_still_carries_version_after_mint_time_send() {
        let url = build_download_url_ctx(
            "https://api.example.test",
            "workspace",
            "19",
            "n1",
            "tok-abc",
            Some("ver-7"),
        );
        assert!(
            url.contains("version_id=ver-7"),
            "read URL must keep the version selector for servers that do not \
             pin at mint time: {url}"
        );
        assert!(
            url.contains("token=tok-abc"),
            "token must ride the URL: {url}"
        );
    }

    #[test]
    fn ctx_url_share_with_version() {
        // Share context + a specific version → version_id is appended as a
        // query param on the /read/ URL (NOT on requestread).
        let url = build_download_url_ctx(
            "https://api.fast.io/current",
            "share",
            "55",
            "node1",
            "tok",
            Some("v9bcd"),
        );
        assert_eq!(
            url,
            "https://api.fast.io/current/share/55/storage/node1/read/?token=tok&version_id=v9bcd"
        );
    }

    #[test]
    fn ctx_url_trims_trailing_base_slash() {
        let url = build_download_url_ctx(
            "https://api.fast.io/current/",
            "workspace",
            "19",
            "n",
            "t",
            None,
        );
        assert!(url.starts_with("https://api.fast.io/current/workspace/19/"));
    }

    #[test]
    fn sanitize_filename_strips_traversal() {
        // Directory components are stripped (basename only), so a traversal
        // path collapses to its final segment.
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("/a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_filename("..."), "download");
    }
}
