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
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::client::{ApiClient, build_password_header};
use crate::error::CliError;

/// Name of the request header carrying a File Share recipient's link password.
///
/// Mirrors the constant of the same value in `crate::client`; declared here too
/// because the raw-reqwest upload paths attach the header at their own
/// request-builder sites. The header VALUE is built by the single shared
/// [`crate::client::build_password_header`] seam (sensitive, UTF-8-capable);
/// only the header NAME is needed here.
const PASSWORD_HEADER: &str = "x-ve-password";

/// Parse a Fast.io error envelope from a raw write-back / chunk response body
/// into a [`CliError::Api`], tolerating the platform's shape variations.
///
/// The named-key write-back endpoints can return the message under either
/// `error.text` OR `error.message`, and the code as either a JSON number
/// (`"code": 400`) OR a string-encoded number (`"code": "400"`). This shared
/// parser (used by both [`single_shot_fileshare_writeback`] and
/// [`handle_chunk_response`]) accepts all of those; an unparseable / absent code
/// falls back to `0` (the HTTP status still drives the suggestion) and an absent
/// message falls back to `fallback_message`. Mirrors the client-layer
/// `ApiClient::extract_error` contract for the raw paths that cannot reach it.
#[must_use]
fn parse_writeback_error(body: &Value, http_status: u16, fallback_message: &str) -> CliError {
    let err = body.get("error");
    let message = err
        .and_then(|e| e.get("text").or_else(|| e.get("message")))
        .and_then(Value::as_str)
        .unwrap_or(fallback_message)
        .to_owned();
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(|c| {
            c.as_u64()
                .or_else(|| c.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    CliError::Api(crate::error::ApiError {
        code,
        error_code: None,
        message,
        http_status,
        details: None,
    })
}

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
    instance_id: &str,
    profile_type: &str,
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
            .text("instance_id", instance_id.to_owned())
            .text("folder_id", folder_id.to_owned())
            .text("profile_type", profile_type.to_owned())
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
                    details: None,
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
    instance_id: &str,
    profile_type: &str,
    folder_id: &str,
    filename: &str,
    filesize: u64,
) -> Result<Value, CliError> {
    let form = create_upload_session_form(instance_id, profile_type, folder_id, filename, filesize);
    client.post("/upload/", &form).await
}

/// Build the form body for [`create_upload_session`]. `profile_type` is
/// `workspace` or `share` and `instance_id` is the target workspace/share id.
fn create_upload_session_form(
    instance_id: &str,
    profile_type: &str,
    folder_id: &str,
    filename: &str,
    filesize: u64,
) -> HashMap<String, String> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), filename.to_owned());
    form.insert("size".to_owned(), filesize.to_string());
    form.insert("action".to_owned(), "create".to_owned());
    form.insert("instance_id".to_owned(), instance_id.to_owned());
    form.insert("folder_id".to_owned(), folder_id.to_owned());
    form.insert("profile_type".to_owned(), profile_type.to_owned());
    form
}

// ─── File Share write-back (external edit) ─────────────────────────────────
//
// A holder of an `edit` grant replaces a File Share's bound file content by
// targeting the FILE SHARE id as the update target of a normal upload session
// (`upload.txt:650-693`). These functions differ from the workspace upload
// helpers above in three ways: `action=update` (not `create`), `instance_id` is
// the File Share id (not a workspace id), NO `profile_type`/`folder_id` is sent,
// and an optional `if_version_id` compare-and-swap precondition is honored
// (File-Share-only; 400 on any other target). The share's read gate applies to
// EVERY step, so each helper threads the optional `x-ve-password` header.

/// Build the shared write-back session form fields.
///
/// `name` / `size` / `action=update` / `instance_id={fileshare_id}` /
/// `file_id={bound_node_id}` plus `if_version_id` only when `Some`. Deliberately
/// emits NEITHER `profile_type` NOR `folder_id` — the write-back contract omits
/// both (`upload.txt:657-666`). Shared by the session-create and single-shot
/// paths so the field set cannot drift between them.
#[must_use]
fn writeback_form_fields(
    fileshare_id: &str,
    bound_node_id: &str,
    filename: &str,
    filesize: u64,
    if_version_id: Option<&str>,
) -> HashMap<String, String> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), filename.to_owned());
    form.insert("size".to_owned(), filesize.to_string());
    form.insert("action".to_owned(), "update".to_owned());
    form.insert("instance_id".to_owned(), fileshare_id.to_owned());
    form.insert("file_id".to_owned(), bound_node_id.to_owned());
    if let Some(version) = if_version_id {
        form.insert("if_version_id".to_owned(), version.to_owned());
    }
    form
}

/// Create a chunked write-back upload session targeting a File Share's bound
/// file.
///
/// `POST /upload/` (form) with `action=update`, `instance_id={fileshare_id}`,
/// `file_id={bound_node_id}`, and optional `if_version_id`; NO `profile_type`.
/// Routed through [`ApiClient::post_with_password`] so the share's read gate
/// (link password) is satisfied. Do NOT reuse [`create_upload_session`] — it
/// hardcodes `action=create` / `profile_type=workspace`.
pub async fn create_fileshare_writeback_session(
    client: &ApiClient,
    fileshare_id: &str,
    bound_node_id: &str,
    filename: &str,
    filesize: u64,
    if_version_id: Option<&str>,
    password: Option<&SecretString>,
) -> Result<Value, CliError> {
    let form = writeback_form_fields(
        fileshare_id,
        bound_node_id,
        filename,
        filesize,
        if_version_id,
    );
    client.post_with_password("/upload/", &form, password).await
}

/// Replace a File Share's bound file in a TRUE single-shot multipart request.
///
/// `POST /upload/` with the chunk bytes attached to the INITIAL request
/// alongside the write-back form fields (`action=update`, `instance_id`,
/// `file_id`, optional `if_version_id`) and the optional `x-ve-password` header.
/// The session **auto-assembles** when the full chunk rides the initial request
/// — do NOT call `complete` afterwards (it returns a benign "not in a valid
/// state" error, `upload.txt:653`). Modeled on [`single_call_upload`]'s
/// multipart mechanics but with the write-back fields and password support, and
/// without the create-only `profile_type` / `folder_id`.
///
/// Returns the unwrapped response payload (the write-back `session` is at the
/// top level of the named-key envelope; see
/// [`crate::api::fileshare::extract_session`]).
///
/// **Idempotency / retry policy:** this is a TRUE single-shot upload — the full
/// file body rides the initial request and the server auto-assembles a new
/// version from it. The retry policy is therefore even narrower than
/// [`stream_upload`]: only **429** (the server rejected the request before
/// touching the body) and a **`is_connect`** failure (the TCP connection itself
/// could not be established, so no bytes reached the wire) are retried.
/// **`is_request`, 5xx, and timeouts are NOT retried** — once the whole body is
/// on the wire the server may have already assembled and written a new version
/// before returning a 500 (or before the client timed out / the in-flight
/// `inner.call(req)` errored), so a replay could create a duplicate version or,
/// with `if_version_id`, turn a silent success into a later CAS conflict. In
/// reqwest 0.12 a request error wraps `inner.call(req).await`, which can fire
/// while the body is mid-send — so it is excluded here even though
/// [`stream_upload`] (where the same hazard exists but the precedent is broader)
/// still retries it. The re-sendable chunked path
/// ([`upload_chunk_with_password`]) keeps its broader retry behavior.
// The write-back contract genuinely needs every field (target ids, filename,
// bytes, CAS precondition, password).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn single_shot_fileshare_writeback(
    token: &str,
    api_base: &str,
    fileshare_id: &str,
    bound_node_id: &str,
    filename: &str,
    file_data: Vec<u8>,
    if_version_id: Option<&str>,
    password: Option<&SecretString>,
) -> Result<Value, CliError> {
    let file_size = file_data.len() as u64;
    let url = format!("{}/upload/", api_base.trim_end_matches('/'));

    // The write-back client is built with `redirect(Policy::none())`
    // UNCONDITIONALLY: this path is always potentially password-bearing
    // (`x-ve-password`), and reqwest follows up to 10 redirects by default
    // WITHOUT stripping custom headers, so a stray 3xx would forward the link
    // password to the `Location` target. `.build()` surfaces its error rather
    // than falling back to a redirect-following client (H1/H2).
    let http_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(CHUNK_UPLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CHUNK_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    // L1: parse the optional password to a sensitive header value ONCE before
    // the retry loop (a bad value fails fast, without a panic) — the value is
    // cheaply cloned per attempt inside the loop. Uses the single shared
    // `client::build_password_header` seam (F2-9).
    let password_header = password.map(build_password_header).transpose()?;
    // L2: the multipart text fields derive from the single `writeback_form_fields`
    // source of truth so the field set cannot drift from the chunked path; the
    // file bytes are attached as the `chunk` part on top. `Bytes` clones O(1)
    // per retry (L7) instead of copying the whole buffer.
    let writeback_fields = writeback_form_fields(
        fileshare_id,
        bound_node_id,
        filename,
        file_size,
        if_version_id,
    );
    let file_bytes = Bytes::from(file_data);

    let mut attempt: u32 = 0;
    let mut backoff = CHUNK_INITIAL_BACKOFF;

    loop {
        let mut form = multipart::Form::new();
        for (key, value) in &writeback_fields {
            form = form.text(key.clone(), value.clone());
        }
        let chunk_len = file_bytes.len() as u64;
        let part = multipart::Part::stream_with_length(file_bytes.clone(), chunk_len)
            .file_name(filename.to_owned());
        let form = form.part("chunk", part);

        let mut request = http_client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(USER_AGENT, UPLOAD_USER_AGENT)
            .multipart(form);
        if let Some(value) = &password_header {
            request = request.header(PASSWORD_HEADER, value.clone());
        }

        match request.send().await {
            Ok(resp) => {
                let status = resp.status();

                // 429 is safe to retry — the server rejected the request before
                // processing the body, so no version was assembled.
                if status.as_u16() == 429 && attempt < CHUNK_MAX_RETRIES {
                    let retry_secs = parse_rate_limit_expiry(&resp);
                    attempt += 1;
                    eprintln!(
                        "{} Write-back rate limited (attempt {}/{CHUNK_MAX_RETRIES}). \
                         Waiting {retry_secs} seconds.",
                        "warning:".yellow().bold(),
                        attempt,
                    );
                    tokio::time::sleep(Duration::from_secs(retry_secs)).await;
                    continue;
                }

                // Do NOT retry 5xx — this is a single-shot, auto-assembling
                // write-back: the server may have already created a new version
                // from the body before returning an error. Replaying could
                // duplicate the version or convert an `if_version_id` success
                // into a later CAS conflict (mirrors `stream_upload`).

                // Fail closed on an unexpected redirect: the no-redirect client
                // did NOT follow it. Chasing it would forward any link password
                // (when one is present) to the `Location` target, and the client
                // is built no-redirect UNCONDITIONALLY — so the message must not
                // claim the upload was password-protected (it may not be). Secret-
                // and URL-free message (H1).
                if status.is_redirection() {
                    return Err(CliError::Parse(format!(
                        "File Share write-back upload cannot follow redirects \
                         (HTTP {}); refusing to follow it",
                        status.as_u16()
                    )));
                }

                let body: Value = resp.json().await.map_err(|e| {
                    CliError::Parse(format!("failed to parse write-back response: {e}"))
                })?;

                let result_ok = match body.get("result") {
                    Some(Value::String(s)) => s == "yes",
                    Some(Value::Bool(b)) => *b,
                    _ => false,
                };

                if result_ok {
                    // L6: this DELIBERATELY preserves the top-level `result` (the
                    // named-key write-back envelope; `extract_session` reads the
                    // top-level `session`), unlike `single_call_upload`, which
                    // unwraps/strips into a `new_file_id` shape. Only server
                    // bookkeeping (`current_api_version`) is removed.
                    let payload = if let Some(response_obj) = body.get("response") {
                        response_obj.clone()
                    } else {
                        let mut map = body;
                        if let Some(obj) = map.as_object_mut() {
                            obj.remove("current_api_version");
                        }
                        map
                    };
                    return Ok(payload);
                }

                return Err(parse_writeback_error(
                    &body,
                    status.as_u16(),
                    "File Share write-back failed",
                ));
            }
            Err(err) => {
                // Retry ONLY a defensibly-pre-send failure: `is_connect` (the
                // TCP connection itself failed) occurs before any bytes reach the
                // wire, so a replay cannot duplicate a version. `is_request` is
                // DELIBERATELY excluded — in reqwest 0.12 a request error wraps
                // failures from `inner.call(req).await`, which can fire WHILE the
                // body is being sent or the response awaited; for a non-idempotent
                // single-shot write-back that may have already created a version,
                // replaying on `is_request` risks a duplicate version or, with
                // `if_version_id`, converting a silent success into a later CAS
                // conflict. A timeout is likewise excluded — the full body may
                // have been sent and a new version assembled before the client
                // timed out. The classifiers OVERLAP in reqwest 0.12 (a timed-out
                // connect can ALSO report `is_connect`), so the leading
                // `!is_timeout()` guard keeps a post-send timeout out even on the
                // narrowed `is_connect()` predicate.
                let retryable =
                    writeback_send_error_is_retryable(err.is_timeout(), err.is_connect());
                if retryable && attempt < CHUNK_MAX_RETRIES {
                    attempt += 1;
                    eprintln!(
                        "{} Write-back connection error (attempt {}/{CHUNK_MAX_RETRIES}): \
                         {}. Retrying in {} seconds.",
                        "warning:".yellow().bold(),
                        attempt,
                        // URL-scrubbed (F2-10 rationale; the write-back path is
                        // password-bearing so the URL must never reach the log).
                        network_error_without_url(&err),
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

/// Decide whether a SEND error from the single-shot write-back is safe to retry.
///
/// Split out from [`single_shot_fileshare_writeback`] so the (security-sensitive)
/// retry predicate is unit-testable without fabricating a classified
/// `reqwest::Error` (reqwest exposes no constructor that pins `is_connect` /
/// `is_request` / `is_timeout`). Takes the relevant classifier flags directly.
///
/// Only `is_connect` (the TCP connection itself failed — no bytes on the wire)
/// is retryable, and only when NOT also a timeout (the reqwest 0.12 classifiers
/// overlap, so a timed-out connect can report `is_connect` too). `is_request` is
/// intentionally NOT a parameter: it is never retryable on this non-idempotent
/// single-shot path, because in reqwest 0.12 a request error wraps
/// `inner.call(req).await` and can fire mid-send after the server may already
/// have assembled a new version.
#[must_use]
fn writeback_send_error_is_retryable(is_timeout: bool, is_connect: bool) -> bool {
    !is_timeout && is_connect
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

/// Upload a single chunk of a File Share write-back session, threading the
/// optional `x-ve-password` header.
///
/// Identical to [`upload_chunk`] but attaches the optional recipient link
/// password (the share's read gate applies to every write-back step). Existing
/// non-write-back chunk uploads stay on [`upload_chunk`] (zero blast radius).
pub async fn upload_chunk_with_password(
    token: &str,
    api_base: &str,
    upload_id: &str,
    chunk_number: u32,
    chunk_data: Vec<u8>,
    password: Option<&SecretString>,
) -> Result<Value, CliError> {
    let chunk_size = chunk_data.len();
    let url = format!(
        "{}/upload/{}/chunk/?order={}&size={}",
        api_base.trim_end_matches('/'),
        urlencoding::encode(upload_id),
        chunk_number,
        chunk_size,
    );

    // Built with `redirect(Policy::none())` unconditionally — this path is
    // always potentially password-bearing, and reqwest does not strip
    // `x-ve-password` across a redirect; a stray 3xx fails closed in
    // `handle_chunk_response` instead of leaking the password (H1/H2).
    let http_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(CHUNK_UPLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CHUNK_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    // L1: parse the optional password ONCE before the retry loop; clone the
    // sensitive header value per attempt. Uses the single shared
    // `client::build_password_header` seam (F2-9).
    let password_header = password.map(build_password_header).transpose()?;

    let mut attempt: u32 = 0;
    let mut backoff = CHUNK_INITIAL_BACKOFF;

    loop {
        let part = multipart::Part::bytes(chunk_data.clone()).file_name("chunk.bin");
        let form = multipart::Form::new().part("chunk", part);

        let mut request = http_client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(USER_AGENT, UPLOAD_USER_AGENT)
            .multipart(form);
        if let Some(value) = &password_header {
            request = request.header(PASSWORD_HEADER, value.clone());
        }

        match request.send().await {
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

    // Fail closed on an unexpected redirect. The password-bearing chunk client
    // is built with `redirect(Policy::none())`, so a 3xx surfaces here rather
    // than being followed (which would forward the link password to the
    // `Location` target); the non-password chunk path never expects a redirect
    // on this endpoint either, so failing closed is correct for both. The
    // message embeds neither the secret nor any URL.
    if status.is_redirection() {
        return ChunkResult::Error(CliError::Parse(
            "the server returned an unexpected redirect during a chunk upload; \
             refusing to follow it"
                .to_owned(),
        ));
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

    // M1: shared tolerant parser (text|message, numeric|string-numeric code).
    ChunkResult::Error(parse_writeback_error(
        &body,
        status.as_u16(),
        "Chunk upload failed",
    ))
}

/// Render a [`reqwest::Error`] for a log line WITHOUT the request URL.
///
/// F2-10: `reqwest::Error`'s `Display` embeds the request URL, which would echo
/// credentials if `api_base` ever carried any (e.g. userinfo in the URL).
/// [`reqwest::Error::without_url`] consumes `self`, so it cannot be called on a
/// borrowed error; this helper reconstructs an equivalent URL-free message from
/// the error's `kind` flags plus its underlying source chain (the source carries
/// the real cause — "connection refused", a timeout, etc. — and never the URL).
/// Behavior-neutral: it only shapes a warning string.
fn network_error_without_url(err: &reqwest::Error) -> String {
    let kind = if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connection failed"
    } else if err.is_request() {
        "request error"
    } else {
        "network error"
    };
    // Walk the source chain for the concrete cause, which does not include the
    // request URL (only the top-level reqwest Display does).
    let mut cause: Option<String> = None;
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(s) = source {
        cause = Some(s.to_string());
        source = s.source();
    }
    match cause {
        Some(c) => format!("{kind}: {c}"),
        None => kind.to_owned(),
    }
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
            "{} Chunk {chunk_number} network error (attempt {}/{CHUNK_MAX_RETRIES}): {}. \
             Retrying in {} seconds.",
            "warning:".yellow().bold(),
            *attempt,
            // F2-10: scrub the request URL from the error text (see
            // `network_error_without_url`). Behavior-neutral (log-string only).
            network_error_without_url(err),
            backoff.as_secs(),
        );
        tokio::time::sleep(*backoff).await;
        *backoff = std::cmp::min(backoff.saturating_mul(2), CHUNK_MAX_BACKOFF);
        return true;
    }
    false
}

/// Parse the rate-limit expiry header to estimate seconds until reset.
///
/// Reads the modern `x-ve-limit-expires` header first, falling back to the
/// legacy `X-Rate-Limit-Expiry` for older API deployments. HTTP header lookups
/// are case-insensitive, so the literal casing does not matter. This mirrors
/// the centralized parser in `crate::client` (which is private there, so the
/// modern-with-legacy-fallback logic is duplicated here for the upload retry
/// paths — single/chunk/stream/batch all route through this one function).
fn parse_rate_limit_expiry(resp: &reqwest::Response) -> u64 {
    let raw = rate_limit_expiry_header(resp.headers());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    rate_limit_secs_from_expiry(raw, now)
}

/// Select the rate-limit expiry header value, preferring the modern
/// `x-ve-limit-expires` over the legacy `X-Rate-Limit-Expiry`.
///
/// Split out so the modern-over-legacy selection is unit-testable without
/// depending on the system clock (which [`parse_rate_limit_expiry`] reads).
fn rate_limit_expiry_header(headers: &reqwest::header::HeaderMap) -> Option<&str> {
    headers
        .get("x-ve-limit-expires")
        .or_else(|| headers.get("X-Rate-Limit-Expiry"))
        .and_then(|v| v.to_str().ok())
}

/// Convert a (possibly missing) rate-limit expiry epoch string into seconds
/// remaining relative to `now`. Falls back to 60s when absent or unparseable.
///
/// Split out from [`parse_rate_limit_expiry`] so the modern-with-legacy
/// header selection and the epoch arithmetic are unit-testable without a live
/// [`reqwest::Response`].
fn rate_limit_secs_from_expiry(raw: Option<&str>, now: u64) -> u64 {
    raw.and_then(|v| v.parse::<u64>().ok())
        .map_or(60, |expiry_epoch| expiry_epoch.saturating_sub(now))
}

/// Trigger file assembly after all chunks are uploaded.
///
/// `POST /upload/{upload_id}/complete/`
pub async fn complete_upload(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!("/upload/{}/complete/", urlencoding::encode(upload_id),);
    client.post(&path, &form).await
}

/// Trigger assembly for a File Share write-back session, threading the optional
/// `x-ve-password` header.
///
/// `POST /upload/{upload_id}/complete/`. The share's read gate applies to the
/// complete step too, so the optional link password is forwarded. Existing
/// non-write-back callers stay on [`complete_upload`].
pub async fn complete_upload_with_password(
    client: &ApiClient,
    upload_id: &str,
    password: Option<&SecretString>,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!("/upload/{}/complete/", urlencoding::encode(upload_id),);
    client.post_with_password(&path, &form, password).await
}

/// Get the current status of an upload session.
///
/// `GET /upload/{upload_id}/details/`
pub async fn get_upload_status(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let path = format!("/upload/{}/details/", urlencoding::encode(upload_id),);
    client.get(&path).await
}

/// Poll a File Share write-back session's status, threading the optional
/// `x-ve-password` header.
///
/// `GET /upload/{upload_id}/details/`. The share's read gate applies to status
/// polling too, so the optional link password is forwarded. Existing
/// non-write-back callers stay on [`get_upload_status`].
pub async fn get_upload_status_with_password(
    client: &ApiClient,
    upload_id: &str,
    password: Option<&SecretString>,
) -> Result<Value, CliError> {
    let path = format!("/upload/{}/details/", urlencoding::encode(upload_id),);
    client.get_with_password(&path, password).await
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
/// `GET /web_upload/` (the root handler serves the list — there is no
/// `/web_upload/list/` subpath). Optional `limit` / `offset` / `status` are
/// forwarded as query params.
pub async fn web_list(
    client: &ApiClient,
    limit: Option<u32>,
    offset: Option<u32>,
    status: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if let Some(s) = status {
        params.insert("status".to_owned(), s.to_owned());
    }
    if params.is_empty() {
        client.get("/web_upload/").await
    } else {
        client.get_with_params("/web_upload/", &params).await
    }
}

/// Cancel a web import job.
///
/// `DELETE /web_upload/?id=<upload_id>` — the id is a QUERY param on the root
/// handler (NOT a `/web_upload/{id}/` path segment).
pub async fn web_cancel(client: &ApiClient, upload_id: &str) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("id".to_owned(), upload_id.to_owned());
    client.delete_with_params("/web_upload/", &params).await
}

/// Get upload limits for the user's plan, optionally in a target context.
///
/// `GET /upload/limits/` — the optional query params resolve limits in the
/// context of a specific operation: `action` (`create` or `update`), `org`
/// (organization id, used when no `action` is given), `instance_id` (target
/// workspace or share id; required when `action` is `create` or `update`,
/// since the handler derives the profile type from it), `folder_id` (target
/// folder `OpaqueId` or `root`), and `file_id` (required when `action=update`).
pub async fn upload_limits(
    client: &ApiClient,
    action: Option<&str>,
    org: Option<&str>,
    instance_id: Option<&str>,
    folder_id: Option<&str>,
    file_id: Option<&str>,
) -> Result<Value, CliError> {
    let params = upload_limits_query(action, org, instance_id, folder_id, file_id);
    if params.is_empty() {
        client.get("/upload/limits/").await
    } else {
        client.get_with_params("/upload/limits/", &params).await
    }
}

/// Build the optional context query map for [`upload_limits`].
fn upload_limits_query(
    action: Option<&str>,
    org: Option<&str>,
    instance_id: Option<&str>,
    folder_id: Option<&str>,
    file_id: Option<&str>,
) -> HashMap<String, String> {
    let mut params = HashMap::new();
    if let Some(v) = action {
        params.insert("action".to_owned(), v.to_owned());
    }
    if let Some(v) = org {
        params.insert("org".to_owned(), v.to_owned());
    }
    if let Some(v) = instance_id {
        params.insert("instance_id".to_owned(), v.to_owned());
    }
    if let Some(v) = folder_id {
        params.insert("folder_id".to_owned(), v.to_owned());
    }
    if let Some(v) = file_id {
        params.insert("file_id".to_owned(), v.to_owned());
    }
    params
}

/// List the hash algorithms supported by the upload integrity-check flow.
///
/// `GET /upload/algos/` — returns `{ "algos": ["md5", "sha1", …] }`.
pub async fn algos(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/upload/algos/").await
}

/// Get restricted file extensions.
///
/// `GET /upload/limits/extensions/` — optional `plan` query selects the plan
/// whose extension limits to return.
pub async fn upload_extensions(client: &ApiClient, plan: Option<&str>) -> Result<Value, CliError> {
    if let Some(p) = plan {
        let mut params = HashMap::new();
        params.insert("plan".to_owned(), p.to_owned());
        client
            .get_with_params("/upload/limits/extensions/", &params)
            .await
    } else {
        client.get("/upload/limits/extensions/").await
    }
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
    instance_id: &str,
    profile_type: &str,
    folder_id: &str,
    filename: &str,
    max_size: Option<u64>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), filename.to_owned());
    form.insert("stream".to_owned(), "true".to_owned());
    form.insert("action".to_owned(), "create".to_owned());
    form.insert("instance_id".to_owned(), instance_id.to_owned());
    form.insert("folder_id".to_owned(), folder_id.to_owned());
    form.insert("profile_type".to_owned(), profile_type.to_owned());
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
        details: None,
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
            details: None,
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
    fn create_session_form_workspace_profile() {
        let f = create_upload_session_form("19", "workspace", "root", "a.txt", 1024);
        assert_eq!(f.get("instance_id").map(String::as_str), Some("19"));
        assert_eq!(f.get("profile_type").map(String::as_str), Some("workspace"));
        assert_eq!(f.get("folder_id").map(String::as_str), Some("root"));
        assert_eq!(f.get("size").map(String::as_str), Some("1024"));
        assert_eq!(f.get("action").map(String::as_str), Some("create"));
    }

    #[test]
    fn create_session_form_share_profile() {
        // Share-context upload: profile_type=share, instance_id=<share id>.
        let f = create_upload_session_form("55", "share", "fold1", "b.bin", 42);
        assert_eq!(f.get("instance_id").map(String::as_str), Some("55"));
        assert_eq!(f.get("profile_type").map(String::as_str), Some("share"));
    }

    #[test]
    fn upload_limits_query_empty_by_default() {
        assert!(upload_limits_query(None, None, None, None, None).is_empty());
    }

    #[test]
    fn upload_limits_query_create_context() {
        let q = upload_limits_query(Some("create"), None, Some("19"), Some("root"), None);
        assert_eq!(q.get("action").map(String::as_str), Some("create"));
        assert_eq!(q.get("instance_id").map(String::as_str), Some("19"));
        assert_eq!(q.get("folder_id").map(String::as_str), Some("root"));
        assert!(!q.contains_key("org"));
        assert!(!q.contains_key("file_id"));
    }

    #[test]
    fn upload_limits_query_update_context() {
        // The update context requires instance_id (profile-type source) plus
        // file_id; both must be forwarded as query params.
        let q = upload_limits_query(Some("update"), None, Some("19"), None, Some("file9"));
        assert_eq!(q.get("action").map(String::as_str), Some("update"));
        assert_eq!(q.get("instance_id").map(String::as_str), Some("19"));
        assert_eq!(q.get("file_id").map(String::as_str), Some("file9"));
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

    /// Build a `HeaderMap` carrying the given headers so the modern-over-legacy
    /// selection in `rate_limit_expiry_header` can be exercised directly
    /// (no live `reqwest::Response` needed).
    fn headers_with(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            let name = HeaderName::from_bytes(k.as_bytes()).expect("valid header name");
            let value = HeaderValue::from_str(v).expect("valid header value");
            map.insert(name, value);
        }
        map
    }

    #[test]
    fn rate_limit_header_prefers_modern_over_legacy() {
        // FIX 6: the platform now emits x-ve-limit-expires; it must win over
        // the legacy X-Rate-Limit-Expiry when both are present.
        let headers = headers_with(&[
            ("x-ve-limit-expires", "1000"),
            ("X-Rate-Limit-Expiry", "2000"),
        ]);
        assert_eq!(rate_limit_expiry_header(&headers), Some("1000"));
    }

    #[test]
    fn rate_limit_header_reads_modern_when_only_modern_present() {
        let headers = headers_with(&[("x-ve-limit-expires", "1500")]);
        assert_eq!(rate_limit_expiry_header(&headers), Some("1500"));
    }

    #[test]
    fn rate_limit_header_falls_back_to_legacy() {
        let headers = headers_with(&[("X-Rate-Limit-Expiry", "2000")]);
        assert_eq!(rate_limit_expiry_header(&headers), Some("2000"));
        // No headers at all → None (the parse path then defaults to 60s).
        let none = headers_with(&[]);
        assert_eq!(rate_limit_expiry_header(&none), None);
        assert_eq!(
            rate_limit_secs_from_expiry(rate_limit_expiry_header(&none), 1000),
            60
        );
    }

    #[test]
    fn rate_limit_secs_from_expiry_arithmetic() {
        // Future epoch: returns the difference.
        assert_eq!(rate_limit_secs_from_expiry(Some("1100"), 1000), 100);
        // Past epoch: saturates to 0.
        assert_eq!(rate_limit_secs_from_expiry(Some("900"), 1000), 0);
        // Missing / unparseable: default 60.
        assert_eq!(rate_limit_secs_from_expiry(None, 1000), 60);
        assert_eq!(rate_limit_secs_from_expiry(Some("abc"), 1000), 60);
    }

    #[test]
    fn writeback_form_has_update_fields_and_omits_create_fields() {
        let form = writeback_form_fields("FS1", "node1", "v2.pdf", 1024, None);
        assert_eq!(form.get("action").map(String::as_str), Some("update"));
        assert_eq!(form.get("instance_id").map(String::as_str), Some("FS1"));
        assert_eq!(form.get("file_id").map(String::as_str), Some("node1"));
        assert_eq!(form.get("name").map(String::as_str), Some("v2.pdf"));
        assert_eq!(form.get("size").map(String::as_str), Some("1024"));
        // Write-back must NEVER send the create-only fields.
        assert!(
            !form.contains_key("profile_type"),
            "write-back must not send profile_type"
        );
        assert!(
            !form.contains_key("folder_id"),
            "write-back must not send folder_id"
        );
        // if_version_id is omitted when None.
        assert!(!form.contains_key("if_version_id"));
    }

    #[test]
    fn writeback_form_includes_if_version_id_only_when_some() {
        let with = writeback_form_fields("FS1", "node1", "v2.pdf", 10, Some("v7"));
        assert_eq!(with.get("if_version_id").map(String::as_str), Some("v7"));
        let without = writeback_form_fields("FS1", "node1", "v2.pdf", 10, None);
        assert!(!without.contains_key("if_version_id"));
    }

    #[tokio::test]
    async fn network_error_without_url_omits_the_request_url() {
        // F3-8(a): the write-back log path is password-bearing, so the rendered
        // network-error warning must NEVER include the request URL (which could
        // echo userinfo if `api_base` ever carried any). Drive a REAL connect
        // failure to a refused port with a recognizable URL marker, then assert
        // the helper's output omits it (while the raw Display would include it).
        let marker = "URLMARKER-do-not-leak";
        let url = format!("http://127.0.0.1:1/{marker}");
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client builds");
        let err = client
            .get(&url)
            .send()
            .await
            .expect_err("connect to port 1 must fail");

        let rendered = network_error_without_url(&err);
        assert!(
            !rendered.contains(marker),
            "the URL marker must not appear in the scrubbed warning: {rendered}"
        );
        // Sanity: the raw reqwest Display DOES embed the URL (so the helper is
        // doing real work, not trivially passing because reqwest changed).
        assert!(
            err.to_string().contains(marker),
            "raw reqwest Display is expected to carry the URL (regression guard): {err}"
        );
    }

    #[test]
    fn writeback_send_error_retry_predicate_is_connect_only() {
        // F3-1: the single-shot write-back is non-idempotent, so the SEND-error
        // retry predicate must be NARROW. A pure `is_connect` failure (no bytes
        // on the wire) is the only retryable case, and only when it is NOT also
        // a timeout.
        assert!(
            writeback_send_error_is_retryable(false, true),
            "a pure connect failure is retryable"
        );
        // A timeout-classified connect must NOT retry (overlapping classifiers).
        assert!(
            !writeback_send_error_is_retryable(true, true),
            "a timed-out connect must not retry (body may have been sent)"
        );
        // A pure timeout (no connect) must NOT retry.
        assert!(
            !writeback_send_error_is_retryable(true, false),
            "a timeout must not retry"
        );
        // Neither flag (e.g. an `is_request`-only error, which is not a parameter
        // here BECAUSE it is never retryable) must NOT retry.
        assert!(
            !writeback_send_error_is_retryable(false, false),
            "a non-connect, non-timeout send error (e.g. is_request) must not retry"
        );
    }

    #[test]
    fn upload_password_header_accepts_utf8_and_marks_sensitive() {
        // F2-5/F2-9: the upload paths reuse the single shared
        // `client::build_password_header` seam, which is built from the
        // password BYTES so a valid non-ASCII UTF-8 link password (settable via
        // the management form) is sendable rather than rejected. Round-trips and
        // is marked sensitive.
        let pw = SecretString::from("pässwört→".to_owned());
        let value = build_password_header(&pw).expect("utf-8 password header");
        assert!(
            value.is_sensitive(),
            "the password header value must be marked sensitive"
        );
        assert_eq!(value.as_bytes(), "pässwört→".as_bytes());
    }

    #[test]
    fn upload_password_header_rejects_control_chars_without_leaking() {
        // A newline still cannot be a header value — must fail without a panic,
        // name only the header, and never echo the secret.
        let bad = SecretString::from("line1\nSECRET-LEAK".to_owned());
        let err = build_password_header(&bad).expect_err("control char must be rejected");
        match &err {
            CliError::InvalidHeaderValue { header } => assert_eq!(*header, PASSWORD_HEADER),
            other => panic!("expected InvalidHeaderValue, got {other:?}"),
        }
        assert!(
            !err.to_string().contains("SECRET-LEAK"),
            "the secret must never appear in the error: {err}"
        );
    }

    #[test]
    fn parse_writeback_error_accepts_text_message_and_string_codes() {
        // `text` + numeric code.
        let a = serde_json::json!({"error": {"code": 1605, "text": "bad node"}});
        let CliError::Api(err) = parse_writeback_error(&a, 400, "fallback") else {
            panic!("expected Api error");
        };
        assert_eq!(err.code, 1605);
        assert_eq!(err.message, "bad node");
        assert_eq!(err.http_status, 400);

        // `message` (not `text`) + string-encoded code.
        let b = serde_json::json!({"error": {"code": "1700", "message": "tier too low"}});
        let CliError::Api(err) = parse_writeback_error(&b, 403, "fallback") else {
            panic!("expected Api error");
        };
        assert_eq!(err.code, 1700, "string-encoded code must parse");
        assert_eq!(err.message, "tier too low");

        // Absent error object → fallback message, code 0, status preserved.
        let c = serde_json::json!({"result": false});
        let CliError::Api(err) = parse_writeback_error(&c, 500, "fallback") else {
            panic!("expected Api error");
        };
        assert_eq!(err.code, 0);
        assert_eq!(err.message, "fallback");
        assert_eq!(err.http_status, 500);

        // Unparseable string code → 0 (no panic).
        let d = serde_json::json!({"error": {"code": "nope", "text": "x"}});
        let CliError::Api(err) = parse_writeback_error(&d, 400, "fallback") else {
            panic!("expected Api error");
        };
        assert_eq!(err.code, 0);
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

    /// Serve a fixed HTTP/1.1 response (`status_line` + optional `Location`) on
    /// every accepted connection, COUNTING each request, until the test drops the
    /// returned handle. Returns the bound `127.0.0.1:<port>` and a shared counter.
    ///
    /// Adapts the `client.rs` one-shot loopback pattern (F3-4) but accepts in a
    /// loop and counts requests so the test can prove the single-shot write-back
    /// makes EXACTLY ONE request (no replay) on a terminal 5xx / 3xx — if the
    /// code wrongly retried, the counter would exceed 1.
    async fn spawn_counting_server(
        status_line: &'static str,
        location: Option<&'static str>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let count = Arc::new(AtomicUsize::new(0));
        let count_srv = Arc::clone(&count);
        tokio::spawn(async move {
            // Accept in a loop so a (buggy) replay attempt would be served and
            // counted rather than hanging.
            while let Ok((mut sock, _)) = listener.accept().await {
                let _ = count_srv.fetch_add(1, Ordering::SeqCst);
                // Drain the FULL request before responding. A single `read`
                // leaves the multipart/form body undrained; under parallel test
                // load the client's still-in-flight write then races our close
                // and gets a TCP RST, so `resp.json()` fails with
                // `CliError::Parse` instead of the asserted error. We read in a
                // loop until the header terminator (`\r\n\r\n`) is seen, parse the
                // declared `Content-Length`, then keep reading until that many
                // body bytes are consumed (or the peer closes / read returns 0).
                let mut req_buf: Vec<u8> = Vec::with_capacity(4096);
                let mut chunk = [0u8; 4096];
                let mut header_end: Option<usize> = None;
                let mut content_length: Option<usize> = None;
                loop {
                    // Once headers are fully parsed, stop as soon as the declared
                    // body has been drained (Content-Length: 0 → done immediately).
                    if let (Some(he), Some(cl)) = (header_end, content_length)
                        && req_buf.len() >= he + cl
                    {
                        break;
                    }
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break, // peer closed or read error → stop draining
                        Ok(n) => {
                            req_buf.extend_from_slice(&chunk[..n]);
                            if header_end.is_none()
                                && let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n")
                            {
                                let he = pos + 4;
                                header_end = Some(he);
                                // Case-insensitively parse `Content-Length`.
                                let headers =
                                    String::from_utf8_lossy(&req_buf[..he]).to_ascii_lowercase();
                                content_length = headers
                                    .lines()
                                    .find_map(|line| {
                                        line.strip_prefix("content-length:")
                                            .map(str::trim)
                                            .and_then(|v| v.parse::<usize>().ok())
                                    })
                                    .or(Some(0));
                            }
                        }
                    }
                }
                let body = br#"{"result":false,"error":{"code":500,"text":"boom"}}"#;
                let header = match location {
                    Some(loc) => format!(
                        "HTTP/1.1 {status_line}\r\nLocation: {loc}\r\n\
                         Content-Length: 0\r\nConnection: close\r\n\r\n",
                    ),
                    None => format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    ),
                };
                let _ = sock.write_all(header.as_bytes()).await;
                if location.is_none() {
                    let _ = sock.write_all(body).await;
                }
                let _ = sock.flush().await;
            }
        });
        (addr, count)
    }

    #[tokio::test]
    async fn single_shot_writeback_does_not_retry_5xx_terminal_after_one_request() {
        use std::sync::atomic::Ordering;
        // F3-4: a 500 on the single-shot, auto-assembling write-back must be
        // TERMINAL — surfaced as CliError::Api after EXACTLY ONE request. The
        // server may already have written a new version, so a replay is unsafe;
        // the request counter proves no replay happened.
        let (addr, count) = spawn_counting_server("500 Internal Server Error", None).await;

        let err = single_shot_fileshare_writeback(
            "tok",
            &format!("http://{addr}"),
            "FS1",
            "node1",
            "v2.bin",
            b"hello".to_vec(),
            None,
            None,
        )
        .await
        .expect_err("a 5xx must surface as a terminal error");

        match err {
            CliError::Api(api) => assert_eq!(api.http_status, 500),
            other => panic!("expected terminal CliError::Api, got {other:?}"),
        }
        // The critical safety property: the body was sent ONCE — no replay.
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "single-shot write-back must NOT retry a 5xx (the server may have \
             already assembled a new version)"
        );
    }

    #[tokio::test]
    async fn single_shot_writeback_fails_closed_on_redirect_without_following() {
        use std::sync::atomic::Ordering;
        // F3-4 (also H1): the no-redirect client must FAIL CLOSED on a 3xx —
        // never chase the Location (which would forward the link password) and
        // never replay. One request, then a clear non-following error.
        let (addr, count) = spawn_counting_server(
            "307 Temporary Redirect",
            Some("http://example.invalid/leak"),
        )
        .await;

        let err = single_shot_fileshare_writeback(
            "tok",
            &format!("http://{addr}"),
            "FS1",
            "node1",
            "v2.bin",
            b"hello".to_vec(),
            None,
            Some(&SecretString::from("pw".to_owned())),
        )
        .await
        .expect_err("a 3xx must fail closed, not be followed");

        // Fail-closed redirect surfaces as a Parse error (not an Api error), and
        // its message names neither the secret nor a URL.
        match &err {
            CliError::Parse(msg) => {
                assert!(
                    !msg.contains("pw") && !msg.contains("example.invalid"),
                    "redirect error must not leak the password or the Location URL: {msg}"
                );
            }
            other => panic!("expected a fail-closed Parse error, got {other:?}"),
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "a redirect must be served exactly once and not chased/replayed"
        );
    }
}
