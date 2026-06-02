#![allow(clippy::missing_errors_doc)]

/// HTTP client wrapper for the Fast.io REST API.
///
/// Handles request construction, authentication header injection,
/// response envelope unwrapping, rate-limit detection, and automatic
/// retry with exponential backoff for transient network failures.
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use colored::Colorize;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::error::{ApiError, CliError};
use crate::output::OutputDetail;

/// User-Agent string sent on every outgoing request.
const CLIENT_USER_AGENT: &str = concat!("fastio-cli/", env!("CARGO_PKG_VERSION"));

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Connection timeout (seconds) for the dedicated streaming-download client.
///
/// The streaming client carries this connect timeout but **no** overall body
/// timeout, so an arbitrarily large signed PDF / audit bundle can stream for
/// as long as the connection stays alive. Mirrors the dedicated-client pattern
/// in `crate::api::download`.
const STREAM_CONNECT_TIMEOUT_SECS: u64 = 30;

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

/// Path substrings on which `?output=<detail>` must NEVER be injected.
///
/// Only genuine **non-envelope** endpoint families are excluded — paths that
/// return binary bytes, raw content, or OAuth payloads rather than the JSON
/// envelope. On these an injected `?output=` is either meaningless or actively
/// harmful (it can flip the `Content-Type` to `text/markdown` and crash
/// `resp.json()`).
///
/// Endpoints that *do* accept the documented `?output=terse|standard|full`
/// verbosity tokens are deliberately **not** denied here, even when `output`
/// also carries domain meaning for them — storage search
/// (`/storage/search/`) and every metadata endpoint (`/metadata/…`) both
/// accept the same three detail tokens per the docs (llms-full.txt "Compact
/// Responses" for storage search and metadata), so the generic `--detail`
/// passthrough is correct for them and they are injectable. The binary
/// variants of asset/preview endpoints are already covered by the `/read/`
/// and `/content/` families below; their JSON-list envelope siblings are
/// injectable.
///
/// Matched as a case-insensitive substring of the request path. The
/// pre-existing-`output=` guard in [`Self::output_injectable`] is a separate
/// layer that prevents sending two `output=` parameters.
const OUTPUT_INJECT_DENY_SUBSTRINGS: &[&str] = &[
    // Non-envelope / binary / raw / auth paths only.
    "/read/",
    "/content/",
    "/download/",
    "/oauth/",
];

/// HTTP client that wraps `reqwest` with Fast.io-specific conventions.
pub struct ApiClient {
    /// The underlying HTTP client.
    inner: reqwest::Client,
    /// Base URL for all API requests (e.g. `https://api.fast.io/current`).
    base_url: String,
    /// Bearer token for authentication, stored securely and zeroized on drop.
    token: Option<SecretString>,
    /// Server-side verbosity injected as `?output=<detail>` on allowlisted
    /// envelope GETs. Immutable for the client's lifetime (set at
    /// construction) because handlers hold `&self` async and interior
    /// mutability would be unidiomatic here.
    detail: Option<OutputDetail>,
    /// Lazily-built client used only by [`Self::download_file_stream`].
    ///
    /// Unlike [`Self::inner`], it carries a connect timeout but **no** overall
    /// request timeout, so large signed-PDF / audit-bundle downloads are not
    /// killed by [`DEFAULT_TIMEOUT_SECS`] mid-stream. Built on first use via
    /// [`OnceLock`] and reused thereafter (connection pooling) so a download
    /// burst does not rebuild the client per call.
    streaming_client: OnceLock<reqwest::Client>,
}

impl ApiClient {
    /// Create a new client targeting `base_url` with an optional bearer token.
    ///
    /// The token is wrapped in [`SecretString`] to prevent accidental logging
    /// and to zeroize memory on drop.
    pub fn new(base_url: &str, token: Option<String>) -> Result<Self, CliError> {
        Self::with_detail(base_url, token, None)
    }

    /// Create a new client with an explicit server-verbosity [`OutputDetail`].
    ///
    /// When `detail` is `Some`, allowlisted envelope GETs append
    /// `?output=<detail>` (see [`Self::build_get`]); when `None`, no `output`
    /// parameter is added and the server applies its `full` default. The
    /// detail is fixed for the client's lifetime.
    pub fn with_detail(
        base_url: &str,
        token: Option<String>,
        detail: Option<OutputDetail>,
    ) -> Result<Self, CliError> {
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
            detail,
            streaming_client: OnceLock::new(),
        })
    }

    /// Return the dedicated streaming-download client, building it on first use.
    ///
    /// Carries [`STREAM_CONNECT_TIMEOUT_SECS`] connect timeout but no overall
    /// request timeout (see [`Self::streaming_client`]). If the builder ever
    /// fails, falls back to [`Self::inner`] rather than erroring — the only
    /// documented failure modes for `reqwest::Client::builder().build()` are
    /// TLS-backend init issues that would already have failed `inner`, so the
    /// fallback is purely defensive.
    fn streaming_client(&self) -> &reqwest::Client {
        self.streaming_client.get_or_init(|| {
            let mut headers = HeaderMap::new();
            headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_USER_AGENT));
            reqwest::Client::builder()
                .default_headers(headers)
                .connect_timeout(Duration::from_secs(STREAM_CONNECT_TIMEOUT_SECS))
                .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
                .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
                .build()
                .unwrap_or_else(|_| self.inner.clone())
        })
    }

    /// Whether `?output=<detail>` may be injected on `path`.
    ///
    /// `false` for any path matching [`OUTPUT_INJECT_DENY_SUBSTRINGS`]
    /// (non-envelope binary/raw/oauth endpoints), `true` otherwise. Returns
    /// `false` regardless if the path already carries an explicit `output=`
    /// query parameter, to avoid sending two.
    fn output_injectable(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        if lower.contains("output=") {
            return false;
        }
        !OUTPUT_INJECT_DENY_SUBSTRINGS
            .iter()
            .any(|deny| lower.contains(deny))
    }

    /// Append `?output=<detail>` to `req` when the client has a configured
    /// detail level, `path` is injectable (see [`Self::output_injectable`]),
    /// and the caller has not already supplied an `output` parameter.
    ///
    /// This is the single shared implementation for every envelope GET helper
    /// (`get`/`get_with_params`/`get_with_auth`/`get_with_auth_and_params`/
    /// `get_no_auth_with_params`/`get_partial_envelope`). Routing them all
    /// through one seam keeps the `--detail` injection from drifting between
    /// helpers — previously only the plain `get()` path injected, so
    /// `--detail` silently no-opped on every parameterized / custom-auth GET.
    ///
    /// `has_output_param` lets callers that pass a `&HashMap` of query params
    /// signal that the map already carries an `output` key (case-insensitive);
    /// in that case we do not inject a second one. Because reqwest's
    /// `RequestBuilder::query` is opaque (it cannot be inspected after the
    /// fact), this decision has to happen at construction.
    fn inject_output_query(
        &self,
        req: reqwest::RequestBuilder,
        path: &str,
        has_output_param: bool,
    ) -> reqwest::RequestBuilder {
        if !has_output_param
            && let Some(detail) = self.detail
            && Self::output_injectable(path)
        {
            return req.query(&[("output", detail.as_str())]);
        }
        req
    }

    /// Return `true` if `params` contains an `output` key (case-insensitive).
    fn params_have_output(params: &HashMap<String, String>) -> bool {
        params.keys().any(|k| k.eq_ignore_ascii_case("output"))
    }

    /// Build an authenticated envelope GET, injecting `?output=<detail>` when
    /// a detail level is configured and the path is allowlisted.
    ///
    /// This is the single place that decision is made for the generic
    /// (no-extra-params) envelope GET path; the injection itself is delegated
    /// to [`Self::inject_output_query`].
    fn build_get(&self, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.inject_output_query(self.inner.get(self.url(path)), path, false);
        if let Some(auth) = self.auth_header() {
            req = req.header(AUTHORIZATION, auth);
        }
        req
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
    /// [`OutputDetail`] instead of a free-form string guarantees the
    /// server only ever sees the three documented tokens.
    /// Any `output` key present in `params` is dropped to prevent the
    /// server from receiving duplicate `output=` query parameters.
    ///
    /// This is the raw-text execution path that a future `--server-markdown`
    /// opt-in routes through. It is intentionally NOT wired to the global
    /// `--detail` flag (which selects JSON verbosity via [`Self::build_get`]);
    /// the two are distinct seams. No command exposes `--server-markdown`
    /// yet, so this remains infrastructure for now.
    #[allow(dead_code)]
    pub async fn get_markdown(
        &self,
        path: &str,
        params: Option<&HashMap<String, String>>,
        detail: Option<OutputDetail>,
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
    ///
    /// This is the canonical envelope-GET path. When a server-verbosity
    /// [`OutputDetail`] is configured on the client and `path` is injectable,
    /// it appends `?output=<detail>` (see [`Self::build_get`] /
    /// [`Self::output_injectable`]); only genuine non-envelope binary/raw/oauth
    /// paths (read/content/download/oauth) are excluded.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, "api request");
        self.send_with_retry(|| self.build_get(path)).await
    }

    /// Perform a GET request with query parameters.
    #[allow(dead_code)]
    pub async fn get_with_params<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, ?params, "api request");
        let has_output = Self::params_have_output(params);
        self.send_with_retry(|| {
            let req = self.inner.get(self.url(path)).query(params);
            let mut req = self.inject_output_query(req, path, has_output);
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
            let req = self.inject_output_query(self.inner.get(self.url(path)), path, false);
            req.header(AUTHORIZATION, auth_owned.clone())
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
        let has_output = Self::params_have_output(params);
        self.send_with_retry(|| {
            let req = self
                .inner
                .get(self.url(path))
                .header(AUTHORIZATION, auth_owned.clone())
                .query(params);
            self.inject_output_query(req, path, has_output)
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
        let has_output = Self::params_have_output(params);
        self.send_with_retry(|| {
            let req = self.inner.get(self.url(path)).query(params);
            self.inject_output_query(req, path, has_output)
        })
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

    /// Perform a JSON POST and return the raw JSON body **without** the
    /// `result`/`response` envelope unwrap.
    ///
    /// Prefer [`Self::post_json`] for endpoints that follow the standard
    /// `{"result": "yes", "response": …}` envelope. Use this for endpoints
    /// whose success body does not (e.g. AI chat cancel, which returns
    /// `{"success": true, …}` on 2xx). Non-2xx responses are surfaced as
    /// `CliError::Api` via [`Self::extract_error`], which recognizes both
    /// the nested standard envelope and a flat
    /// `{"error_message": …, "error_id": …}` shape. Callers are
    /// responsible for inspecting the returned 2xx body for any
    /// application-level error fields.
    pub async fn post_json_raw<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, body = %body, "api request (json, raw)");
        self.send_with_retry_raw(|| {
            let mut req = self.inner.post(self.url(path)).json(body);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform an authenticated POST with **no request body at all** and
    /// return the raw JSON body **without** the `result`/`response` envelope
    /// unwrap.
    ///
    /// Unlike [`Self::post_json_raw`], this sends neither a JSON body nor a
    /// `Content-Type` header — the wire request carries an empty body. It is
    /// for endpoints whose contract is literally "Body: Empty" (e.g. the AI
    /// chat cancel endpoint, ai.txt:625), where sending `{}` with
    /// `Content-Type: application/json` would diverge from the documented
    /// contract. Like `post_json_raw`, the 2xx body is returned verbatim and
    /// non-2xx responses are surfaced as `CliError::Api` via
    /// [`Self::extract_error`] (which recognizes both the nested standard
    /// envelope and a flat `{"error_message": …, "error_id": …}` shape).
    /// Callers are responsible for inspecting the returned 2xx body for any
    /// application-level error fields.
    pub async fn post_empty_raw<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, "api request (empty body, raw)");
        self.send_with_retry_raw(|| {
            let mut req = self.inner.post(self.url(path));
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a JSON PATCH and unwrap the API envelope.
    ///
    /// Mirrors [`Self::post_json`] with the method swapped to `PATCH`; routes
    /// through the same retry / rate-limit / envelope-unwrap path. Used by
    /// endpoints whose PATCH bodies are genuine JSON (verify per-endpoint —
    /// many orchestration PATCH bodies are form-encoded; use
    /// [`Self::patch_form`] for those).
    #[allow(dead_code)]
    pub async fn patch_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "PATCH", path, body = %body, "api request (json)");
        self.send_with_retry(|| {
            let mut req = self.inner.patch(self.url(path)).json(body);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a JSON PUT and unwrap the API envelope.
    ///
    /// Mirrors [`Self::post_json`] with the method swapped to `PUT`; routes
    /// through the same retry / rate-limit / envelope-unwrap path.
    #[allow(dead_code)]
    pub async fn put_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "PUT", path, body = %body, "api request (json)");
        self.send_with_retry(|| {
            let mut req = self.inner.put(self.url(path)).json(body);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a form-encoded PATCH and unwrap the API envelope.
    ///
    /// Mirrors [`Self::post`] with the method swapped to `PATCH`. Several
    /// orchestration PATCH endpoints accept `application/x-www-form-urlencoded`
    /// bodies whose values are JSON strings (e.g. `output={…}`); this is the
    /// helper for those — not [`Self::patch_json`].
    #[allow(dead_code)]
    pub async fn patch_form<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "PATCH", path, ?form, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.patch(self.url(path)).form(form);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a form-encoded PUT and unwrap the API envelope.
    ///
    /// Mirrors [`Self::post`] with the method swapped to `PUT`. Used by
    /// orchestration PUT endpoints that take form-encoded bodies with
    /// JSON-string values.
    #[allow(dead_code)]
    pub async fn put_form<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "PUT", path, ?form, "api request");
        self.send_with_retry(|| {
            let mut req = self.inner.put(self.url(path)).form(form);
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
            let mut req = self.inject_output_query(self.inner.get(self.url(path)), path, false);
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

    /// Decide whether a streaming-download response is an error rather than a
    /// streamable body, based purely on the HTTP status.
    ///
    /// Returns `true` only when the status is **not** a success (non-2xx). A
    /// 2xx response is always streamed, regardless of `Content-Type` — the
    /// signing audit-certificate endpoint
    /// (`/sign_envelopes/{id}/audit/download/`) returns a 2xx
    /// `application/json` body that is the *success* payload, not an error
    /// envelope, so content-type sniffing here would wrongly reject it. Error
    /// detection therefore keys on status alone. Pure function so the branch
    /// is unit-testable without a live server.
    fn stream_response_is_error(status_is_success: bool) -> bool {
        !status_is_success
    }

    /// Stream a binary GET response directly to disk, returning the number of
    /// bytes written.
    ///
    /// This is the canonical helper for large authenticated binary/streamed
    /// downloads (signed PDFs, audit bundles). Unlike `read_user_asset`, which
    /// buffers the whole body via `resp.bytes().await`, this streams the body
    /// in chunks via [`reqwest::Response::bytes_stream`] and writes them with
    /// [`tokio::io::AsyncWriteExt`], so memory stays bounded regardless of
    /// file size.
    ///
    /// **Timeout:** uses the dedicated [`Self::streaming_client`] (connect
    /// timeout only, no overall body timeout) so a multi-MB download is never
    /// killed mid-stream by the pooled client's [`DEFAULT_TIMEOUT_SECS`].
    ///
    /// **Error detection:** a non-2xx status is treated as an error — the
    /// (small) body is buffered and surfaced as a structured
    /// [`CliError::Api`] via [`Self::extract_error`], and no output file is
    /// created. A 2xx status is *always* streamed regardless of
    /// `Content-Type`, because the audit-certificate endpoint legitimately
    /// returns a 2xx `application/json` success body (see
    /// [`Self::stream_response_is_error`]).
    ///
    /// **Atomicity:** the body streams to a sibling `<output_path>.partial`
    /// temp file in the same directory, is flushed and `sync_all`'d, then
    /// atomically [`tokio::fs::rename`]d onto `output_path` only on full
    /// success. On *any* error during streaming/write/flush/rename the temp
    /// file is removed (best effort) and the error returned, so a mid-stream
    /// failure never leaves a truncated file at `output_path` and never
    /// clobbers a pre-existing file there.
    ///
    /// Uses the bearer token directly (no envelope unwrap, no retry layer):
    /// the body is consumed exactly once as a stream, which the retry path
    /// cannot replay.
    pub async fn download_file_stream(
        &self,
        path: &str,
        output_path: &std::path::Path,
    ) -> Result<u64, CliError> {
        tracing::trace!(method = "GET", path, "api request (stream download)");
        let mut req = self.streaming_client().get(self.url(path));
        if let Some(auth) = self.auth_header() {
            req = req.header(AUTHORIZATION, auth);
        }
        let resp = req.send().await.map_err(CliError::Http)?;
        let status = resp.status();

        // Decide error-vs-stream by HTTP status only (a 2xx JSON body is a
        // valid success payload for the audit-certificate endpoint).
        if Self::stream_response_is_error(status.is_success()) {
            let http_status = status.as_u16();
            // The error body is small; buffering it here is fine (and the
            // stream is not yet consumed). Fall back to a generic message if
            // the body is missing or not JSON.
            let body: Value = resp.json().await.unwrap_or_default();
            tracing::trace!(body = %body, "stream download error body");
            return Err(Self::extract_error(&body, http_status).into());
        }

        // Stream to a UNIQUE sibling temp file, then atomically rename on
        // success so a mid-stream failure never leaves a partial at
        // `output_path` and two concurrent downloads of the same target never
        // collide on (or clobber) each other's temp.
        //
        // FIX E: create the temp as a DISTINCT first step. If `create_new`
        // fails (the unique path is somehow already taken, a permission error,
        // etc.) we return immediately WITHOUT any cleanup — we must never
        // `remove_file` a path this invocation did not create. Only once the
        // temp is confirmed ours do we enter the cleanup-bearing finalize path.
        let temp_path = Self::partial_path(output_path);
        let file = Self::create_temp(&temp_path).await?;
        let written = Self::stream_to_temp(resp, file).await;
        Self::finalize_download(written, &temp_path, output_path).await
    }

    /// Resolve a streaming download to its final state.
    ///
    /// On a streaming success, atomically replaces `output_path` with
    /// `temp_path` (see [`Self::atomic_replace`]) and returns the byte count.
    /// On *any* streaming or rename failure, removes `temp_path` (best effort)
    /// and returns the error, guaranteeing no partial/truncated file is left at
    /// `output_path`. By contract this is only ever called AFTER
    /// [`Self::create_temp`] has succeeded (FIX E), so `temp_path` is always a
    /// file THIS invocation created — the cleanup never touches a stale or
    /// unrelated file. Split out so the rename/cleanup contract is unit-testable
    /// without a live server.
    async fn finalize_download(
        streamed: Result<u64, CliError>,
        temp_path: &std::path::Path,
        output_path: &std::path::Path,
    ) -> Result<u64, CliError> {
        match streamed {
            Ok(written) => match Self::atomic_replace(temp_path, output_path).await {
                Ok(()) => Ok(written),
                Err(e) => {
                    // Rename failed (e.g. cross-device, permissions): clean up
                    // the temp and surface the error.
                    let _ = tokio::fs::remove_file(temp_path).await;
                    Err(CliError::Io(e))
                }
            },
            Err(e) => {
                // Streaming/write/flush failed: remove the partial (best
                // effort) so no truncated file is left behind.
                let _ = tokio::fs::remove_file(temp_path).await;
                Err(e)
            }
        }
    }

    /// Portably move `temp` onto `dest`, replacing any existing `dest` WITHOUT
    /// ever risking the loss of the user's pre-existing file (FIX F).
    ///
    /// On Unix [`tokio::fs::rename`] atomically replaces an existing
    /// destination, so the first rename is all that's needed. On Windows
    /// `rename` refuses to overwrite and fails with `AlreadyExists`; rather than
    /// the unsafe delete-then-retry (which loses `dest` if the retry fails), we
    /// do a **backup swap**:
    ///
    /// 1. Try `rename(temp, dest)`. Success → done (the Unix replace case, and
    ///    the case where `dest` does not exist on any platform).
    /// 2. On `AlreadyExists`, delegate to [`Self::backup_swap_replace`], which
    ///    backs `dest` up, replaces it, and rolls back on failure so the
    ///    original is never lost.
    ///
    /// On any failure the caller still cleans up `temp` (it was never renamed
    /// away on the error paths here).
    async fn atomic_replace(temp: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
        match tokio::fs::rename(temp, dest).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Windows: rename won't overwrite. Preserve `dest` via a backup
                // swap so a failed replacement can be rolled back.
                Self::backup_swap_replace(temp, dest).await
            }
            Err(e) => Err(e),
        }
    }

    /// Replace an existing `dest` with `temp` via a backup swap, rolling back on
    /// failure so the user's original file is never lost (FIX F).
    ///
    /// Used by [`Self::atomic_replace`] only when a plain rename refuses to
    /// overwrite (the Windows `AlreadyExists` case). Steps:
    ///
    /// 1. Move `dest` to a unique sibling backup (see [`Self::backup_path`]).
    /// 2. Move `temp` onto `dest`.
    ///    - SUCCESS → best-effort remove the backup and return `Ok`.
    ///    - FAILURE → restore by moving the backup back to `dest` so the
    ///      original survives, then return the error. `dest` is never left
    ///      missing on this path. If the restore itself fails (extremely rare),
    ///      the original replace error is surfaced and the backup remains on
    ///      disk for manual recovery.
    ///
    /// Pulled out as a distinct helper so the restore-on-failure path is
    /// unit-testable on any platform (a non-existent `temp` forces the inner
    /// `temp → dest` rename to fail, exercising the rollback) without depending
    /// on platform-specific `rename`-overwrite behavior.
    async fn backup_swap_replace(
        temp: &std::path::Path,
        dest: &std::path::Path,
    ) -> std::io::Result<()> {
        let backup = Self::backup_path(dest);
        tokio::fs::rename(dest, &backup).await?;
        match tokio::fs::rename(temp, dest).await {
            Ok(()) => {
                // Replacement landed; the backup is now redundant.
                let _ = tokio::fs::remove_file(&backup).await;
                Ok(())
            }
            Err(replace_err) => {
                // Replacement failed — restore the user's original file so
                // `dest` is never left missing. The restore is the inverse of
                // the move we just made; if it somehow fails too, surface the
                // original replace error (the backup remains on disk for manual
                // recovery).
                let _ = tokio::fs::rename(&backup, dest).await;
                Err(replace_err)
            }
        }
    }

    /// Compute a UNIQUE sibling backup path for the existing destination during
    /// an [`Self::atomic_replace`] backup swap (FIX F).
    ///
    /// Appends a `.<pid>.<counter>.bak` suffix to the destination name so the
    /// backup lives in the **same directory** as `dest` (rename is only atomic
    /// within one filesystem, and a sibling is guaranteed to be on the same
    /// one). The PID disambiguates concurrent processes and the process-global
    /// [`AtomicU64`] counter disambiguates concurrent in-process replaces, so
    /// two callers never collide on the same backup name.
    fn backup_path(dest: &std::path::Path) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut name = dest.as_os_str().to_owned();
        name.push(format!(".{}.{n}.bak", std::process::id()));
        std::path::PathBuf::from(name)
    }

    /// Compute a UNIQUE sibling temp path used while streaming a download.
    ///
    /// Appends a `.<pid>.<counter>.partial` suffix to the final filename so the
    /// temp lives in the **same directory** as `output_path` — a prerequisite
    /// for the atomic [`tokio::fs::rename`] (rename is only atomic within a
    /// single filesystem, and a sibling path is guaranteed to be on the same
    /// one) — while remaining unique per call. The PID disambiguates concurrent
    /// processes and the process-global [`AtomicU64`] counter disambiguates
    /// concurrent in-process downloads of the same target, so two callers never
    /// collide on or clobber each other's temp.
    fn partial_path(output_path: &std::path::Path) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut name = output_path.as_os_str().to_owned();
        name.push(format!(".{}.{n}.partial", std::process::id()));
        std::path::PathBuf::from(name)
    }

    /// Create the streaming temp file with `create_new(true)`.
    ///
    /// Kept as a DISTINCT step (FIX E) so the caller can establish temp
    /// ownership BEFORE entering any cleanup-bearing path. `create_new(true)`
    /// fails (rather than truncating) if a file already exists at the unique
    /// path; on that — or any other open error — the caller returns the error
    /// immediately and must NOT remove the path, because this invocation never
    /// created it. Only after this returns `Ok` does the temp belong to us and
    /// become eligible for cleanup.
    async fn create_temp(temp_path: &std::path::Path) -> Result<tokio::fs::File, CliError> {
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temp_path)
            .await
            .map_err(CliError::Io)
    }

    /// Stream a (already validated 2xx) response body into the already-created
    /// temp `file`, returning the byte count. The caller owns rename/cleanup of
    /// the temp path.
    ///
    /// The temp is created up-front by [`Self::create_temp`] and passed in here,
    /// so this function never touches the filesystem namespace — it only writes
    /// to a handle the caller already confirmed it owns (FIX E).
    async fn stream_to_temp(
        resp: reqwest::Response,
        mut file: tokio::fs::File,
    ) -> Result<u64, CliError> {
        let mut written: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(CliError::Http)?;
            file.write_all(&chunk).await.map_err(CliError::Io)?;
            written = written.saturating_add(chunk.len() as u64);
        }
        file.flush().await.map_err(CliError::Io)?;
        // Cheap durability barrier before the atomic rename so the renamed
        // file's contents are on disk, not just in the page cache.
        file.sync_all().await.map_err(CliError::Io)?;
        Ok(written)
    }

    /// Send a request with automatic retry and exponential backoff,
    /// returning the unprocessed [`reqwest::Response`] for body-shape-specific
    /// handlers to parse.
    ///
    /// Retries on connection errors, timeouts, and HTTP 502/503/504. A 429
    /// response is converted to [`CliError::RateLimit`] without retrying.
    /// Rate-limit headers are checked on every successful return.
    async fn send_request_with_retry<F>(
        &self,
        build_request: F,
    ) -> Result<reqwest::Response, CliError>
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
                    tracing::trace!(status = status.as_u16(), "api response");

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
                    return Ok(resp);
                }
                Err(e) if Self::is_retryable_error(&e) && attempt < MAX_RETRIES => {
                    tracing::warn!(error = %e, "transient network error, will retry");
                    last_error = Some(CliError::Http(e));
                }
                Err(e) => return Err(CliError::Http(e)),
            }
        }

        // `last_error` is always `Some` here because the loop body sets it
        // on every retryable failure, but we handle `None` defensively.
        Err(last_error
            .unwrap_or_else(|| CliError::Parse("request failed: all retries exhausted".to_owned())))
    }

    /// Send a request with retry; deserialize the unwrapped envelope payload.
    async fn send_with_retry<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let resp = self.send_request_with_retry(build_request).await?;
        self.handle_response(resp).await
    }

    /// Send a request with retry; deserialize the full JSON body without
    /// envelope unwrapping.
    async fn send_with_retry_raw<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let resp = self.send_request_with_retry(build_request).await?;
        self.handle_response_raw(resp).await
    }

    /// Send a request with retry; return `(http_status, body)` for both
    /// HTTP 200 and HTTP 404 responses without envelope unwrapping. Used
    /// by `get_partial_envelope`; see that method for the contract.
    async fn send_with_retry_partial<F>(&self, build_request: F) -> Result<(u16, Value), CliError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let resp = self.send_request_with_retry(build_request).await?;
        Self::handle_response_partial(resp).await
    }

    /// Send a request with retry; return the raw response body as text
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
        let resp = self.send_request_with_retry(build_request).await?;
        Self::handle_response_text(resp).await
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

    /// Process an API response: parse JSON, check the envelope, return the
    /// unwrapped payload.
    async fn handle_response<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, CliError> {
        let status = resp.status();
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

    /// Process an API response without envelope unwrapping: deserialize the
    /// full JSON body directly into `T`.
    async fn handle_response_raw<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, CliError> {
        let status = resp.status();

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

    /// Process an API response for the bulk-details partial-envelope contract.
    ///
    /// Returns `(http_status, body)` for both HTTP 200 and HTTP 404 so the
    /// caller can distinguish full success from the all-errored case (which
    /// the server signals as 404 + `result: "no"`). Other statuses produce
    /// a structured error.
    async fn handle_response_partial(resp: reqwest::Response) -> Result<(u16, Value), CliError> {
        let status = resp.status();

        // Non-200/404 responses may legitimately have a non-JSON body
        // (proxy HTML error pages, empty 401s); fall back to an empty
        // Value so `extract_error` can still return its default message
        // rather than collapsing to a parse error.
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
            // The bulk-details contract uses the HTTP status and
            // `result: "no"` together: a 404 with `result: "no"` is the
            // all-errored success case, but a 200 with `result: "no"`
            // (or either status with a top-level `error` and no bulk
            // shape) is an authoritative envelope failure that must NOT
            // be parsed as a bulk body. Detect via "no `nodes`/`errors`
            // arrays and no non-null `node` object". A literal
            // `node: null` does NOT count as bulk shape — treating it
            // as such would let `result: "no"` + null-node masquerade
            // as a successful empty result (round-2 review N3).
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
        Err(Self::extract_error(&body, status.as_u16()).into())
    }

    /// Process an API response that returns text rather than JSON (e.g. the
    /// markdown fetch path). Non-success statuses are surfaced as
    /// `CliError::Api` with the body included as the error message.
    async fn handle_response_text(resp: reqwest::Response) -> Result<String, CliError> {
        let status = resp.status();
        let http_status = status.as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| CliError::Parse(format!("failed to read response body: {e}")))?;

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
                details: None,
            }));
        }

        Ok(body)
    }

    /// Extract a structured API error from the response body.
    ///
    /// Recognizes two envelope shapes:
    ///
    /// - **Nested** (the standard Fast.io envelope):
    ///   `{"result": "no", "error": {"code": …, "text"|"message": …,
    ///   "error_code": …}}`. Mined first.
    /// - **Flat** (used by the AI chat cancel endpoint and any future
    ///   non-conforming endpoints): `{"result": false, "error_message": …,
    ///   "error_id": …}`. Tried as a fallback when no nested `error` object
    ///   is present, so live HTTP 4xx/5xx responses surface the server's
    ///   actual message instead of the generic
    ///   `"API request failed with HTTP {status}"` placeholder.
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
                details: Self::extract_error_details(err),
            };
        }

        if let Some(message) = body
            .get("error_message")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            // `error_id` is documented numeric on at least the cancel
            // endpoint; accept either a number or a numeric string for
            // forward-compatibility. Server IDs may exceed u32; emit a
            // trace warning when narrowing collapses a non-zero ID to 0
            // so support can correlate to the raw body if needed.
            let raw_id = body.get("error_id").and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            });
            let code = raw_id
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or_else(|| {
                    if let Some(n) = raw_id {
                        tracing::warn!(
                            error_id = n,
                            http_status,
                            "API error_id exceeds u32; truncating ApiError.code to 0"
                        );
                    }
                    0
                });
            return ApiError {
                code,
                error_code: None,
                message: message.to_owned(),
                http_status,
                details: None,
            };
        }

        ApiError {
            code: 0,
            error_code: None,
            message: format!("API request failed with HTTP {http_status}"),
            http_status,
            details: None,
        }
    }

    /// Collect structured diagnostics from a Fast.io `error` envelope object
    /// into a single JSON object for [`ApiError::details`].
    ///
    /// Preserves the documented enrichment fields when present:
    /// - `params` — per-field validation failures (400; `name`/`kind`/`code`/
    ///   `message`), the modern replacement for the retired per-field codes.
    /// - `validation_report` — structured report (422).
    /// - `reason` — structured fire/conflict reason (409).
    /// - `documentation_url` and `resource` — links to the relevant docs and
    ///   the offending resource identifier.
    ///
    /// Returns `None` if the envelope carried none of these, so callers can
    /// cheaply branch on "is there extra detail to render". Boxed to match
    /// [`ApiError::details`], which boxes to keep `CliError` small.
    fn extract_error_details(err: &Value) -> Option<Box<Value>> {
        const DETAIL_KEYS: &[&str] = &[
            "params",
            "validation_report",
            "reason",
            "documentation_url",
            "resource",
        ];
        let mut collected = serde_json::Map::new();
        for key in DETAIL_KEYS {
            if let Some(v) = err.get(*key)
                && !v.is_null()
            {
                collected.insert((*key).to_owned(), v.clone());
            }
        }
        if collected.is_empty() {
            None
        } else {
            Some(Box::new(Value::Object(collected)))
        }
    }

    /// Read a rate-limit header by its modern lowercase name, falling back to
    /// the legacy `X-Rate-Limit-*` name for older API deployments.
    ///
    /// The Fast.io API migrated to `x-ve-limit-avail`/`x-ve-limit-max`/
    /// `x-ve-limit-expires`; the previous `X-Rate-Limit-Available`/`-Max`/
    /// `-Expiry` names are still accepted as a fallback so the client works
    /// against both. HTTP header lookups are case-insensitive, so the casing
    /// of these literals does not matter.
    fn rate_limit_header<'a>(
        resp: &'a reqwest::Response,
        modern: &str,
        legacy: &str,
    ) -> Option<&'a str> {
        resp.headers()
            .get(modern)
            .or_else(|| resp.headers().get(legacy))
            .and_then(|v| v.to_str().ok())
    }

    /// Parse the rate-limit expiry header to estimate seconds until reset.
    fn parse_rate_limit_expiry(resp: &reqwest::Response) -> u64 {
        Self::rate_limit_header(resp, "x-ve-limit-expires", "X-Rate-Limit-Expiry")
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
        let available = Self::rate_limit_header(resp, "x-ve-limit-avail", "X-Rate-Limit-Available")
            .and_then(|v| v.parse::<u64>().ok());

        let max = Self::rate_limit_header(resp, "x-ve-limit-max", "X-Rate-Limit-Max")
            .and_then(|v| v.parse::<u64>().ok());

        if let Some(avail) = available {
            if avail == 0 {
                let expiry =
                    Self::rate_limit_header(resp, "x-ve-limit-expires", "X-Rate-Limit-Expiry")
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
    use reqwest::header::CONTENT_TYPE;

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

    #[test]
    fn extract_error_uses_nested_envelope_when_present() {
        let body = serde_json::json!({
            "result": "no",
            "error": {"code": 1605, "text": "bad hash", "error_code": "APP_BAD_HASH"},
        });
        let err = ApiClient::extract_error(&body, 403);
        assert_eq!(err.code, 1605);
        assert_eq!(err.message, "bad hash");
        assert_eq!(err.error_code.as_deref(), Some("APP_BAD_HASH"));
        assert_eq!(err.http_status, 403);
    }

    #[test]
    fn extract_error_falls_back_to_flat_envelope() {
        // Cancel-endpoint error shape: flat `error_message` / `error_id`,
        // no nested `error` object. Must surface the server's actual
        // message rather than the generic placeholder.
        let body = serde_json::json!({
            "result": false,
            "error_message": "Chat not found",
            "error_id": 12_345,
        });
        let err = ApiClient::extract_error(&body, 406);
        assert_eq!(err.message, "Chat not found");
        assert_eq!(err.code, 12_345);
        assert_eq!(err.http_status, 406);
        assert!(err.error_code.is_none());
    }

    #[test]
    fn extract_error_flat_envelope_accepts_string_id() {
        let body = serde_json::json!({
            "error_message": "permission denied",
            "error_id": "67890",
        });
        let err = ApiClient::extract_error(&body, 406);
        assert_eq!(err.code, 67_890);
        assert_eq!(err.message, "permission denied");
    }

    #[test]
    fn extract_error_flat_envelope_truncates_oversize_id_to_zero() {
        // 19-digit Fast.io entity IDs overflow u32; ApiError.code is u32
        // throughout the codebase. Verify the narrowing collapses to 0
        // without panicking and the message still surfaces.
        let body = serde_json::json!({
            "error_message": "rejected",
            "error_id": "4687730903718774523",
        });
        let err = ApiClient::extract_error(&body, 406);
        assert_eq!(err.code, 0);
        assert_eq!(err.message, "rejected");
    }

    #[test]
    fn extract_error_falls_through_to_generic_message_when_no_known_shape() {
        let body = serde_json::json!({"some_other_field": "value"});
        let err = ApiClient::extract_error(&body, 502);
        assert_eq!(err.code, 0);
        assert_eq!(err.message, "API request failed with HTTP 502");
        assert_eq!(err.http_status, 502);
    }

    #[test]
    fn extract_error_empty_flat_message_falls_through_to_generic() {
        // An empty `error_message` would render as a blank line to the
        // user; treat it as no usable message and surface the generic
        // placeholder instead.
        let body = serde_json::json!({"error_message": "", "error_id": 99});
        let err = ApiClient::extract_error(&body, 406);
        assert_eq!(err.message, "API request failed with HTTP 406");
        assert_eq!(err.code, 0);
    }

    // ----- ApiError detail enrichment -----

    #[test]
    fn extract_error_preserves_params_validation_report_and_reason() {
        let body = serde_json::json!({
            "result": "no",
            "error": {
                "code": 1660,
                "text": "conflict",
                "params": [{"name": "name", "kind": "invalid", "code": 1, "message": "taken"}],
                "validation_report": {"ok": false, "fields": ["name"]},
                "reason": {"type": "fire_conflict", "detail": "already firing"},
                "documentation_url": "https://docs.fast.io/x",
                "resource": "workflow:123",
            },
        });
        let err = ApiClient::extract_error(&body, 409);
        let details = err.details.expect("details should be populated");
        assert!(details.get("params").is_some(), "params preserved");
        assert!(
            details.get("validation_report").is_some(),
            "validation_report preserved"
        );
        assert!(details.get("reason").is_some(), "reason preserved");
        assert_eq!(
            details.get("documentation_url").and_then(Value::as_str),
            Some("https://docs.fast.io/x"),
        );
        assert_eq!(
            details.get("resource").and_then(Value::as_str),
            Some("workflow:123"),
        );
    }

    #[test]
    fn extract_error_details_none_when_no_enrichment_fields() {
        let body = serde_json::json!({
            "result": "no",
            "error": {"code": 1605, "text": "bad hash"},
        });
        let err = ApiClient::extract_error(&body, 403);
        assert!(err.details.is_none(), "no enrichment fields → None");
    }

    #[test]
    fn extract_error_details_skips_null_fields() {
        let body = serde_json::json!({
            "result": "no",
            "error": {"code": 1660, "text": "x", "params": null, "reason": null,
                       "documentation_url": "https://d"},
        });
        let err = ApiClient::extract_error(&body, 409);
        let details = err.details.expect("documentation_url present");
        assert!(details.get("params").is_none(), "null params dropped");
        assert!(details.get("reason").is_none(), "null reason dropped");
        assert_eq!(details.as_object().map(serde_json::Map::len), Some(1));
    }

    // ----- `?output=` injection allowlist -----

    #[test]
    fn output_injectable_allows_plain_envelope_paths() {
        assert!(ApiClient::output_injectable("/org/123/details/"));
        assert!(ApiClient::output_injectable("/workspace/1/list/"));
        assert!(ApiClient::output_injectable("/shares/all/"));
    }

    #[test]
    fn output_injectable_denies_download_content_oauth() {
        assert!(!ApiClient::output_injectable("/storage/abc/read/"));
        assert!(!ApiClient::output_injectable("/storage/abc/download/"));
        assert!(!ApiClient::output_injectable("/oauth/token/"));
        assert!(!ApiClient::output_injectable("/user/u/assets/a/read/"));
        assert!(!ApiClient::output_injectable("/storage/n/content/"));
        // Signing audit/source/signed binary streams go through /download/.
        assert!(!ApiClient::output_injectable(
            "/workspace/1/sign_envelopes/e/audit/download/"
        ));
    }

    #[test]
    fn output_injectable_allows_storage_search_and_metadata() {
        // storage search and every metadata endpoint accept the documented
        // ?output=terse|standard|full tokens, so --detail SHOULD inject.
        assert!(ApiClient::output_injectable("/workspace/1/storage/search/"));
        assert!(ApiClient::output_injectable(
            "/workspace/1/storage/n/metadata/details/"
        ));
        assert!(ApiClient::output_injectable(
            "/workspace/1/metadata/templates/"
        ));
        // assets/preview JSON-envelope siblings are injectable too; only the
        // binary read/content variants are denied.
        assert!(ApiClient::output_injectable("/user/u/assets/"));
        assert!(ApiClient::output_injectable(
            "/workspace/1/storage/n/preview/thumbnail/preauthorize/"
        ));
    }

    #[test]
    fn output_injectable_denies_path_with_existing_output_param() {
        assert!(!ApiClient::output_injectable("/org/1/details/?output=full"));
    }

    #[test]
    fn build_get_injects_output_when_detail_set_and_allowlisted() {
        let client = ApiClient::with_detail(
            "https://api.example/current",
            Some("tok".to_owned()),
            Some(OutputDetail::Terse),
        )
        .expect("client builds");
        let req = client
            .build_get("/org/123/details/")
            .build()
            .expect("request builds");
        assert_eq!(req.method(), reqwest::Method::GET);
        assert_eq!(req.url().query(), Some("output=terse"));
    }

    #[test]
    fn build_get_omits_output_on_denylisted_path() {
        let client = ApiClient::with_detail(
            "https://api.example/current",
            Some("tok".to_owned()),
            Some(OutputDetail::Full),
        )
        .expect("client builds");
        // download path must never get `?output=`.
        let dl = client
            .build_get("/storage/n/read/")
            .build()
            .expect("request builds");
        assert!(dl.url().query().is_none(), "download path must not inject");
        // oauth path must never get `?output=`.
        let oauth = client
            .build_get("/oauth/authorize/")
            .build()
            .expect("request builds");
        assert!(oauth.url().query().is_none(), "oauth must not inject");
    }

    #[test]
    fn build_get_omits_output_when_no_detail_configured() {
        let client =
            ApiClient::new("https://api.example/current", Some("tok".to_owned())).expect("builds");
        let req = client
            .build_get("/org/123/details/")
            .build()
            .expect("request builds");
        assert!(req.url().query().is_none(), "no --detail → no output param");
    }

    // ----- shared `?output=` injection across parameterized GET helpers (FIX 4) -----

    #[test]
    fn params_have_output_is_case_insensitive() {
        let mut p = HashMap::new();
        p.insert("OUTPUT".to_owned(), "full".to_owned());
        assert!(ApiClient::params_have_output(&p));
        let mut p2 = HashMap::new();
        p2.insert("limit".to_owned(), "10".to_owned());
        assert!(!ApiClient::params_have_output(&p2));
    }

    #[test]
    fn inject_output_query_adds_detail_on_parameterized_get_when_injectable() {
        // Mirrors the build path inside `get_with_params`: query(params) then
        // inject. `--detail` must now appear (previously it no-opped here).
        let client = ApiClient::with_detail(
            "https://api.example/current",
            Some("tok".to_owned()),
            Some(OutputDetail::Standard),
        )
        .expect("client builds");
        let mut params = HashMap::new();
        params.insert("limit".to_owned(), "25".to_owned());
        let has_output = ApiClient::params_have_output(&params);
        let req = client
            .inner
            .get(client.url("/storage/search/"))
            .query(&params);
        let req = client
            .inject_output_query(req, "/storage/search/", has_output)
            .build()
            .expect("request builds");
        let query = req.url().query().unwrap_or_default();
        assert!(
            query.contains("limit=25"),
            "caller param preserved: {query}"
        );
        assert!(
            query.contains("output=standard"),
            "detail injected: {query}"
        );
    }

    #[test]
    fn inject_output_query_skips_when_params_already_carry_output() {
        let client = ApiClient::with_detail(
            "https://api.example/current",
            Some("tok".to_owned()),
            Some(OutputDetail::Terse),
        )
        .expect("client builds");
        let mut params = HashMap::new();
        params.insert("output".to_owned(), "full".to_owned());
        let has_output = ApiClient::params_have_output(&params);
        let req = client
            .inner
            .get(client.url("/storage/search/"))
            .query(&params);
        let req = client
            .inject_output_query(req, "/storage/search/", has_output)
            .build()
            .expect("request builds");
        let query = req.url().query().unwrap_or_default();
        // The caller's explicit output=full survives; no second output= added.
        assert_eq!(query, "output=full", "no duplicate output param: {query}");
    }

    #[test]
    fn inject_output_query_skips_on_denylisted_path() {
        let client = ApiClient::with_detail(
            "https://api.example/current",
            Some("tok".to_owned()),
            Some(OutputDetail::Full),
        )
        .expect("client builds");
        let mut params = HashMap::new();
        params.insert("token".to_owned(), "abc".to_owned());
        let has_output = ApiClient::params_have_output(&params);
        let req = client
            .inner
            .get(client.url("/storage/n/read/"))
            .query(&params);
        let req = client
            .inject_output_query(req, "/storage/n/read/", has_output)
            .build()
            .expect("request builds");
        let query = req.url().query().unwrap_or_default();
        assert!(
            !query.contains("output="),
            "denylisted path must not inject: {query}"
        );
    }

    // ----- atomic streaming-download finalize (FIX 1) -----

    #[tokio::test]
    async fn finalize_download_renames_temp_to_output_on_success() {
        let dir = std::env::temp_dir().join(format!("fastio-dl-ok-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("file.bin");
        let temp = ApiClient::partial_path(&output);
        tokio::fs::write(&temp, b"streamed-bytes")
            .await
            .expect("write temp");

        let res = ApiClient::finalize_download(Ok(14), &temp, &output).await;
        assert_eq!(res.expect("ok"), 14);
        // Output now holds the bytes; the .partial temp is gone.
        let contents = tokio::fs::read(&output).await.expect("read output");
        assert_eq!(contents, b"streamed-bytes");
        assert!(
            tokio::fs::metadata(&temp).await.is_err(),
            "temp should be renamed away"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn finalize_download_leaves_no_partial_on_mid_stream_error() {
        let dir = std::env::temp_dir().join(format!("fastio-dl-err-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("file.bin");
        let temp = ApiClient::partial_path(&output);
        // Simulate a partially-written temp from a stream that then failed.
        tokio::fs::write(&temp, b"partial")
            .await
            .expect("write temp");

        let streamed = Err(CliError::Parse("simulated mid-stream failure".to_owned()));
        let res = ApiClient::finalize_download(streamed, &temp, &output).await;
        assert!(res.is_err(), "error must propagate");
        // No truncated file at output_path, and the temp is cleaned up.
        assert!(
            tokio::fs::metadata(&output).await.is_err(),
            "output must NOT exist after a mid-stream error"
        );
        assert!(
            tokio::fs::metadata(&temp).await.is_err(),
            "partial temp must be removed"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn finalize_download_error_does_not_clobber_existing_output() {
        let dir = std::env::temp_dir().join(format!("fastio-dl-keep-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("file.bin");
        let temp = ApiClient::partial_path(&output);
        tokio::fs::write(&output, b"pre-existing")
            .await
            .expect("write output");
        tokio::fs::write(&temp, b"junk").await.expect("write temp");

        let streamed = Err(CliError::Parse("boom".to_owned()));
        let _ = ApiClient::finalize_download(streamed, &temp, &output).await;
        // The pre-existing file is untouched.
        let contents = tokio::fs::read(&output).await.expect("read output");
        assert_eq!(
            contents, b"pre-existing",
            "existing file must not be clobbered"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn partial_path_is_unique_sibling_with_partial_suffix() {
        let out = std::path::Path::new("/tmp/some/dir/report.pdf");
        let temp = ApiClient::partial_path(out);
        // Same parent directory → rename stays on one filesystem.
        assert_eq!(temp.parent(), out.parent());
        // Built from the output name, scoped by pid, and ends with `.partial`.
        let temp_name = temp
            .file_name()
            .and_then(|n| n.to_str())
            .expect("temp file name is valid utf-8");
        assert!(temp_name.starts_with("report.pdf."), "got: {temp_name}");
        assert!(temp_name.ends_with(".partial"), "got: {temp_name}");
        assert!(
            temp_name.contains(&format!(".{}.", std::process::id())),
            "temp name must embed the pid: {temp_name}"
        );
    }

    #[test]
    fn partial_path_is_unique_per_call() {
        // Two calls for the SAME target must yield distinct temps so concurrent
        // downloads cannot collide on or clobber each other's partial.
        let out = std::path::Path::new("/tmp/some/dir/report.pdf");
        let a = ApiClient::partial_path(out);
        let b = ApiClient::partial_path(out);
        assert_ne!(a, b, "partial paths must be unique per call");
    }

    #[tokio::test]
    async fn concurrent_downloads_to_same_target_do_not_clobber_temps() {
        // FIX C: two concurrent download flows for the SAME output must each
        // get their own unique temp (create_new succeeds for both) and neither
        // clobbers the other's partial. The last finalize wins on the target.
        let dir = std::env::temp_dir().join(format!("fastio-dl-concur-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("file.bin");

        let temp_a = ApiClient::partial_path(&output);
        let temp_b = ApiClient::partial_path(&output);
        assert_ne!(temp_a, temp_b, "concurrent temps must differ");

        // Both temps create cleanly with create_new (no collision).
        for (temp, bytes) in [(&temp_a, b"aaaa".as_slice()), (&temp_b, b"bbbb".as_slice())] {
            let mut f = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(temp)
                .await
                .expect("create_new unique temp");
            f.write_all(bytes).await.expect("write temp");
            f.flush().await.expect("flush temp");
        }
        // The other call's temp is intact (not truncated) while both exist.
        assert_eq!(
            tokio::fs::read(&temp_a).await.expect("read a"),
            b"aaaa",
            "temp_a not clobbered by temp_b"
        );

        // Finalize both; each only ever touches its own temp.
        ApiClient::finalize_download(Ok(4), &temp_a, &output)
            .await
            .expect("finalize a");
        ApiClient::finalize_download(Ok(4), &temp_b, &output)
            .await
            .expect("finalize b");
        // A valid (4-byte) result is at the target; no temps remain.
        let final_contents = tokio::fs::read(&output).await.expect("read output");
        assert!(matches!(final_contents.as_slice(), b"aaaa" | b"bbbb"));
        assert!(
            !dir_has_partial(&dir).await,
            "no leftover .partial after both finalize"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stream_to_temp_does_not_truncate_an_existing_partial() {
        // An unrelated pre-existing file at the (would-be) temp path must NOT be
        // truncated: create_new(true) fails instead, leaving it intact.
        let dir = std::env::temp_dir().join(format!("fastio-dl-notrunc-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("file.bin");
        let temp = ApiClient::partial_path(&output);
        tokio::fs::write(&temp, b"unrelated-existing")
            .await
            .expect("write temp");

        // We cannot easily forge a reqwest::Response here, so assert the
        // open-with-create_new semantics directly: opening the existing path
        // with create_new must fail with AlreadyExists, never truncate.
        let err = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await
            .expect_err("create_new must refuse an existing file");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        let contents = tokio::fs::read(&temp).await.expect("read temp");
        assert_eq!(
            contents, b"unrelated-existing",
            "existing partial must not be truncated"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn create_temp_errors_and_does_not_delete_pre_existing_file() {
        // FIX E: when the unique temp path is somehow already taken, create_temp
        // must FAIL (create_new) and must NOT delete the pre-existing file — we
        // never created it, so we must never remove it. This guards the
        // ownership boundary: create_temp is the gate before any cleanup path.
        let dir = std::env::temp_dir().join(format!("fastio-dl-owngate-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("file.bin");
        let temp = ApiClient::partial_path(&output);
        // Pre-create a file at the would-be temp path (stale/crashed prior
        // partial, or an unrelated file that collided on the name).
        tokio::fs::write(&temp, b"do-not-touch")
            .await
            .expect("write pre-existing temp");

        let res = ApiClient::create_temp(&temp).await;
        assert!(res.is_err(), "create_temp must fail when the path exists");
        if let Err(CliError::Io(e)) = &res {
            assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        } else {
            panic!("expected an Io(AlreadyExists) error");
        }
        // The pre-existing file is still present and untouched.
        let contents = tokio::fs::read(&temp).await.expect("read temp");
        assert_eq!(
            contents, b"do-not-touch",
            "create_temp must not delete a file it did not create"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn atomic_replace_overwrites_existing_destination() {
        // FIX D/F: replacing an existing dest must succeed and leave the NEW
        // content at dest. On Unix the first rename atomically replaces; on
        // Windows the backup-swap path lands the same result.
        let dir = std::env::temp_dir().join(format!("fastio-dl-replace-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let dest = dir.join("file.bin");
        let temp = ApiClient::partial_path(&dest);
        tokio::fs::write(&dest, b"old-contents")
            .await
            .expect("write dest");
        tokio::fs::write(&temp, b"new-contents")
            .await
            .expect("write temp");

        ApiClient::atomic_replace(&temp, &dest)
            .await
            .expect("replace existing destination");
        let contents = tokio::fs::read(&dest).await.expect("read dest");
        assert_eq!(contents, b"new-contents", "destination must be overwritten");
        assert!(
            tokio::fs::metadata(&temp).await.is_err(),
            "temp must be renamed away"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn backup_swap_replace_lands_new_content_and_clears_backup() {
        // FIX F (a): the backup-swap path itself (exercised directly, since on
        // Unix the plain rename never reaches it) must replace dest with the new
        // content and leave no leftover .bak behind.
        let dir = std::env::temp_dir().join(format!("fastio-dl-bswap-ok-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let dest = dir.join("file.bin");
        let temp = ApiClient::partial_path(&dest);
        tokio::fs::write(&dest, b"original")
            .await
            .expect("write dest");
        tokio::fs::write(&temp, b"replacement")
            .await
            .expect("write temp");

        ApiClient::backup_swap_replace(&temp, &dest)
            .await
            .expect("backup swap must succeed");
        let contents = tokio::fs::read(&dest).await.expect("read dest");
        assert_eq!(contents, b"replacement", "dest must hold the new content");
        assert!(
            tokio::fs::metadata(&temp).await.is_err(),
            "temp must be consumed"
        );
        assert!(
            !dir_has_backup(&dir).await,
            "no .bak should remain after a successful swap"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn backup_swap_replace_failure_restores_original_dest() {
        // FIX F (b): if the temp → dest rename FAILS after the backup move, the
        // original dest must be restored so the user never loses their file.
        // We force the failure by pointing at a temp that does not exist, so the
        // inner rename errors and the rollback path runs.
        let dir = std::env::temp_dir().join(format!("fastio-dl-bswap-fail-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let dest = dir.join("file.bin");
        let missing_temp = dir.join("does-not-exist.partial");
        tokio::fs::write(&dest, b"precious-original")
            .await
            .expect("write dest");

        let res = ApiClient::backup_swap_replace(&missing_temp, &dest).await;
        assert!(
            res.is_err(),
            "a failed replacement must surface an error, not silently succeed"
        );
        // The user's original file survived: it was restored from the backup.
        let contents = tokio::fs::read(&dest)
            .await
            .expect("dest must still exist after a failed replace");
        assert_eq!(
            contents, b"precious-original",
            "the original dest must be restored on a failed replace"
        );
        assert!(
            !dir_has_backup(&dir).await,
            "the backup must be moved back to dest, leaving none behind"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn backup_path_is_unique_sibling_with_bak_suffix() {
        // FIX F: the backup path must be a same-directory sibling (so the move
        // stays on one filesystem) and unique per call (so concurrent replaces
        // never collide on the same backup name).
        let dest = std::path::Path::new("/tmp/some/dir/report.pdf");
        let a = ApiClient::backup_path(dest);
        let b = ApiClient::backup_path(dest);
        assert_eq!(
            a.parent(),
            dest.parent(),
            "backup must be a sibling of dest"
        );
        let name = a
            .file_name()
            .and_then(|n| n.to_str())
            .expect("backup name is valid utf-8");
        assert!(name.starts_with("report.pdf."), "got: {name}");
        assert!(
            std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("bak")),
            "got: {name}"
        );
        assert!(
            name.contains(&format!(".{}.", std::process::id())),
            "backup name must embed the pid: {name}"
        );
        assert_ne!(a, b, "backup paths must be unique per call");
    }

    // ----- download_file_stream end-to-end against a loopback server (FIX 1/2/3) -----

    /// Return true if `dir` contains any file whose name ends in `.partial`.
    /// Used instead of recomputing the (now unique-per-call) temp path.
    async fn dir_has_partial(dir: &std::path::Path) -> bool {
        dir_has_suffix(dir, ".partial").await
    }

    /// Return true if `dir` contains any file whose name ends in `.bak`.
    /// Used to assert the backup-swap (FIX F) leaves no leftover backup.
    async fn dir_has_backup(dir: &std::path::Path) -> bool {
        dir_has_suffix(dir, ".bak").await
    }

    /// Return true if `dir` contains any file whose name ends in `suffix`.
    async fn dir_has_suffix(dir: &std::path::Path, suffix: &str) -> bool {
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return false;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(suffix))
            {
                return true;
            }
        }
        false
    }

    /// Serve a single HTTP/1.1 response with `status_line`, `content_type`,
    /// and `body`, then close. Returns the bound `127.0.0.1:<port>` address.
    /// Used to exercise `download_file_stream` without a live API.
    async fn spawn_one_shot_server(
        status_line: &'static str,
        content_type: &'static str,
        body: &'static [u8],
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Drain the request headers (read once; enough for a GET).
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let header = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn download_file_stream_writes_2xx_json_body_to_disk() {
        // FIX 3: a 2xx application/json response is a SUCCESS stream (the
        // audit-certificate contract) and must be written to disk, not
        // rejected as an error envelope.
        let body = br#"{"audit":"certificate","ok":true}"#;
        let addr = spawn_one_shot_server("200 OK", "application/json", body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let dir = std::env::temp_dir().join(format!("fastio-dl-json-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("audit.json");

        let written = client
            .download_file_stream("/audit/download/", &output)
            .await
            .expect("2xx json body should stream to disk");
        assert_eq!(written, body.len() as u64);
        let contents = tokio::fs::read(&output).await.expect("read output");
        assert_eq!(contents, body, "json success body streamed verbatim");
        // No leftover *.partial after the atomic rename (the temp name is
        // unique per call, so scan the directory rather than recomputing it).
        assert!(
            !dir_has_partial(&dir).await,
            "no .partial left behind after success"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn download_file_stream_surfaces_non_2xx_and_writes_no_file() {
        // A non-2xx JSON envelope must be surfaced as CliError::Api with no
        // output file created.
        let body = br#"{"result":"no","error":{"code":403,"text":"forbidden"}}"#;
        let addr = spawn_one_shot_server("403 Forbidden", "application/json", body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let dir = std::env::temp_dir().join(format!("fastio-dl-403-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("nope.bin");

        let err = client
            .download_file_stream("/signed/download/", &output)
            .await
            .expect_err("403 must error");
        match err {
            CliError::Api(api) => assert_eq!(api.http_status, 403),
            other => panic!("unexpected error variant: {other:?}"),
        }
        assert!(
            tokio::fs::metadata(&output).await.is_err(),
            "no output file on error"
        );
        assert!(
            !dir_has_partial(&dir).await,
            "no .partial on error (the temp this call created is removed)"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ----- PATCH / PUT / form helper parity (method + url + body) -----

    /// Build a client whose request builders we can inspect via `.build()`.
    fn parity_client() -> ApiClient {
        ApiClient::new("https://api.example/current", Some("tok".to_owned()))
            .expect("client builds")
    }

    #[test]
    fn patch_json_uses_patch_method_url_and_json_body() {
        let client = parity_client();
        let body = serde_json::json!({"name": "x"});
        let req = client
            .inner
            .patch(client.url("/workflows/1/"))
            .json(&body)
            .build()
            .expect("builds");
        assert_eq!(req.method(), reqwest::Method::PATCH);
        assert_eq!(
            req.url().as_str(),
            "https://api.example/current/workflows/1/"
        );
        let sent = req.body().and_then(reqwest::Body::as_bytes).unwrap_or(&[]);
        assert_eq!(sent, br#"{"name":"x"}"#);
    }

    #[test]
    fn put_json_uses_put_method_and_url() {
        let client = parity_client();
        let body = serde_json::json!({"v": 1});
        let req = client
            .inner
            .put(client.url("/org/1/billing/"))
            .json(&body)
            .build()
            .expect("builds");
        assert_eq!(req.method(), reqwest::Method::PUT);
        assert_eq!(
            req.url().as_str(),
            "https://api.example/current/org/1/billing/"
        );
    }

    #[test]
    fn patch_form_uses_patch_method_and_form_body() {
        let client = parity_client();
        let mut form = HashMap::new();
        form.insert("output".to_owned(), "{\"k\":1}".to_owned());
        let req = client
            .inner
            .patch(client.url("/workflows/1/steps/2/output/"))
            .form(&form)
            .build()
            .expect("builds");
        assert_eq!(req.method(), reqwest::Method::PATCH);
        let ct = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(ct, "application/x-www-form-urlencoded");
        let sent =
            String::from_utf8_lossy(req.body().and_then(reqwest::Body::as_bytes).unwrap_or(&[]))
                .into_owned();
        assert!(sent.contains("output="), "form body carries output: {sent}");
    }

    // ----- download_file_stream error-sniff branch -----

    #[test]
    fn stream_sniff_treats_non_success_as_error() {
        // Non-2xx → error (the small body is read and surfaced).
        assert!(ApiClient::stream_response_is_error(false));
    }

    #[test]
    fn stream_sniff_streams_all_2xx_regardless_of_content_type() {
        // 2xx is ALWAYS streamed — including the audit-certificate endpoint's
        // 2xx application/json SUCCESS body, which earlier content-type
        // sniffing wrongly rejected (FIX 3).
        assert!(!ApiClient::stream_response_is_error(true));
    }

    #[test]
    fn put_form_uses_put_method_and_form_content_type() {
        let client = parity_client();
        let mut form = HashMap::new();
        form.insert("k".to_owned(), "v".to_owned());
        let req = client
            .inner
            .put(client.url("/x/"))
            .form(&form)
            .build()
            .expect("builds");
        assert_eq!(req.method(), reqwest::Method::PUT);
        let ct = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(ct, "application/x-www-form-urlencoded");
    }

    // ----- post_empty_raw: authed POST with NO body / NO content-type -----

    /// Build the request `post_empty_raw` issues and assert it carries no
    /// body and no `Content-Type` (the AI chat cancel contract is "Body:
    /// Empty", ai.txt:625), while still attaching the bearer token.
    #[test]
    fn post_empty_raw_builds_bodyless_authed_post() {
        let client = parity_client();
        let mut req = client.inner.post(client.url("/x/ai/agent/c/cancel/"));
        if let Some(auth) = client.auth_header() {
            req = req.header(AUTHORIZATION, auth);
        }
        let built = req.build().expect("builds");
        assert_eq!(built.method(), reqwest::Method::POST);
        assert_eq!(
            built.url().as_str(),
            "https://api.example/current/x/ai/agent/c/cancel/"
        );
        // No JSON (or any) body, and therefore no Content-Type header.
        let sent = built.body().and_then(reqwest::Body::as_bytes);
        assert!(
            sent.is_none() || sent == Some(&b""[..]),
            "cancel request must carry no body, got {sent:?}"
        );
        assert!(
            built.headers().get(CONTENT_TYPE).is_none(),
            "cancel request must not set a Content-Type"
        );
        // The bearer token is still attached.
        assert!(
            built.headers().get(AUTHORIZATION).is_some(),
            "cancel request must be authenticated"
        );
    }

    /// End-to-end: `post_empty_raw` reaches a real socket, the server sees a
    /// POST with a zero-length body and no `Content-Type`, and the 2xx body
    /// is returned verbatim (no envelope unwrap).
    #[tokio::test]
    async fn post_empty_raw_sends_no_body_over_the_wire() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let body = br#"{"success":true,"no_pending_message":true}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
                let _ = tx.send(request);
            }
        });
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let body: Value = client
            .post_empty_raw("/ai/agent/c/cancel/")
            .await
            .expect("empty-body POST succeeds");
        // 2xx body returned verbatim (no envelope unwrap).
        assert_eq!(body["no_pending_message"], Value::Bool(true));

        let request = rx.await.expect("server captured request");
        assert!(
            request.starts_with("POST /ai/agent/c/cancel/"),
            "expected POST to cancel path, got: {request}"
        );
        let lower = request.to_ascii_lowercase();
        assert!(
            !lower.contains("content-type:"),
            "empty-body POST must not send a Content-Type: {request}"
        );
        // No request body: a zero Content-Length (or none) and no trailing
        // payload after the header terminator.
        let after_headers = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            after_headers.is_empty(),
            "empty-body POST must send no payload, got body: {after_headers:?}"
        );
    }
}
