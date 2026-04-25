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

    /// Perform a GET request and return the parsed JSON body for both
    /// HTTP 200 and HTTP 404 responses, without unwrapping the
    /// `result`/`response` envelope.
    ///
    /// Bulk-resource endpoints (e.g. `/storage/{ids}/details/`) signal
    /// "all items errored" with HTTP 404 but still return a useful
    /// per-item body (`{nodes: [], errors: [...]}`); the caller needs to
    /// see that body to surface per-id outcomes. Other 4xx and unrecoverable
    /// 5xx responses are still converted to `CliError::Api` via the
    /// standard error envelope.
    pub async fn get_partial_envelope(&self, path: &str) -> Result<(u16, Value), CliError> {
        tracing::trace!(method = "GET", path, "api request (partial envelope)");
        self.send_with_retry_partial(|| {
            let mut req = self.inner.get(self.url(path));
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

    /// Send a request with retry, returning `(http_status, body)` for both
    /// HTTP 200 and HTTP 404 responses without envelope unwrapping. Used
    /// by `get_partial_envelope`; see that method for the contract.
    async fn send_with_retry_partial<F>(&self, build_request: F) -> Result<(u16, Value), CliError>
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

                    if status.as_u16() == 429 {
                        let retry_secs = Self::parse_rate_limit_expiry(&resp);
                        Self::emit_rate_limit_error(retry_secs);
                        return Err(CliError::RateLimit {
                            retry_after_secs: retry_secs,
                        });
                    }

                    Self::check_rate_limit(&resp);

                    // Non-200/404 responses may legitimately have a
                    // non-JSON body (proxy HTML error pages, empty
                    // 401s); fall back to an empty Value so
                    // `extract_error` can still return its default
                    // message rather than collapsing to a parse error.
                    let body: Value = if matches!(status.as_u16(), 200 | 404) {
                        resp.json().await.map_err(|e| {
                            CliError::Parse(format!(
                                "failed to parse {} response body: {e}",
                                status.as_u16()
                            ))
                        })?
                    } else {
                        resp.json().await.unwrap_or_default()
                    };
                    tracing::trace!(body = %body, "api response body (partial envelope)");

                    if matches!(status.as_u16(), 200 | 404) {
                        // The bulk-details contract uses the HTTP status
                        // and `result: "no"` together: a 404 with
                        // `result: "no"` is the all-errored success
                        // case, but a 200 with `result: "no"` (or
                        // either status with a top-level `error` and
                        // no bulk shape) is an authoritative envelope
                        // failure that must NOT be parsed as a bulk
                        // body. Detect via "no `nodes`/`errors` arrays
                        // and no non-null `node` object". A literal
                        // `node: null` does NOT count as bulk shape —
                        // treating it as such would let `result: "no"`
                        // + null-node masquerade as a successful
                        // empty result (round-2 review N3).
                        let payload = body.get("response").unwrap_or(&body);
                        let has_bulk_shape = payload.get("nodes").is_some_and(Value::is_array)
                            || payload.get("errors").is_some_and(Value::is_array)
                            || payload.get("node").is_some_and(|n| !n.is_null());
                        let result_no = matches!(
                            body.get("result"),
                            Some(Value::String(s)) if s == "no"
                        );
                        if result_no && !has_bulk_shape {
                            return Err(Self::extract_error(&body, status.as_u16()).into());
                        }
                        return Ok((status.as_u16(), body));
                    }
                    return Err(Self::extract_error(&body, status.as_u16()).into());
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

        // Unwrap: prefer "response" sub-object, fall back to top level sans "result".
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
