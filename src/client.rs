#![allow(clippy::missing_errors_doc)]

/// HTTP client wrapper for the Fast.io REST API.
///
/// Handles request construction, authentication header injection,
/// response envelope unwrapping, rate-limit detection, and automatic
/// retry with exponential backoff for transient network failures.
use std::collections::HashMap;
use std::time::Duration;

use colored::Colorize;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{ApiError, CliError};

/// User-Agent string sent on every outgoing request.
const CLIENT_USER_AGENT: &str = concat!("fastio-cli/", env!("CARGO_PKG_VERSION"));

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Threshold below which a rate-limit warning is emitted.
const RATE_LIMIT_LOW_THRESHOLD: u64 = 5;

/// Maximum number of retries for transient network failures.
const MAX_RETRIES: u32 = 3;

/// Initial backoff delay between retries.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Maximum connection pool idle timeout in seconds.
const POOL_IDLE_TIMEOUT_SECS: u64 = 90;

/// Maximum number of idle connections per host in the pool.
const POOL_MAX_IDLE_PER_HOST: usize = 10;

/// Detail level for the server-side `?output=markdown` modifier.
///
/// Passed to [`ApiClient::get_markdown`] to control how verbose the
/// server's markdown rendering is. The server accepts exactly these three
/// tokens, combined with `markdown` as `?output=<detail>,markdown`; using
/// an enum instead of a free-form string enforces the contract at the
/// type level so callers cannot accidentally send an unrecognized token.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MarkdownDetail {
    /// Minimal output: identifiers and headline fields only.
    Terse,
    /// Default output: the fields most clients need.
    Standard,
    /// Full output: all available fields, including administrative metadata.
    Full,
}

impl MarkdownDetail {
    /// Server query token for this detail level.
    fn as_str(self) -> &'static str {
        match self {
            Self::Terse => "terse",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }
}

/// HTTP client that wraps `reqwest` with Fast.io-specific conventions.
pub struct ApiClient {
    /// The underlying HTTP client.
    inner: reqwest::Client,
    /// Base URL for all API requests (e.g. `https://api.fast.io/current`).
    base_url: String,
    /// Bearer token for authentication, stored securely and zeroized on drop.
    token: Option<SecretString>,
}

impl ApiClient {
    /// Create a new client targeting `base_url` with an optional bearer token.
    ///
    /// The token is wrapped in [`SecretString`] to prevent accidental logging
    /// and to zeroize memory on drop.
    pub fn new(base_url: &str, token: Option<String>) -> Result<Self, CliError> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_USER_AGENT));

        let inner = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .build()?;

        Ok(Self {
            inner,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token: token.map(SecretString::from),
        })
    }

    /// Replace the bearer token used for subsequent requests.
    #[allow(dead_code)]
    pub fn set_token(&mut self, token: String) {
        self.token = Some(SecretString::from(token));
    }

    /// Return the current bearer token as a plain string, if any.
    ///
    /// Callers should avoid logging or persisting the returned value.
    pub fn get_token(&self) -> Option<&str> {
        self.token
            .as_ref()
            .map(secrecy::ExposeSecret::expose_secret)
    }

    /// Return the base URL for this client.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Build the full URL for a relative path.
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// Perform a GET request that asks the server for a markdown-rendered
    /// response via the `?output=markdown` modifier, and returns the raw
    /// markdown body as a string.
    ///
    /// The server contract (documented at
    /// `https://api.fast.io/current/llms/full/`) guarantees that every
    /// endpoint which returns a JSON envelope also supports this modifier,
    /// emitting `Content-Type: text/markdown; charset=UTF-8`. Error envelopes
    /// render as markdown too when markdown was requested; on non-2xx HTTP
    /// statuses, the body is surfaced as `CliError::Api.message` (capped at
    /// `ERROR_MESSAGE_MAX_BYTES`).
    ///
    /// `detail` selects the server's markdown verbosity and is combined
    /// with `markdown` as `?output=<detail>,markdown`. Using
    /// [`MarkdownDetail`] instead of a free-form string guarantees the
    /// server only ever sees the three documented tokens.
    /// Any `output` key present in `params` is dropped to prevent the
    /// server from receiving duplicate `output=` query parameters.
    ///
    /// Currently no handler calls this method; it is infrastructure that
    /// lets a future tool or command opt into server-authoritative markdown
    /// instead of the client-side renderer.
    #[allow(dead_code)]
    pub async fn get_markdown(
        &self,
        path: &str,
        params: Option<&HashMap<String, String>>,
        detail: Option<MarkdownDetail>,
    ) -> Result<String, CliError> {
        let output_value = match detail {
            Some(d) => format!("{},markdown", d.as_str()),
            None => "markdown".to_owned(),
        };
        tracing::trace!(method = "GET", path, output = %output_value, "api request (markdown)");
        // Drop any caller-supplied `output` key (case-insensitive) so the
        // server never sees two `output=` query parameters.
        let filtered_params: Option<Vec<(&str, &str)>> = params.map(|p| {
            p.iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case("output"))
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect()
        });
        self.send_raw_text_with_retry(|| {
            let mut req = self
                .inner
                .get(self.url(path))
                .query(&[("output", output_value.as_str())]);
            if let Some(ref p) = filtered_params {
                req = req.query(p);
            }
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Build the `Authorization: Bearer <token>` header value.
    fn auth_header(&self) -> Option<String> {
        self.token
            .as_ref()
            .map(|t| format!("Bearer {}", t.expose_secret()))
    }

    /// Perform a GET request and unwrap the API envelope.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.get(self.url(path));
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a GET request with query parameters.
    #[allow(dead_code)]
    pub async fn get_with_params<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, ?params, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.get(self.url(path)).query(params);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a GET request with a custom `Authorization` header (e.g. Basic auth).
    pub async fn get_with_auth<T: DeserializeOwned>(
        &self,
        path: &str,
        auth_value: &str,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, "api request (custom auth)");
        let auth_owned = auth_value.to_owned();
        self.send_with_retry(|| {
            self.inner
                .get(self.url(path))
                .header(AUTHORIZATION, auth_owned.clone())
        })
        .await
    }

    /// Perform a GET request with a custom `Authorization` header and query parameters.
    #[allow(dead_code)]
    pub async fn get_with_auth_and_params<T: DeserializeOwned>(
        &self,
        path: &str,
        auth_value: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, ?params, "api request (custom auth)");
        let auth_owned = auth_value.to_owned();
        self.send_with_retry(|| {
            self.inner
                .get(self.url(path))
                .header(AUTHORIZATION, auth_owned.clone())
                .query(params)
        })
        .await
    }

    /// Perform a GET request with query parameters but no authentication.
    pub async fn get_no_auth_with_params<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, ?params, "api request (no auth)");
        self.send_with_retry(|| self.inner.get(self.url(path)).query(params))
            .await
    }

    /// Perform a form-encoded POST and unwrap the API envelope.
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, ?form, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.post(self.url(path)).form(form);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a form-encoded POST without authentication.
    pub async fn post_no_auth<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, ?form, "api request (no auth)");
        self.send_with_retry(|| self.inner.post(self.url(path)).form(form))
            .await
    }

    /// Perform a form-encoded POST without authentication, returning the raw
    /// JSON response without Fast.io envelope unwrapping.
    ///
    /// Use this for endpoints (e.g. `/oauth/token/`) that return a standard
    /// response body instead of the Fast.io `{"result": …}` envelope.
    pub async fn post_no_auth_raw<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, ?form, "api request (no auth, raw)");
        self.send_with_retry_raw(|| self.inner.post(self.url(path)).form(form))
            .await
    }

    /// Perform a JSON POST and unwrap the API envelope.
    #[allow(dead_code)]
    pub async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, body = %body, "api request (json)");
        self.send_with_retry(|| {
            let mut req = self.inner.post(self.url(path)).json(body);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a DELETE request and unwrap the API envelope.
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        tracing::trace!(method = "DELETE", path, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.delete(self.url(path));
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a DELETE request with query parameters.
    #[allow(dead_code)]
    pub async fn delete_with_params<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "DELETE", path, ?params, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.delete(self.url(path)).query(params);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a form-encoded DELETE and unwrap the API envelope.
    pub async fn delete_with_form<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "DELETE", path, ?form, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.delete(self.url(path)).form(form);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Send a request with automatic retry and exponential backoff for transient errors.
    ///
    /// Retries on connection errors, timeouts, and HTTP 502/503/504 responses.
    /// Does not retry on client errors (4xx) or successful responses.
    async fn send_with_retry<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut last_error: Option<CliError> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let backoff = INITIAL_BACKOFF * 2u32.saturating_pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                    "retrying request after transient failure"
                );
                tokio::time::sleep(backoff).await;
            }

            let req = build_request();
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();

                    // Retry on server gateway errors.
                    if matches!(status.as_u16(), 502..=504) && attempt < MAX_RETRIES {
                        tracing::warn!(
                            status = status.as_u16(),
                            "received transient server error, will retry"
                        );
                        // error_for_status() always returns Err for 5xx codes.
                        if let Err(e) = resp.error_for_status() {
                            last_error = Some(CliError::Http(e));
                        }
                        continue;
                    }

                    return self.handle_response(resp).await;
                }
                Err(e) if Self::is_retryable_error(&e) && attempt < MAX_RETRIES => {
                    tracing::warn!(error = %e, "transient network error, will retry");
                    last_error = Some(CliError::Http(e));
                }
                Err(e) => return Err(CliError::Http(e)),
            }
        }

        // All retries exhausted - return the last error.
        // `last_error` is always `Some` here because the loop body sets it
        // on every retryable failure, but we handle `None` defensively.
        Err(last_error
            .unwrap_or_else(|| CliError::Parse("request failed: all retries exhausted".to_owned())))
    }

    /// Send a request with retry, returning the raw JSON response without
    /// envelope unwrapping.
    async fn send_with_retry_raw<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut last_error: Option<CliError> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let backoff = INITIAL_BACKOFF * 2u32.saturating_pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                    "retrying request after transient failure"
                );
                tokio::time::sleep(backoff).await;
            }

            let req = build_request();
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();

                    if matches!(status.as_u16(), 502..=504) && attempt < MAX_RETRIES {
                        tracing::warn!(
                            status = status.as_u16(),
                            "received transient server error, will retry"
                        );
                        if let Err(e) = resp.error_for_status() {
                            last_error = Some(CliError::Http(e));
                        }
                        continue;
                    }

                    return self.handle_response_raw(resp).await;
                }
                Err(e) if Self::is_retryable_error(&e) && attempt < MAX_RETRIES => {
                    tracing::warn!(error = %e, "transient network error, will retry");
                    last_error = Some(CliError::Http(e));
                }
                Err(e) => return Err(CliError::Http(e)),
            }
        }

        Err(last_error
            .unwrap_or_else(|| CliError::Parse("request failed: all retries exhausted".to_owned())))
    }

    /// Send a request with retry that returns the raw response body as text
    /// (no JSON parse, no envelope unwrap).
    ///
    /// Used by the markdown fetch path: the server emits
    /// `Content-Type: text/markdown; charset=UTF-8` which cannot be parsed as
    /// JSON. Non-success HTTP statuses are surfaced as `CliError::Api` with
    /// the markdown body included as the error message, matching the
    /// behavior of `handle_response_raw` for JSON error responses. (The
    /// server renders error envelopes as markdown when `?output=markdown`
    /// is set, so the message is still human-readable.)
    async fn send_raw_text_with_retry<F>(&self, build_request: F) -> Result<String, CliError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut last_error: Option<CliError> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let backoff = INITIAL_BACKOFF * 2u32.saturating_pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                    "retrying request after transient failure"
                );
                tokio::time::sleep(backoff).await;
            }

            let req = build_request();
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();

                    if status.as_u16() == 429 {
                        let retry_secs = Self::parse_rate_limit_expiry(&resp);
                        Self::emit_rate_limit_error(retry_secs);
                        return Err(CliError::RateLimit {
                            retry_after_secs: retry_secs,
                        });
                    }

                    if matches!(status.as_u16(), 502..=504) && attempt < MAX_RETRIES {
                        tracing::warn!(
                            status = status.as_u16(),
                            "received transient server error, will retry"
                        );
                        if let Err(e) = resp.error_for_status_ref() {
                            last_error = Some(CliError::Http(e));
                        }
                        continue;
                    }

                    Self::check_rate_limit(&resp);

                    let http_status = status.as_u16();
                    let body = resp.text().await.map_err(|e| {
                        CliError::Parse(format!("failed to read response body: {e}"))
                    })?;

                    if !status.is_success() {
                        let message = if body.trim().is_empty() {
                            format!("API request failed with HTTP {http_status}")
                        } else {
                            Self::truncate_for_error_message(&body)
                        };
                        return Err(CliError::Api(ApiError {
                            code: 0,
                            error_code: None,
                            message,
                            http_status,
                        }));
                    }

                    return Ok(body);
                }
                Err(e) if Self::is_retryable_error(&e) && attempt < MAX_RETRIES => {
                    tracing::warn!(error = %e, "transient network error, will retry");
                    last_error = Some(CliError::Http(e));
                }
                Err(e) => return Err(CliError::Http(e)),
            }
        }

        Err(last_error
            .unwrap_or_else(|| CliError::Parse("request failed: all retries exhausted".to_owned())))
    }

    /// Maximum length of an error-response body included in `ApiError.message`.
    ///
    /// Non-JSON error paths (markdown, HTML gateway pages) can produce very
    /// large bodies. Without a cap the body flows verbatim to stderr and log
    /// sinks via `Display`, which is hostile to both terminals and log
    /// pipelines. 8 KB keeps multi-paragraph markdown errors readable.
    const ERROR_MESSAGE_MAX_BYTES: usize = 8 * 1024;

    /// Return the input when short; otherwise return an 8 KB prefix with a
    /// trailing `… [truncated, N more bytes]` marker (U+2026 HORIZONTAL
    /// ELLIPSIS, not three ASCII dots). Slicing is UTF-8-safe: the cut
    /// point is walked back to a char boundary.
    fn truncate_for_error_message(body: &str) -> String {
        if body.len() <= Self::ERROR_MESSAGE_MAX_BYTES {
            return body.to_owned();
        }
        let mut cut = Self::ERROR_MESSAGE_MAX_BYTES;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        let remaining = body.len() - cut;
        format!("{}\n… [truncated, {remaining} more bytes]", &body[..cut])
    }

    /// Determine whether a `reqwest::Error` is transient and worth retrying.
    fn is_retryable_error(err: &reqwest::Error) -> bool {
        err.is_timeout() || err.is_connect() || err.is_request()
    }

    /// Process an API response: check rate limits, unwrap envelope.
    async fn handle_response<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, CliError> {
        let status = resp.status();
        tracing::trace!(status = status.as_u16(), "api response");

        // Check for HTTP 429 before attempting to parse body.
        if status.as_u16() == 429 {
            let retry_secs = Self::parse_rate_limit_expiry(&resp);
            Self::emit_rate_limit_error(retry_secs);
            return Err(CliError::RateLimit {
                retry_after_secs: retry_secs,
            });
        }

        Self::check_rate_limit(&resp);

        let body: Value = resp
            .json()
            .await
            .map_err(|e| CliError::Parse(format!("failed to parse API response: {e}")))?;
        tracing::trace!(body = %body, "api response body");

        // The Fast.io envelope uses "yes"/"no" strings (or bool true/false in some endpoints).
        let result_ok = match body.get("result") {
            Some(Value::String(s)) => s == "yes",
            Some(Value::Bool(b)) => *b,
            _ => false,
        };

        if !result_ok {
            return Err(Self::extract_error(&body, status.as_u16()).into());
        }

        // Unwrap behavior:
        //   - If the body has a `response` sub-object, return that. Callers
        //     that deserialize into concrete structs expect the payload
        //     already unwrapped.
        //   - Otherwise, preserve the full envelope (including `result`)
        //     so downstream renderers — in particular the markdown
        //     renderer, which needs `result` to produce the
        //     `**Result:** success|failure` preamble — receive the server
        //     envelope verbatim. `current_api_version` is dropped because
        //     it's server-bookkeeping, not payload.
        let payload = if let Some(response_obj) = body.get("response") {
            response_obj.clone()
        } else {
            let mut map = body;
            if let Some(obj) = map.as_object_mut() {
                obj.remove("current_api_version");
            }
            map
        };

        serde_json::from_value(payload)
            .map_err(|e| CliError::Parse(format!("failed to deserialize response: {e}")))
    }

    /// Process an API response without envelope unwrapping.
    ///
    /// Checks for rate limits and HTTP errors, then deserializes the full
    /// JSON body directly into `T`.
    async fn handle_response_raw<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, CliError> {
        let status = resp.status();
        tracing::trace!(status = status.as_u16(), "api response (raw)");

        if status.as_u16() == 429 {
            let retry_secs = Self::parse_rate_limit_expiry(&resp);
            Self::emit_rate_limit_error(retry_secs);
            return Err(CliError::RateLimit {
                retry_after_secs: retry_secs,
            });
        }

        Self::check_rate_limit(&resp);

        if !status.is_success() {
            let body: Value = resp.json().await.unwrap_or_default();
            tracing::trace!(body = %body, "api error response body (raw)");
            return Err(Self::extract_error(&body, status.as_u16()).into());
        }

        let body_text = resp
            .text()
            .await
            .map_err(|e| CliError::Parse(format!("failed to read response body: {e}")))?;
        tracing::trace!(body = %body_text, "api response body (raw)");

        serde_json::from_str(&body_text)
            .map_err(|e| CliError::Parse(format!("failed to deserialize response: {e}")))
    }

    /// Extract a structured API error from the response body.
    fn extract_error(body: &Value, http_status: u16) -> ApiError {
        if let Some(err) = body.get("error") {
            let code = err.get("code").and_then(Value::as_u64).unwrap_or(0);
            let message = err
                .get("text")
                .or_else(|| err.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Unknown API error")
                .to_owned();
            let error_code = err
                .get("error_code")
                .and_then(Value::as_str)
                .map(String::from);
            return ApiError {
                code: u32::try_from(code).unwrap_or(0),
                error_code,
                message,
                http_status,
            };
        }

        ApiError {
            code: 0,
            error_code: None,
            message: format!("API request failed with HTTP {http_status}"),
            http_status,
        }
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

    /// Emit a clear rate-limit error to stderr.
    fn emit_rate_limit_error(retry_secs: u64) {
        eprintln!(
            "{} API rate limit exceeded. Retry in {} seconds.",
            "error:".red().bold(),
            retry_secs
        );
    }

    /// Emit a warning to stderr if rate-limit headers indicate low remaining quota.
    fn check_rate_limit(resp: &reqwest::Response) {
        let available = resp
            .headers()
            .get("X-Rate-Limit-Available")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let max = resp
            .headers()
            .get("X-Rate-Limit-Max")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        if let Some(avail) = available {
            if avail == 0 {
                let expiry = resp
                    .headers()
                    .get("X-Rate-Limit-Expiry")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown");
                eprintln!(
                    "{} API rate limit exhausted (0/{} remaining). Resets at {expiry}.",
                    "warning:".yellow().bold(),
                    max.map_or_else(|| "?".to_owned(), |m| m.to_string()),
                );
            } else if avail <= RATE_LIMIT_LOW_THRESHOLD {
                eprintln!(
                    "{} API rate limit low ({avail}/{} requests remaining).",
                    "warning:".yellow().bold(),
                    max.map_or_else(|| "?".to_owned(), |m| m.to_string()),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_body_returned_verbatim() {
        let body = "short error";
        assert_eq!(ApiClient::truncate_for_error_message(body), body);
    }

    #[test]
    fn long_body_truncated_with_marker() {
        let body = "a".repeat(ApiClient::ERROR_MESSAGE_MAX_BYTES + 1000);
        let out = ApiClient::truncate_for_error_message(&body);
        assert!(out.len() < body.len());
        assert!(out.contains("[truncated, 1000 more bytes]"), "got: {out}");
    }

    #[test]
    fn truncation_walks_back_to_char_boundary() {
        // Build a body whose cut point (8192) would split a multi-byte char.
        // "あ" is 3 bytes (E3 81 82); place one spanning position 8191-8193.
        let mut body = "a".repeat(ApiClient::ERROR_MESSAGE_MAX_BYTES - 1);
        body.push('あ');
        body.push_str("bbb");
        let out = ApiClient::truncate_for_error_message(&body);
        // Output must be valid UTF-8 and contain the truncation marker.
        assert!(out.is_char_boundary(out.len()));
        assert!(out.contains("[truncated,"), "got: {out}");
    }
}
