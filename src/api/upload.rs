#![allow(clippy::missing_errors_doc)]

/// Upload API endpoints for the Fast.io REST API.
///
/// Handles chunked upload lifecycle: session creation, chunk upload,
/// assembly, polling, and adding files to workspace storage.
/// Maps to endpoints at `/current/upload/`.
use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use bytes::Bytes;
use colored::Colorize;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::multipart;
use serde_json::{Value, json};

use crate::client::ApiClient;
use crate::error::CliError;

/// User-Agent string for upload requests.
const UPLOAD_USER_AGENT: &str = concat!("fastio-cli/", env!("CARGO_PKG_VERSION"));

/// Timeout for chunk upload requests (5 minutes to accommodate large chunks).
const CHUNK_UPLOAD_TIMEOUT_SECS: u64 = 300;

/// Connection timeout for chunk upload requests.
const CHUNK_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Maximum number of retries for chunk uploads.
const CHUNK_MAX_RETRIES: u32 = 3;

/// Initial backoff delay for chunk upload retries.
const CHUNK_INITIAL_BACKOFF: Duration = Duration::from_secs(2);

/// Maximum backoff delay for chunk upload retries.
const CHUNK_MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Maximum file size for single-call uploads (4 MB).
pub const SINGLE_CALL_MAX_SIZE: u64 = 4 * 1024 * 1024;

/// Upload a small file in a single API call (≤ 4 MB).
///
/// `POST /upload/` with multipart form including the file data as the `chunk` field.
/// Returns the unwrapped response payload with `new_file_id` — no separate assembly
/// or polling step required. Includes retry logic with exponential backoff for
/// transient failures (429, 502-504, timeouts, network errors).
#[allow(clippy::too_many_lines)]
pub async fn single_call_upload(
    token: &str,
    api_base: &str,
    workspace_id: &str,
    folder_id: &str,
    filename: &str,
    file_data: Vec<u8>,
) -> Result<Value, CliError> {
    let file_size = file_data.len();
    let url = format!("{}/upload/", api_base.trim_end_matches('/'));

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(CHUNK_UPLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CHUNK_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    let mut attempt: u32 = 0;
    let mut backoff = CHUNK_INITIAL_BACKOFF;

    loop {
        let part = multipart::Part::bytes(file_data.clone()).file_name(filename.to_owned());
        let form = multipart::Form::new()
            .text("name", filename.to_owned())
            .text("size", file_size.to_string())
            .text("action", "create")
            .text("instance_id", workspace_id.to_owned())
            .text("folder_id", folder_id.to_owned())
            .text("profile_type", "workspace")
            .part("chunk", part);

        let send_result = http_client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(USER_AGENT, UPLOAD_USER_AGENT)
            .multipart(form)
            .send()
            .await;

        match send_result {
            Ok(resp) => {
                let status = resp.status();

                // Handle 429 rate limiting with retry.
                if status.as_u16() == 429 && attempt < CHUNK_MAX_RETRIES {
                    let retry_secs = parse_rate_limit_expiry(&resp);
                    attempt += 1;
                    eprintln!(
                        "{} Upload rate limited (attempt {}/{CHUNK_MAX_RETRIES}). \
                         Waiting {retry_secs} seconds.",
                        "warning:".yellow().bold(),
                        attempt,
                    );
                    tokio::time::sleep(Duration::from_secs(retry_secs)).await;
                    continue;
                }

                // Retry on server errors.
                if status.is_server_error() && attempt < CHUNK_MAX_RETRIES {
                    attempt += 1;
                    eprintln!(
                        "{} Upload server error (HTTP {}, attempt {}/{CHUNK_MAX_RETRIES}). \
                         Retrying in {} seconds.",
                        "warning:".yellow().bold(),
                        status.as_u16(),
                        attempt,
                        backoff.as_secs(),
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff.saturating_mul(2), CHUNK_MAX_BACKOFF);
                    continue;
                }

                let body: Value = resp.json().await.map_err(|e| {
                    CliError::Parse(format!("failed to parse single-call upload response: {e}"))
                })?;

                let result_ok = match body.get("result") {
                    Some(Value::String(s)) => s == "yes",
                    Some(Value::Bool(b)) => *b,
                    _ => false,
                };

                if result_ok {
                    // Unwrap envelope: prefer "response" sub-object, fall back
                    // to top level sans metadata keys.
                    let payload = if let Some(response_obj) = body.get("response") {
                        response_obj.clone()
                    } else {
                        let mut map = body;
                        if let Some(obj) = map.as_object_mut() {
                            obj.remove("result");
                            obj.remove("current_api_version");
                        }
                        map
                    };
                    return Ok(payload);
                }

                let message = body
                    .get("error")
                    .and_then(|e| e.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("Single-call upload failed");
                return Err(CliError::Api(crate::error::ApiError {
                    code: u32::try_from(
                        body.get("error")
                            .and_then(|e| e.get("code"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .unwrap_or(0),
                    error_code: None,
                    message: message.to_owned(),
                    http_status: status.as_u16(),
                }));
            }
            Err(err) => {
                if should_retry_network_error(&err, 0, &mut attempt, &mut backoff).await {
                    continue;
                }
                return Err(err.into());
            }
        }
    }
}

/// Create a new chunked upload session.
///
/// `POST /upload/`
pub async fn create_upload_session(
    client: &ApiClient,
    workspace_id: &str,
    folder_id: &str,
    filename: &str,
    filesize: u64,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), filename.to_owned());
    form.insert("size".to_owned(), filesize.to_string());
    form.insert("action".to_owned(), "create".to_owned());
    form.insert("instance_id".to_owned(), workspace_id.to_owned());
    form.insert("folder_id".to_owned(), folder_id.to_owned());
    form.insert("profile_type".to_owned(), "workspace".to_owned());
    client.post("/upload/", &form).await
}

/// Upload a single chunk of file data via multipart form.
///
/// `POST /upload/{upload_id}/chunk/?order={chunk_number}&size={chunk_size}`
///
/// This uses a raw reqwest client because the API expects multipart/form-data
/// with a binary `chunk` field, which differs from the standard form-encoded
/// POST used elsewhere. Includes its own retry logic with exponential backoff
/// for transient failures and rate limiting.
pub async fn upload_chunk(
    token: &str,
    api_base: &str,
    upload_id: &str,
    chunk_number: u32,
    chunk_data: Vec<u8>,
) -> Result<Value, CliError> {
    let chunk_size = chunk_data.len();
    let url = format!(
        "{}/upload/{}/chunk/?order={}&size={}",
        api_base.trim_end_matches('/'),
        urlencoding::encode(upload_id),
        chunk_number,
        chunk_size,
    );

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(CHUNK_UPLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CHUNK_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    let mut attempt: u32 = 0;
    let mut backoff = CHUNK_INITIAL_BACKOFF;

    loop {
        let part = multipart::Part::bytes(chunk_data.clone()).file_name("chunk.bin");
        let form = multipart::Form::new().part("chunk", part);

        let send_result = http_client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(USER_AGENT, UPLOAD_USER_AGENT)
            .multipart(form)
            .send()
            .await;

        match send_result {
            Ok(resp) => {
                match handle_chunk_response(resp, chunk_number, &mut attempt, &mut backoff).await {
                    ChunkResult::Success(body) => return Ok(body),
                    ChunkResult::Error(err) => return Err(err),
                    ChunkResult::Retry => {}
                }
            }
            Err(err) => {
                if should_retry_network_error(&err, chunk_number, &mut attempt, &mut backoff).await
                {
                    continue;
                }
                return Err(err.into());
            }
        }
    }
}

/// Result of processing a chunk upload HTTP response.
enum ChunkResult {
    Success(Value),
    Error(CliError),
    Retry,
}

/// Handle a successful HTTP response from a chunk upload.
async fn handle_chunk_response(
    resp: reqwest::Response,
    chunk_number: u32,
    attempt: &mut u32,
    backoff: &mut Duration,
) -> ChunkResult {
    let status = resp.status();

    if status.as_u16() == 429 && *attempt < CHUNK_MAX_RETRIES {
        let retry_secs = parse_rate_limit_expiry(&resp);
        *attempt += 1;
        eprintln!(
            "{} Chunk {chunk_number} rate limited (attempt {}/{CHUNK_MAX_RETRIES}). \
             Waiting {retry_secs} seconds.",
            "warning:".yellow().bold(),
            *attempt,
        );
        tokio::time::sleep(Duration::from_secs(retry_secs)).await;
        return ChunkResult::Retry;
    }

    if status.is_server_error() && *attempt < CHUNK_MAX_RETRIES {
        *attempt += 1;
        eprintln!(
            "{} Chunk {chunk_number} server error (HTTP {}, attempt {}/{CHUNK_MAX_RETRIES}). \
             Retrying in {} seconds.",
            "warning:".yellow().bold(),
            status.as_u16(),
            *attempt,
            backoff.as_secs(),
        );
        tokio::time::sleep(*backoff).await;
        *backoff = std::cmp::min(backoff.saturating_mul(2), CHUNK_MAX_BACKOFF);
        return ChunkResult::Retry;
    }

    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            return ChunkResult::Error(CliError::Parse(format!(
                "failed to parse chunk upload response: {e}"
            )));
        }
    };

    let result_ok = match body.get("result") {
        Some(Value::String(s)) => s == "yes",
        Some(Value::Bool(b)) => *b,
        _ => false,
    };

    if result_ok {
        return ChunkResult::Success(body);
    }

    let message = body
        .get("error")
        .and_then(|e| e.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("Chunk upload failed");
    ChunkResult::Error(CliError::Api(crate::error::ApiError {
        code: u32::try_from(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
        .unwrap_or(0),
        error_code: None,
        message: message.to_owned(),
        http_status: status.as_u16(),
    }))
}

/// Check if a network error should be retried, logging and sleeping if so.
async fn should_retry_network_error(
    err: &reqwest::Error,
    chunk_number: u32,
    attempt: &mut u32,
    backoff: &mut Duration,
) -> bool {
    let retryable = err.is_timeout() || err.is_connect() || err.is_request();
    if retryable && *attempt < CHUNK_MAX_RETRIES {
        *attempt += 1;
        eprintln!(
            "{} Chunk {chunk_number} network error (attempt {}/{CHUNK_MAX_RETRIES}): {err}. \
             Retrying in {} seconds.",
            "warning:".yellow().bold(),
            *attempt,
            backoff.as_secs(),
        );
        tokio::time::sleep(*backoff).await;
        *backoff = std::cmp::min(backoff.saturating_mul(2), CHUNK_MAX_BACKOFF);
        return true;
    }
    false
}

/// Parse the rate-limit expiry header to estimate seconds until reset.
fn parse_rate_limit_expiry(resp: &reqwest::Response) -> u64 {
    resp.headers()
        .get("X-Rate-Limit-Expiry")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(60, |expiry_epoch| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            expiry_epoch.saturating_sub(now)
        })
}

/// Trigger file assembly after all chunks are uploaded.
///
/// `POST /upload/{upload_id}/complete/`
pub async fn complete_upload(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!("/upload/{}/complete/", urlencoding::encode(upload_id),);
    client.post(&path, &form).await
}

/// Get the current status of an upload session.
///
/// `GET /upload/{upload_id}/details/`
pub async fn get_upload_status(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let path = format!("/upload/{}/details/", urlencoding::encode(upload_id),);
    client.get(&path).await
}

/// Cancel and delete an upload session.
///
/// `DELETE /upload/{upload_id}/`
pub async fn cancel_upload(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let path = format!("/upload/{}/", urlencoding::encode(upload_id));
    client.delete(&path).await
}

/// Start a web (URL) import.
///
/// `POST /web_upload/`
pub async fn web_import(
    client: &ApiClient,
    workspace_id: &str,
    folder_id: &str,
    url: &str,
    filename: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("source_url".to_owned(), url.to_owned());
    form.insert("profile_id".to_owned(), workspace_id.to_owned());
    form.insert("profile_type".to_owned(), "workspace".to_owned());
    form.insert("folder_id".to_owned(), folder_id.to_owned());
    if let Some(name) = filename {
        form.insert("file_name".to_owned(), name.to_owned());
    }
    client.post("/web_upload/", &form).await
}

/// Get the status of a web import job.
///
/// `GET /web_upload/{upload_id}/details/`
pub async fn web_import_status(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let path = format!("/web_upload/{}/details/", urlencoding::encode(upload_id),);
    client.get(&path).await
}

/// List all active upload sessions.
///
/// `GET /upload/details/`
pub async fn list_sessions(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/upload/details/").await
}

/// Cancel all active upload sessions.
///
/// `DELETE /upload/`
pub async fn cancel_all(client: &ApiClient) -> Result<Value, CliError> {
    client.delete("/upload/").await
}

/// Get chunk status for an upload session.
///
/// `GET /upload/{upload_id}/chunk/`
pub async fn chunk_status(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let path = format!("/upload/{}/chunk/", urlencoding::encode(upload_id));
    client.get(&path).await
}

/// Delete a chunk in an upload session.
///
/// `DELETE /upload/{upload_id}/chunk/?order={chunk_number}`
pub async fn chunk_delete(
    client: &ApiClient,
    upload_id: &str,
    chunk_number: u32,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("order".to_owned(), chunk_number.to_string());
    let path = format!("/upload/{}/chunk/", urlencoding::encode(upload_id));
    client.delete_with_params(&path, &params).await
}

/// List web import jobs.
///
/// `GET /web_upload/list/`
pub async fn web_list(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/web_upload/list/").await
}

/// Cancel a web import job.
///
/// `DELETE /web_upload/{upload_id}/`
pub async fn web_cancel(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let path = format!("/web_upload/{}/", urlencoding::encode(upload_id));
    client.delete(&path).await
}

/// Get upload limits for the user's plan.
///
/// `GET /upload/limits/`
pub async fn upload_limits(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/upload/limits/").await
}

/// Get restricted file extensions.
///
/// `GET /upload/extensions/`
pub async fn upload_extensions(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/upload/extensions/").await
}

// ─── Streaming Upload ──────────────────────────────────────────────────────

/// Timeout for stream upload requests (10 minutes for potentially large streams).
const STREAM_UPLOAD_TIMEOUT_SECS: u64 = 600;

/// Create a new streaming upload session.
///
/// `POST /upload/`
///
/// Unlike [`create_upload_session`], this enables stream mode where the exact
/// file size is not required upfront. The `max_size` parameter sets an upper
/// bound on the stream payload; if omitted, the plan's file-size limit applies.
pub async fn create_stream_session(
    client: &ApiClient,
    workspace_id: &str,
    folder_id: &str,
    filename: &str,
    max_size: Option<u64>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), filename.to_owned());
    form.insert("stream".to_owned(), "true".to_owned());
    form.insert("action".to_owned(), "create".to_owned());
    form.insert("instance_id".to_owned(), workspace_id.to_owned());
    form.insert("folder_id".to_owned(), folder_id.to_owned());
    form.insert("profile_type".to_owned(), "workspace".to_owned());
    if let Some(max) = max_size {
        form.insert("max_size".to_owned(), max.to_string());
    }
    client.post("/upload/", &form).await
}

/// Stream-upload data to a streaming upload session.
///
/// `POST /upload/{upload_id}/stream/`
///
/// Sends the entire payload as a raw binary body with
/// `Content-Type: application/octet-stream`. The session auto-finalizes on
/// completion — no `/complete/` call is needed.
///
/// Accepts [`Bytes`] for O(1) cloning across retry attempts. Only retries on
/// rate-limiting (429) and pre-send connection errors — server errors and
/// timeouts are *not* retried because the server may have already received
/// and finalized the stream data.
pub async fn stream_upload(
    token: &str,
    api_base: &str,
    upload_id: &str,
    data: Bytes,
    hash: Option<&str>,
    hash_algo: Option<&str>,
) -> Result<Value, CliError> {
    let mut url = format!(
        "{}/upload/{}/stream/",
        api_base.trim_end_matches('/'),
        urlencoding::encode(upload_id),
    );

    let mut has_query = false;
    if let Some(h) = hash {
        let _ = write!(url, "?hash={}", urlencoding::encode(h));
        has_query = true;
    }
    if let Some(algo) = hash_algo {
        let sep = if has_query { "&" } else { "?" };
        let _ = write!(url, "{sep}hash_algo={}", urlencoding::encode(algo));
    }

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(STREAM_UPLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CHUNK_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    let mut attempt: u32 = 0;
    let mut backoff = CHUNK_INITIAL_BACKOFF;

    loop {
        // Bytes::clone() is O(1) — reference-counted, no data copy.
        let send_result = http_client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(USER_AGENT, UPLOAD_USER_AGENT)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(data.clone())
            .send()
            .await;

        match send_result {
            Ok(resp) => match handle_stream_response(resp, &mut attempt).await {
                StreamResult::Success(body) => return Ok(body),
                StreamResult::Error(err) => return Err(err),
                StreamResult::Retry => {}
            },
            Err(err) => {
                // Only retry connection errors where no data was sent.
                // Timeouts are NOT retried — the server may have received
                // and finalized the data before the client timed out.
                if should_retry_stream_error(&err, &mut attempt, &mut backoff).await {
                    continue;
                }
                return Err(err.into());
            }
        }
    }
}

/// Result of processing a stream upload HTTP response.
enum StreamResult {
    Success(Value),
    Error(CliError),
    Retry,
}

/// Handle an HTTP response from a stream upload.
///
/// Only 429 (rate-limited) is retried — the server explicitly rejected the
/// request before processing the body. Server errors (5xx) are *not* retried
/// because the stream is single-shot and the server may have already received
/// and finalized the data.
async fn handle_stream_response(resp: reqwest::Response, attempt: &mut u32) -> StreamResult {
    let status = resp.status();

    // 429 is safe to retry — the server rejected the request before
    // processing the body.
    if status.as_u16() == 429 && *attempt < CHUNK_MAX_RETRIES {
        let retry_secs = parse_rate_limit_expiry(&resp);
        *attempt += 1;
        eprintln!(
            "{} Stream upload rate limited (attempt {}/{CHUNK_MAX_RETRIES}). \
             Waiting {retry_secs} seconds.",
            "warning:".yellow().bold(),
            *attempt,
        );
        tokio::time::sleep(Duration::from_secs(retry_secs)).await;
        return StreamResult::Retry;
    }

    // Do NOT retry 5xx — unlike chunked uploads, streaming is single-shot.
    // The server may have received and finalized the data before returning
    // an error response.

    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            return StreamResult::Error(CliError::Parse(format!(
                "failed to parse stream upload response: {e}"
            )));
        }
    };

    let result_ok = match body.get("result") {
        Some(Value::String(s)) => s == "yes",
        Some(Value::Bool(b)) => *b,
        _ => false,
    };

    if result_ok {
        return StreamResult::Success(body);
    }

    let message = body
        .get("error")
        .and_then(|e| e.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("Stream upload failed");
    StreamResult::Error(CliError::Api(crate::error::ApiError {
        code: u32::try_from(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
        .unwrap_or(0),
        error_code: None,
        message: message.to_owned(),
        http_status: status.as_u16(),
    }))
}

/// Check if a stream network error should be retried, logging and sleeping if so.
///
/// Only connection and request-build errors are retried — these occur before
/// any data is sent. Timeouts are *not* retried because the server may have
/// received the full payload before the client timed out waiting for the
/// response.
async fn should_retry_stream_error(
    err: &reqwest::Error,
    attempt: &mut u32,
    backoff: &mut Duration,
) -> bool {
    // is_connect: TCP connection failed — no data sent.
    // is_request: error building the request — no data sent.
    // is_timeout: NOT retried — data may have been fully sent.
    let retryable = err.is_connect() || err.is_request();
    if retryable && *attempt < CHUNK_MAX_RETRIES {
        *attempt += 1;
        eprintln!(
            "{} Stream upload connection error (attempt {}/{CHUNK_MAX_RETRIES}): {err}. \
             Retrying in {} seconds.",
            "warning:".yellow().bold(),
            *attempt,
            backoff.as_secs(),
        );
        tokio::time::sleep(*backoff).await;
        *backoff = std::cmp::min(backoff.saturating_mul(2), CHUNK_MAX_BACKOFF);
        return true;
    }
    false
}

// ─── Batch Upload ──────────────────────────────────────────────────────────

/// Hard limit: maximum files per batch request.
pub const BATCH_MAX_FILES: usize = 200;

/// Hard limit: maximum total multipart body size per batch (100 MB).
///
/// The server applies the same cap post-base64-decode on the JSON path.
pub const BATCH_MAX_BODY_BYTES: u64 = 100 * 1024 * 1024;

/// Hard limit: maximum per-file size accepted by the batch endpoint.
///
/// Files larger than this must route to the single-file chunked pipeline.
pub const BATCH_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// One file to upload in a batch, with its in-memory bytes and manifest metadata.
///
/// Constructed by callers (not read from the wire), so this type is NOT
/// `#[non_exhaustive]` — consumers must be able to build it with struct
/// literal syntax. Fields may still be added in a minor release; any
/// addition will be a semver-compatible `Default`-populated field.
#[derive(Debug, Clone)]
pub struct BatchUploadItem {
    /// 1-255 char filename the server will use.
    pub filename: String,
    /// Optional per-entry sub-path under the batch-level `folder_id`.
    ///
    /// Must end in `/`, may not start with `/` or contain `..` segments.
    pub relative_path: Option<String>,
    /// The raw file bytes. Must be ≤ [`BATCH_MAX_FILE_BYTES`].
    ///
    /// `Bytes` is reference-counted, so the retry loop inside [`upload_batch`]
    /// re-uses the same backing buffer across attempts instead of memcpy'ing
    /// a fresh `Vec<u8>` per retry. (The underlying HTTP/TLS layer may still
    /// stage its own buffers; the guarantee is at this type's boundary.)
    pub data: Bytes,
    /// Optional hex digest; pairs with [`Self::hash_algo`].
    pub hash: Option<String>,
    /// Optional hash algorithm (`md5`, `sha1`, `sha256`, `sha384`).
    pub hash_algo: Option<String>,
}

/// Per-file outcome from a batch upload, mirroring `results[]` in the response.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BatchUploadResult {
    /// Manifest index (0-based).
    pub index: u32,
    /// Echo of the submitted filename.
    pub filename: String,
    /// `"ok"` or `"error"`.
    pub status: String,
    /// Upload session `OpaqueId`. `Some` on `ok`.
    pub upload_id: Option<String>,
    /// Final file node `OpaqueId`. `Some(Some(id))` when finalize completed inline,
    /// `Some(None)` when storage is async (assigned later by the assemble worker),
    /// `None` on error.
    ///
    /// Do NOT conflate a null `node_id` with failure — it is a documented success
    /// state for async-storage workspaces.
    pub node_id: Option<Option<String>>,
    /// Numeric Fast.io error code. `Some` on `error`.
    pub error_code: Option<u32>,
    /// Human-readable error message. `Some` on `error`.
    pub error_message: Option<String>,
}

/// Aggregated response from `POST /upload/batch/` — mirrors the unwrapped
/// `response` envelope.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BatchUploadResponse {
    /// Opaque batch identifier (valid for 1 hour).
    pub batch_id: String,
    /// Total files submitted.
    pub count_total: u32,
    /// Files with `status: "ok"`.
    pub count_succeeded: u32,
    /// Files with `status: "error"`.
    pub count_errored: u32,
    /// Per-file results.
    pub results: Vec<BatchUploadResult>,
}

/// Post a single batch to `POST /upload/batch/`.
///
/// This is the low-level single-request function: callers are responsible for
/// ensuring the batch satisfies the hard limits (≤ [`BATCH_MAX_FILES`] files,
/// ≤ [`BATCH_MAX_BODY_BYTES`] total, each file ≤ [`BATCH_MAX_FILE_BYTES`]).
/// Use the higher-level orchestrator for chunking + large-file fallback.
///
/// Rate-limit handling: on HTTP 429 (code 1671) the function reads
/// `X-Rate-Limit-Expiry` and sleeps until then before retrying, up to
/// [`CHUNK_MAX_RETRIES`]. The batch endpoint has its own bucket
/// (`upload_batch_create`) independent of the single-file `/upload/` bucket.
///
/// Whole-batch 4xx rejections return `CliError::Api` with the server's error
/// code and message. Per-file errors are *not* elevated — they land in
/// `results[]` and the HTTP status is 200.
pub async fn upload_batch(
    token: &str,
    api_base: &str,
    instance_id: &str,
    folder_id: Option<&str>,
    creator: Option<&str>,
    items: &[BatchUploadItem],
) -> Result<BatchUploadResponse, CliError> {
    if items.is_empty() {
        return Err(CliError::Parse("batch upload has zero items".to_owned()));
    }
    if items.len() > BATCH_MAX_FILES {
        return Err(CliError::Parse(format!(
            "batch upload has {} files; server limit is {BATCH_MAX_FILES}",
            items.len(),
        )));
    }

    let url = format!("{}/upload/batch/", api_base.trim_end_matches('/'));

    let manifest = build_manifest_json(items)?;
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(CHUNK_UPLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CHUNK_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    let mut attempt: u32 = 0;
    let mut backoff = CHUNK_INITIAL_BACKOFF;

    loop {
        let mut form = multipart::Form::new()
            .text("instance_id", instance_id.to_owned())
            .text("manifest", manifest.clone());
        if let Some(folder) = folder_id {
            form = form.text("folder_id", folder.to_owned());
        }
        if let Some(tag) = creator {
            form = form.text("creator", tag.to_owned());
        }
        for (i, item) in items.iter().enumerate() {
            // Bytes::clone is O(1) (refcount bump) — no 100 MB memcpy on retry.
            let body = reqwest::Body::from(item.data.clone());
            let part = multipart::Part::stream_with_length(body, item.data.len() as u64)
                .file_name(item.filename.clone());
            form = form.part(format!("file_{i}"), part);
        }

        let send_result = http_client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(USER_AGENT, UPLOAD_USER_AGENT)
            .multipart(form)
            .send()
            .await;

        match send_result {
            Ok(resp) => {
                let status = resp.status();

                // 429: separate bucket (upload_batch_create). Respect the
                // server's reset header and retry; pressure is surfaced via
                // the eprintln below so the caller sees it on progress UIs.
                if status.as_u16() == 429 && attempt < CHUNK_MAX_RETRIES {
                    let retry_secs = parse_rate_limit_expiry(&resp);
                    attempt += 1;
                    eprintln!(
                        "{} Batch upload rate limited (attempt {}/{CHUNK_MAX_RETRIES}). \
                         Waiting {retry_secs} seconds.",
                        "warning:".yellow().bold(),
                        attempt,
                    );
                    tokio::time::sleep(Duration::from_secs(retry_secs)).await;
                    continue;
                }

                if status.is_server_error() && attempt < CHUNK_MAX_RETRIES {
                    attempt += 1;
                    eprintln!(
                        "{} Batch upload server error (HTTP {}, attempt {}/{CHUNK_MAX_RETRIES}). \
                         Retrying in {} seconds.",
                        "warning:".yellow().bold(),
                        status.as_u16(),
                        attempt,
                        backoff.as_secs(),
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff.saturating_mul(2), CHUNK_MAX_BACKOFF);
                    continue;
                }

                let body: Value = resp.json().await.map_err(|e| {
                    CliError::Parse(format!("failed to parse batch upload response: {e}"))
                })?;

                return parse_batch_response(&body, status.as_u16());
            }
            Err(err) => {
                // Inline retry for network errors: (a) the shared chunk helper
                // would label this "Chunk 0 network error"; (b) batch retry
                // policy is deliberately narrower than chunked — we omit
                // `is_timeout()` because the server may have already parsed
                // the body and created files, and replaying after timeout
                // would duplicate them. Connect/request errors fail before
                // any bytes reach the wire and are safe to retry.
                let retryable = err.is_connect() || err.is_request();
                if retryable && attempt < CHUNK_MAX_RETRIES {
                    attempt += 1;
                    eprintln!(
                        "{} Batch upload network error (attempt {}/{CHUNK_MAX_RETRIES}): {err}. \
                         Retrying in {} seconds.",
                        "warning:".yellow().bold(),
                        attempt,
                        backoff.as_secs(),
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff.saturating_mul(2), CHUNK_MAX_BACKOFF);
                    continue;
                }
                return Err(err.into());
            }
        }
    }
}

/// Serialize the manifest array expected by the server.
fn build_manifest_json(items: &[BatchUploadItem]) -> Result<String, CliError> {
    let entries: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mut obj = serde_json::Map::new();
            obj.insert("index".to_owned(), json!(i));
            obj.insert("filename".to_owned(), json!(item.filename));
            if let Some(rp) = &item.relative_path {
                obj.insert("relative_path".to_owned(), json!(rp));
            }
            if let (Some(algo), Some(hash)) = (&item.hash_algo, &item.hash) {
                obj.insert("hash_algo".to_owned(), json!(algo));
                obj.insert("hash".to_owned(), json!(hash));
            }
            Value::Object(obj)
        })
        .collect();
    serde_json::to_string(&entries)
        .map_err(|e| CliError::Parse(format!("failed to encode batch manifest: {e}")))
}

/// Parse a `POST /upload/batch/` response into a [`BatchUploadResponse`].
///
/// On HTTP 2xx with `result: yes` / `result: true` the payload is unwrapped
/// from either the documented `response` sub-object OR the top level —
/// production returns the latter and `single_call_upload` already handles
/// both shapes, so this function matches.
/// On failure (HTTP 4xx or `result: no`), returns `CliError::Api` with the
/// server's error code and message verbatim.
fn parse_batch_response(body: &Value, http_status: u16) -> Result<BatchUploadResponse, CliError> {
    let result_ok = match body.get("result") {
        Some(Value::String(s)) => s == "yes",
        Some(Value::Bool(b)) => *b,
        _ => false,
    };

    if !result_ok {
        let message = body
            .get("error")
            .and_then(|e| e.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("batch upload failed");
        let code = u32::try_from(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
        .unwrap_or(0);
        return Err(CliError::Api(crate::error::ApiError {
            code,
            error_code: None,
            message: message.to_owned(),
            http_status,
        }));
    }

    // The docs example wraps the batch data in `response: {...}`, but the
    // production server returns it at the top level. Support both.
    let response = body.get("response").unwrap_or(body);

    let batch_id = response
        .get("batch_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let count_total = u32::try_from(
        response
            .get("count_total")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
    .unwrap_or(0);
    let count_succeeded = u32::try_from(
        response
            .get("count_succeeded")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
    .unwrap_or(0);
    let count_errored = u32::try_from(
        response
            .get("count_errored")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
    .unwrap_or(0);

    let results = response
        .get("results")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(parse_batch_result).collect::<Vec<_>>())
        .unwrap_or_default();

    Ok(BatchUploadResponse {
        batch_id,
        count_total,
        count_succeeded,
        count_errored,
        results,
    })
}

/// Parse one entry of `response.results[]`. Preserves the distinction between
/// a missing `node_id` (should not happen on `ok`) and an explicit `null`
/// (documented success state for async-storage workspaces).
fn parse_batch_result(entry: &Value) -> BatchUploadResult {
    let index = u32::try_from(entry.get("index").and_then(Value::as_u64).unwrap_or(0)).unwrap_or(0);
    let filename = entry
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let status = entry
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let upload_id = entry
        .get("upload_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let node_id = entry.get("node_id").map(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    });
    let error_code = entry
        .get("error_code")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let error_message = entry
        .get("error_message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    BatchUploadResult {
        index,
        filename,
        status,
        upload_id,
        node_id,
        error_code,
        error_message,
    }
}

/// Validate a `relative_path` against the server rules:
/// 1-8192 chars, trailing `/`, no leading `/`, no `..` segments.
pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("relative_path must not be empty".to_owned());
    }
    if path.len() > 8192 {
        return Err("relative_path exceeds 8192 characters".to_owned());
    }
    if !path.ends_with('/') {
        return Err("relative_path must end with '/'".to_owned());
    }
    if path.starts_with('/') {
        return Err("relative_path must not start with '/'".to_owned());
    }
    if path.split('/').any(|seg| seg == "..") {
        return Err("relative_path must not contain '..' segments".to_owned());
    }
    Ok(())
}

/// Validate a filename against the characters accepted by the server and
/// defend against multipart-envelope injection, cross-platform surprises,
/// and Trojan-Source-style bidi attacks.
///
/// Rejects:
/// - empty, or > 255 Unicode scalar values (server spec is char-counted)
/// - the special names `.` and `..`
/// - embedded NUL / CR / LF (would corrupt `Content-Disposition`)
/// - path separators (`/`, `\`) — use `relative_path` for subfolders
/// - trailing whitespace or `.` (silently stripped on Windows, collides)
/// - all control-zone characters (C0, C1, DEL)
/// - bidi override and zero-width code points (U+200B-200F, U+202A-202E,
///   U+2066-2069, U+FEFF) — orthogonal to the markdown sanitizer but same
///   Trojan-Source defense (see CLAUDE.md gotcha #14)
pub fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("filename must not be empty".to_owned());
    }
    // Server counts characters, not bytes — a 100-char CJK filename is ~300
    // bytes and must be accepted.
    if name.chars().count() > 255 {
        return Err("filename exceeds 255 characters".to_owned());
    }
    if name == "." || name == ".." {
        return Err("filename must not be '.' or '..'".to_owned());
    }
    // Trailing whitespace / dot: Windows silently strips these, so a pair
    // of filenames differing only by a trailing space would collide after
    // the server reflects them back.
    if matches!(name.chars().last(), Some(' ' | '\t' | '.')) {
        return Err("filename must not end with whitespace or '.'".to_owned());
    }
    for ch in name.chars() {
        match ch {
            '\0' => return Err("filename must not contain NUL bytes".to_owned()),
            '\r' | '\n' => {
                return Err("filename must not contain CR or LF".to_owned());
            }
            '/' | '\\' => {
                return Err(
                    "filename must not contain path separators — use relative_path for subfolders"
                        .to_owned(),
                );
            }
            c if c.is_control() => {
                return Err(format!(
                    "filename must not contain control characters (U+{:04X})",
                    c as u32
                ));
            }
            // Bidi override / zero-width / BOM code points. Grouping by
            // range (not enumerating each) keeps the check readable.
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}' => {
                return Err(format!(
                    "filename must not contain bidi/zero-width code point (U+{:04X})",
                    ch as u32
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate the optional `creator` batch-level correlation tag.
///
/// Server accepts 1-150 chars, ASCII alphanumeric + `-`. Failing the server's
/// validation rejects the whole batch (HTTP 4xx) after the request body is
/// uploaded, so we short-circuit client-side.
pub fn validate_creator_tag(tag: &str) -> Result<(), String> {
    if tag.is_empty() || tag.len() > 150 {
        return Err("creator must be 1-150 characters".to_owned());
    }
    if !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("creator must contain only ASCII alphanumerics and hyphens".to_owned());
    }
    Ok(())
}

/// Compute the lower-case hex SHA-256 of the supplied bytes.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, size: usize) -> BatchUploadItem {
        BatchUploadItem {
            filename: name.to_owned(),
            relative_path: None,
            data: Bytes::from(vec![b'x'; size]),
            hash: None,
            hash_algo: None,
        }
    }

    #[test]
    fn manifest_includes_hash_pair_only_when_both_present() {
        let items = vec![
            BatchUploadItem {
                filename: "a.txt".to_owned(),
                relative_path: Some("sub/".to_owned()),
                data: Bytes::from_static(&[1, 2, 3]),
                hash: Some("abc".to_owned()),
                hash_algo: Some("sha256".to_owned()),
            },
            BatchUploadItem {
                filename: "b.txt".to_owned(),
                relative_path: None,
                // Hash without algo — should be dropped by the manifest builder.
                data: Bytes::from_static(&[4]),
                hash: Some("dangling".to_owned()),
                hash_algo: None,
            },
        ];
        let json = build_manifest_json(&items).expect("manifest");
        let parsed: Vec<Value> = serde_json::from_str(&json).expect("parse");

        assert_eq!(parsed[0]["index"], 0);
        assert_eq!(parsed[0]["filename"], "a.txt");
        assert_eq!(parsed[0]["relative_path"], "sub/");
        assert_eq!(parsed[0]["hash"], "abc");
        assert_eq!(parsed[0]["hash_algo"], "sha256");

        assert_eq!(parsed[1]["index"], 1);
        assert!(parsed[1].get("hash").is_none());
        assert!(parsed[1].get("hash_algo").is_none());
        assert!(parsed[1].get("relative_path").is_none());
    }

    #[test]
    fn parse_batch_response_partial_success() {
        let body = json!({
            "result": "yes",
            "response": {
                "batch_id": "BID-1",
                "count_total": 3,
                "count_succeeded": 2,
                "count_errored": 1,
                "results": [
                    {"index": 0, "filename": "a", "status": "ok",
                     "upload_id": "U0", "node_id": "N0"},
                    // node_id: null → async storage, still success.
                    {"index": 1, "filename": "b", "status": "ok",
                     "upload_id": "U1", "node_id": null},
                    {"index": 2, "filename": "c", "status": "error",
                     "error_code": 1605, "error_message": "bad hash"}
                ]
            }
        });
        let parsed = parse_batch_response(&body, 200).expect("parse");
        assert_eq!(parsed.batch_id, "BID-1");
        assert_eq!(parsed.count_total, 3);
        assert_eq!(parsed.count_succeeded, 2);
        assert_eq!(parsed.count_errored, 1);
        assert_eq!(parsed.results.len(), 3);

        assert_eq!(parsed.results[0].status, "ok");
        assert_eq!(parsed.results[0].node_id, Some(Some("N0".to_owned())));
        // Null node_id is a documented success state, not a failure signal.
        assert_eq!(parsed.results[1].status, "ok");
        assert_eq!(parsed.results[1].node_id, Some(None));
        assert_eq!(parsed.results[2].status, "error");
        assert_eq!(parsed.results[2].error_code, Some(1605));
    }

    #[test]
    fn parse_batch_response_flat_envelope_from_production() {
        // Real production shape: result: true (bool) + batch data at the top
        // level, no `response` sub-object. Verifies we accept both shapes.
        let body = json!({
            "result": true,
            "batch_id": "prod-bid",
            "count_total": 2,
            "count_succeeded": 2,
            "count_errored": 0,
            "results": [
                {"index": 0, "filename": "a.txt", "status": "ok",
                 "upload_id": "U0", "node_id": null},
                {"index": 1, "filename": "b.txt", "status": "ok",
                 "upload_id": "U1", "node_id": null},
            ]
        });
        let parsed = parse_batch_response(&body, 200).expect("parse");
        assert_eq!(parsed.batch_id, "prod-bid");
        assert_eq!(parsed.count_total, 2);
        assert_eq!(parsed.count_succeeded, 2);
        assert_eq!(parsed.count_errored, 0);
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].node_id, Some(None));
    }

    #[test]
    fn parse_batch_response_whole_batch_error_surfaces_api_error() {
        let body = json!({
            "result": "no",
            "error": {"code": 1685, "text": "Batch contains more than 200 files."},
        });
        let err = parse_batch_response(&body, 403).expect_err("should error");
        match err {
            CliError::Api(api) => {
                assert_eq!(api.code, 1685);
                assert_eq!(api.http_status, 403);
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn relative_path_validation_rules() {
        assert!(validate_relative_path("ok/").is_ok());
        assert!(validate_relative_path("deep/nested/path/").is_ok());
        assert!(validate_relative_path("").is_err());
        assert!(validate_relative_path("no-trailing-slash").is_err());
        assert!(validate_relative_path("/leading-slash/").is_err());
        assert!(validate_relative_path("has/../parent/").is_err());
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("abc") — standard test vector.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn filename_validation_rules() {
        assert!(validate_filename("ok.txt").is_ok());
        assert!(validate_filename("a").is_ok());
        assert!(validate_filename(&"x".repeat(255)).is_ok());
        // Multi-byte UTF-8 filenames are bounded by char count, not byte count.
        assert!(validate_filename(&"猫".repeat(255)).is_ok());

        assert!(validate_filename("").is_err());
        assert!(validate_filename(&"x".repeat(256)).is_err());
        assert!(validate_filename(&"猫".repeat(256)).is_err());
        assert!(validate_filename(".").is_err());
        assert!(validate_filename("..").is_err());
        assert!(validate_filename("trailing.").is_err());
        assert!(validate_filename("trailing ").is_err());
        assert!(validate_filename("trailing\t").is_err());
        assert!(validate_filename("has\0null").is_err());
        assert!(validate_filename("has\rcr").is_err());
        assert!(validate_filename("has\nlf").is_err());
        assert!(validate_filename("has\x07bell").is_err());
        assert!(validate_filename("has/slash").is_err());
        assert!(validate_filename("has\\backslash").is_err());
        // Trojan-Source bidi override.
        assert!(validate_filename("safe\u{202E}evil.exe").is_err());
        // Zero-width space.
        assert!(validate_filename("hidden\u{200B}suffix").is_err());
    }

    #[test]
    fn creator_tag_validation_rules() {
        assert!(validate_creator_tag("my-importer").is_ok());
        assert!(validate_creator_tag("abc123").is_ok());
        assert!(validate_creator_tag(&"x".repeat(150)).is_ok());

        assert!(validate_creator_tag("").is_err());
        assert!(validate_creator_tag(&"x".repeat(151)).is_err());
        assert!(validate_creator_tag("has space").is_err());
        assert!(validate_creator_tag("has_underscore").is_err());
        assert!(validate_creator_tag("unicode-☃").is_err());
    }

    #[tokio::test]
    async fn upload_batch_rejects_empty_and_oversize_batches() {
        // Empty — reject before touching the network.
        let empty: Vec<BatchUploadItem> = Vec::new();
        let err = upload_batch("t", "https://x", "1", None, None, &empty)
            .await
            .expect_err("should reject");
        assert!(matches!(err, CliError::Parse(_)));

        // 201 items — reject before touching the network.
        let many: Vec<BatchUploadItem> = (0..=BATCH_MAX_FILES)
            .map(|i| item(&format!("f{i}"), 1))
            .collect();
        let err = upload_batch("t", "https://x", "1", None, None, &many)
            .await
            .expect_err("should reject");
        assert!(matches!(err, CliError::Parse(_)));
    }
}
