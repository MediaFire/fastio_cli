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
use serde_json::Value;

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
