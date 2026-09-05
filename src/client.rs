#![allow(clippy::missing_errors_doc)]

/// HTTP client wrapper for the Fast.io REST API.
///
/// Handles request construction, authentication header injection,
/// response envelope unwrapping, rate-limit detection, and automatic
/// retry with exponential backoff for transient network failures.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
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

/// Byte cap on any ERROR body this client buffers — the 429 read in
/// [`ApiClient::rate_limit_error`] and the non-2xx read in
/// [`ApiClient::download_file_stream`].
///
/// A lockout envelope is a few hundred bytes; 64 KiB is generous for any
/// legitimate error document while still bounding a hostile or runaway one.
///
/// **This does NOT bound a decompression bomb.** `Cargo.toml` enables reqwest's
/// `json`/`stream`/`multipart` features and NOT `gzip`/`brotli`/`deflate`/`zstd`
/// (verified with `cargo tree -e features`), so reqwest never advertises
/// `Accept-Encoding` and never decompresses — the cap sees exactly what came off
/// the wire. The threat is therefore absent rather than mitigated.
/// **If a decompression feature is ever enabled, revisit this**: the
/// cap would then apply to decompressed bytes (good) but one decoder buffer is
/// allocated before the check. A compressed 429 today simply fails to parse and
/// falls back to the header/60s path, losing the lockout identity.
const ERROR_BODY_CAP: usize = 64 * 1024;

/// Wall-clock deadline on those same reads.
///
/// Required IN ADDITION to the byte cap: the streaming clients have no overall
/// body timeout, so a chunked response that trickles bytes indefinitely would
/// never reach the cap and never finish. Short on purpose — the wait value is a
/// convenience and is never worth stalling a download for.
const ERROR_BODY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

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
/// family below; their JSON-list envelope siblings are injectable.
///
/// Matched as a case-insensitive substring of the request path. The
/// pre-existing-`output=` guard in [`Self::output_injectable`] is a separate
/// layer that prevents sending two `output=` parameters.
const OUTPUT_INJECT_DENY_SUBSTRINGS: &[&str] = &[
    // Non-envelope / binary / raw / auth paths only.
    "/read/",
    "/download/",
    "/oauth/",
];

/// JSON object / form keys whose VALUE is a credential and must be redacted
/// before any request or response body is trace-logged.
///
/// Compared case-insensitively against each object/form key. Some API responses
/// return one-time secrets (outbound-subscription create/rotate, realtime-token
/// mint, the OAuth token exchange/refresh) in the body; `RUST_LOG=trace` would
/// otherwise leak them via a shared trace line that runs BEFORE any
/// command-level redaction. Request forms (OAuth refresh, password grants)
/// carry credentials in their values too. The placeholder substituted for a
/// matched value is [`REDACTED_PLACEHOLDER`].
const SECRET_LOG_KEYS: &[&str] = &[
    "secret",
    "token",
    "auth_token",
    "access_token",
    "refresh_token",
    "signing_secret",
    "inbound_signing_key",
    "outbound_secret",
    "api_key",
    "apikey",
    "password",
    // Password-change/reset forms use numbered fields.
    "password1",
    "password2",
    // Current-password proof on `/user/update/` (password-change + email-change).
    "current_password",
    "private_key",
    "client_secret",
    // Billing: `public_key` is a Stripe *publishable* key (not strictly secret),
    // redacted for defense-in-depth; the invoice URLs grant access to hosted
    // invoice / PDF views and genuinely should not appear in trace logs
    // (see the published API docs).
    "public_key",
    "hosted_invoice_url",
    "invoice_pdf",
    // OAuth PKCE token-exchange form fields (one-time, but still credentials).
    "code",
    "code_verifier",
    // Email-verification one-time code (`email_token` on `/user/email/validate/`);
    // the same one-time-credential class as `code` (api/auth.rs `email_verify`).
    "email_token",
    // Storage/file lock ownership token (form field).
    "lock_token",
    // File-lock device fingerprint (`client_info` on the acquire-lock form): a
    // JSON blob of client metadata. Already redacted in the CLI Debug impls, so
    // redact it in form logs too for consistency (api/storage.rs `lock_acquire_form`).
    "client_info",
    // Preview-access JWT returned by `get_preview_url` (`downloadToken` →
    // case-insensitive `downloadtoken`): a bearer-equivalent read token.
    "downloadtoken",
    // Invitation bearer capability (org/workspace/share invite acceptance).
    "invitation_key",
    // Coordination-room agent invite link (`invite_url` returned by
    // `create_invite`): a one-time capability URL embedding the redemption token
    // in its path. Redacted from logged response bodies for defense-in-depth;
    // the token segment inside the URL PATH is separately masked by
    // `redact_path_for_log` (the URL itself never appears as a request path, but
    // the value is a capability and must not leak via a traced body).
    "invite_url",
    // File Share recipient link password (sent in the `x-ve-password` request
    // header; defense-in-depth in case it ever lands in a logged body/form).
    PASSWORD_HEADER,
];

/// Name of the request header carrying a File Share recipient's link password.
///
/// The password travels ONLY in this header — never in a URL or query string.
/// The built [`HeaderValue`] is marked sensitive (see [`build_password_header`])
/// so reqwest's own debug logging redacts it, and the header name is registered
/// in [`SECRET_LOG_KEYS`] for defense-in-depth.
const PASSWORD_HEADER: &str = "x-ve-password";

/// Placeholder written in place of a redacted secret value when trace-logging
/// a response body.
const REDACTED_PLACEHOLDER: &str = "[redacted]";

/// Deep-walk a JSON value and return a clone in which the VALUE of any object
/// key named like a credential (case-insensitive match against
/// [`SECRET_LOG_KEYS`]) is replaced with [`REDACTED_PLACEHOLDER`], preserving
/// the surrounding structure.
///
/// Used to sanitize a request or response body BEFORE it is trace-logged. The
/// match is on the KEY name (not the value shape), so a redacted secret is
/// replaced regardless of whether it was a string, number, array, or object.
/// Non-secret keys — and the structure of objects and arrays — are preserved
/// verbatim so the trace remains useful.
fn redact_secret_values_for_log(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if is_secret_log_key(k) {
                        (k.clone(), Value::String(REDACTED_PLACEHOLDER.to_owned()))
                    } else {
                        (k.clone(), redact_secret_values_for_log(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(redact_secret_values_for_log).collect())
        }
        other => other.clone(),
    }
}

/// Whether a (case-insensitive) object/form key names a credential per
/// [`SECRET_LOG_KEYS`].
fn is_secret_log_key(key: &str) -> bool {
    SECRET_LOG_KEYS
        .iter()
        .any(|name| key.eq_ignore_ascii_case(name))
}

/// Redact a form-field map for trace logging.
///
/// Form-encoded request bodies (OAuth token exchange/refresh, password grants,
/// secret rotations) carry credentials in their VALUES. Returns an ordered map
/// (so the trace line is stable) in which any secret-named field's value is the
/// [`REDACTED_PLACEHOLDER`]. Used before any `?form` request is trace-logged.
fn redact_form_for_log(form: &HashMap<String, String>) -> std::collections::BTreeMap<&str, &str> {
    form.iter()
        .map(|(k, v)| {
            let v = if is_secret_log_key(k) {
                REDACTED_PLACEHOLDER
            } else {
                v.as_str()
            };
            (k.as_str(), v)
        })
        .collect()
}

/// Whether a path segment at the 2FA enable/disable position is a known,
/// NON-secret channel name rather than a disable verification token.
///
/// The enable (`POST /user/auth/2factor/{channel}/`) and disable
/// (`DELETE /user/auth/2factor/{token}/`) routes share the same single-segment
/// shape; only the HTTP method distinguishes them, which [`redact_path_for_log`]
/// cannot see. A real disable token is a numeric/alphanumeric code that never
/// equals one of these channel literals, so allowlisting the enable channels
/// keeps them visible in traces while still masking every token. A future
/// channel not on this list is masked too — harmless over-caution, never an
/// under-mask of a real token.
fn is_known_2fa_channel(segment: &str) -> bool {
    matches!(segment, "sms" | "totp" | "whatsapp")
}

/// Mask the secret segment of a known secret-bearing request path before it is
/// trace-logged.
///
/// A handful of auth endpoints embed a one-time credential directly in the URL
/// PATH (not the body or query), so neither form nor query redaction can reach
/// it. This masks ONLY those known templates, replacing the secret segment with
/// [`REDACTED_PLACEHOLDER`] while leaving the route structure intact so the
/// trace stays useful:
///
/// * `/user/password/<code>/` and `/user/password/<code>/details/` — the
///   password-reset code (`password_reset_complete` / `password_reset_check`).
/// * `/user/auth/2factor/auth/<code>/` — the post-sign-in 2FA code
///   (`two_factor_verify`).
/// * `/user/auth/2factor/verify/<token>/` — the TOTP-setup token
///   (`two_factor_verify_setup`).
/// * `/user/auth/2factor/<token>/` — the disable token (`two_factor_disable`),
///   distinguished from the non-secret enable channel
///   (`/user/auth/2factor/<channel>/`) via [`is_known_2fa_channel`].
/// * `/room/invites/<token>/redeem` — the coordination-room agent invite
///   redemption token (`redeem_invite`): a one-time bearer capability embedded
///   in the request PATH, so neither form nor query redaction reaches it.
///
/// Any path that does not match one of these templates is returned unchanged,
/// so ordinary paths (e.g. `/workspace/<id>/...`, where the id is not a secret)
/// are never altered. The code/token is a single URL-encoded segment, so
/// splitting on `/` isolates it exactly.
fn redact_path_for_log(path: &str) -> String {
    let mut segments: Vec<&str> = path.split('/').collect();
    // Slice patterns are length-specific, so the fixed-length 2FA `auth`/`verify`
    // arms below never collide with the shorter disable/enable arm.
    let mask_idx = match segments.as_slice() {
        // Password-reset code (both the complete and `/details/` check variants),
        // and the room-invite redeem token (`/room/invites/{token}/redeem`) — all
        // carry the secret at index 3.
        //
        // The room-invite arm is DELIBERATELY RETAINED after Coordination Rooms
        // were removed (2026-08-25). The route no longer exists server-side, so
        // this arm is expected to be dead — but the asymmetry decides it:
        // keeping it costs one match arm, and dropping it costs a leaked token
        // in logs if any old invite URL is ever passed through this client.
        // Scrubbing a secret that cannot occur is free; failing to scrub one
        // that does is unrecoverable. Remove only with evidence that no such URL
        // can reach this code path — dead-route status alone is not that.
        ["", "user", "password", _, ""]
        | ["", "user", "password", _, "details", ""]
        | ["", "room", "invites", _, "redeem"] => Some(3),
        ["", "user", "auth", "2factor", "auth" | "verify", _, ""] => Some(5),
        ["", "user", "auth", "2factor", segment, ""] if !is_known_2fa_channel(segment) => Some(4),
        _ => None,
    };
    match mask_idx {
        Some(idx) => {
            segments[idx] = REDACTED_PLACEHOLDER;
            segments.join("/")
        }
        None => path.to_owned(),
    }
}

/// Redact a JSON request/response body presented as text for trace logging.
///
/// Parses the text and applies [`redact_secret_values_for_log`]; if the text is
/// not valid JSON (e.g. a proxy HTML error page) it cannot contain a structured
/// secret field, so it is returned as-is. Callers should already be inside a
/// `tracing::enabled!(Level::TRACE)` guard so the parse only runs when needed.
fn redact_text_body_for_log(text: &str) -> String {
    serde_json::from_str::<Value>(text).map_or_else(
        |_| text.to_owned(),
        |v| redact_secret_values_for_log(&v).to_string(),
    )
}

/// Parse a recipient link password into a sensitive [`HeaderValue`] for the
/// `x-ve-password` header.
///
/// Built from the password's raw UTF-8 BYTES via [`HeaderValue::from_bytes`]
/// rather than [`HeaderValue::from_str`]: the link password contract allows any
/// 1-255 character (UTF-8) value, but `from_str` rejects every non-ASCII byte,
/// which would make a perfectly valid password (e.g. `"pässwört→"`, settable via
/// the management form) unsendable from the CLI. `from_bytes` accepts the
/// non-ASCII bytes while STILL rejecting control bytes / newlines (the bytes
/// HTTP headers genuinely cannot carry), so it fails (no panic) with
/// [`CliError::InvalidHeaderValue`] only on a truly un-encodable value. The
/// error names only the header, NEVER the value — the value is a secret. On
/// success the header is marked sensitive ([`HeaderValue::set_sensitive`]) so
/// reqwest's own debug logging redacts it.
///
/// Defined as a free function (not a method) so it can be parsed ONCE, before
/// the non-fallible retry closure that [`ApiClient::send_request_with_retry`]
/// re-runs per attempt, then cheaply cloned inside the closure. Exposed
/// `pub(crate)` so the raw-reqwest upload paths (which cannot reach a private
/// method) reuse this single builder instead of duplicating it.
pub(crate) fn build_password_header(password: &SecretString) -> Result<HeaderValue, CliError> {
    let mut value = HeaderValue::from_bytes(password.expose_secret().as_bytes()).map_err(|_| {
        CliError::InvalidHeaderValue {
            header: PASSWORD_HEADER,
        }
    })?;
    value.set_sensitive(true);
    Ok(value)
}

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
    /// Eagerly-built client used only by [`Self::download_file_stream`].
    ///
    /// Unlike [`Self::inner`], it carries a connect timeout but **no** overall
    /// request timeout, so large signed-PDF / audit-bundle downloads are not
    /// killed by [`DEFAULT_TIMEOUT_SECS`] mid-stream. Built once at construction
    /// (the only builder failure mode is a TLS-backend init issue that would
    /// already have failed [`Self::inner`]) and reused thereafter (connection
    /// pooling) so a download burst does not rebuild the client per call.
    streaming_client: reqwest::Client,
    /// Eagerly-built no-redirect ENVELOPE client for the leak-safe File Share
    /// JSON/form consumption + write-back paths that carry an optional
    /// `x-ve-password` (`get_with_password` / `post_with_password` — details,
    /// versions, grants, complete, status).
    ///
    /// Like [`Self::no_redirect_streaming_client`] it sets
    /// [`reqwest::redirect::Policy::none`] (the load-bearing leak-safety
    /// property — see that field), but UNLIKE the streaming client it carries the
    /// ordinary [`DEFAULT_TIMEOUT_SECS`] overall request timeout. These calls
    /// return a bounded JSON/form envelope, not a multi-MB stream, so a stalled
    /// response must time out exactly as it would on [`Self::inner`] rather than
    /// hang indefinitely. Built once at construction so a builder failure is a
    /// hard error, NEVER a silent fall-back to the redirect-following
    /// [`Self::inner`] (which would re-introduce the credential-forwarding leak).
    no_redirect_envelope_client: reqwest::Client,
    /// Eagerly-built no-redirect STREAMING client for the leak-safe File Share
    /// binary consumption paths that carry an optional `x-ve-password`
    /// (`download_file_stream_with_password` /
    /// `download_preview_following_redirect`, including the token-bearing follow).
    ///
    /// Same timeout profile as [`Self::streaming_client`] (connect timeout only,
    /// **no** overall body timeout, so a large download is not killed mid-stream)
    /// PLUS [`reqwest::redirect::Policy::none`] so a 3xx is NEVER auto-followed.
    /// That no-redirect policy is the load-bearing safety property for
    /// password-bearing requests: reqwest follows up to 10 redirects by default
    /// and does NOT strip custom headers on a cross-origin redirect, so
    /// auto-following would forward the `x-ve-password` (and bearer) header to
    /// the `Location` target — a CDN. Routing every password-bearing stream onto
    /// this client and treating any 3xx as a terminal (or manually-followed,
    /// header-stripped) case fails closed instead. Built once at construction so
    /// a builder failure is a hard error, NEVER a silent fall-back to the
    /// redirect-following [`Self::inner`].
    no_redirect_streaming_client: reqwest::Client,
}

/// Whether a request may be PUT ON THE WIRE A SECOND TIME to recover from a
/// failure.
///
/// Governs all three doors through which a retry can re-send: a failed
/// response-body read ([`ApiClient::should_retry_body_read`]), an HTTP
/// 502/503/504 ([`ApiClient::should_retry_gateway_error`]), and a transport
/// send failure ([`ApiClient::should_retry_transport_error`]).
///
/// Replay-safety is a property of the ENDPOINT, not of the HTTP method. The
/// method is only a PROXY for it, and this API breaks the proxy: it has
/// **actionful GETs** — `GET /user/auth/2factor/send/{channel}/` sends an SMS,
/// `GET /websocket/auth/{id}` and `…/transform/image/requestread/` mint tokens,
/// `…/preview/{type}/preauthorize/` preauthorizes. Re-sending any of those on a
/// failure that the server may already have acted on would apply the effect a
/// SECOND time.
///
/// So the plumbing takes this as an EXPLICIT argument with no default: a new
/// send path must state its answer rather than inherit a permissive one.
#[derive(Clone, Copy, Debug)]
enum ReplayPolicy {
    /// Re-send whenever the failure is transient and the HTTP method is itself
    /// safe to repeat. For endpoints that are pure reads. This is the behavior
    /// every ordinary request has always had, on all three doors.
    IfMethodIsSafe,
    /// The server ACTS on this request, so never re-send it on a failure it may
    /// already have acted on: a lost body and a 502-504 are refused outright,
    /// and of the transport failures only a CONNECT error — where the
    /// connection was provably never established — is still retried.
    Never,
}

/// A response that has been received (status + headers) but whose body has NOT
/// been read yet, carried together with the two facts a later body-read retry
/// needs: the METHOD that produced it (is re-sending it safe?) and the retry
/// ATTEMPT it was obtained on (how much of the shared budget is left?).
struct SentResponse {
    /// The response; its body is still unread.
    resp: reqwest::Response,
    /// Method of the request that produced `resp`, taken from the BUILT request
    /// rather than from the endpoint path or the calling helper's name.
    method: reqwest::Method,
    /// Zero-based retry attempt this response was obtained on.
    attempt: u32,
}

/// A response whose body has been read to completion — what the buffered
/// (non-streaming) send path hands to its body handlers.
///
/// `body` keeps the two failure modes apart, because they are not equally
/// recoverable: `Err` is a TRANSPORT-level failure (the body never fully
/// arrived — truncated, connection closed mid-body, read timeout), whereas a
/// body that arrived intact but is not valid JSON is an `Ok` here and fails
/// later in the handler's `serde_json` parse. Only the former is ever worth
/// re-sending — and only for an idempotent method (see
/// [`ApiClient::should_retry_body_read`]).
struct BufferedResponse {
    /// Status of the response the body belongs to.
    status: reqwest::StatusCode,
    /// The fully-read body, or the transport failure that prevented reading it.
    body: Result<Bytes, reqwest::Error>,
}

impl ApiClient {
    /// Create a new client targeting `base_url` with an optional bearer token.
    ///
    /// The token is wrapped in [`SecretString`] to prevent accidental logging
    /// and to zeroize memory on drop.
    pub fn new(base_url: &str, token: Option<String>) -> Result<Self, CliError> {
        Self::with_detail(base_url, token, None)
    }

    /// Create a client that never consults the system proxy configuration.
    ///
    /// TEST SUPPORT ONLY — the production constructors deliberately keep proxy
    /// support. reqwest defaults to `auto_sys_proxy`, and hyper-util's
    /// interception consults only `NO_PROXY` with NO implicit loopback bypass,
    /// so a developer's `HTTP_PROXY` (or a macOS system proxy) captures
    /// requests aimed at `127.0.0.1`: a test driving an in-process loopback
    /// stub would send its `Authorization` header to the proxy host and never
    /// reach the stub. A child process can be given a scrubbed environment, but
    /// an in-process test cannot scrub its own (`set_var` is unsafe under Rust
    /// 2024), so the opt-out has to be built into the client.
    #[doc(hidden)]
    pub fn new_without_proxy(base_url: &str, token: Option<String>) -> Result<Self, CliError> {
        Self::build(base_url, token, None, true)
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
        Self::build(base_url, token, detail, false)
    }

    /// Shared constructor: build every inner client and assemble the struct.
    ///
    /// `no_proxy` disables system-proxy discovery on all four clients (see
    /// [`Self::new_without_proxy`]); production construction passes `false`.
    fn build(
        base_url: &str,
        token: Option<String>,
        detail: Option<OutputDetail>,
        no_proxy: bool,
    ) -> Result<Self, CliError> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_USER_AGENT));

        let inner = Self::apply_no_proxy(
            reqwest::Client::builder()
                .default_headers(headers)
                .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .connect_timeout(Duration::from_secs(30))
                .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
                .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST),
            no_proxy,
        )
        .build()?;

        // The streaming and both no-redirect clients are built EAGERLY here so a
        // builder failure is a hard error at construction, NEVER a silent
        // fall-back to the redirect-following `inner` (which for the no-redirect
        // clients would defeat the entire leak-safety design). The no-redirect
        // ENVELOPE client carries the ordinary request timeout (bounded JSON/form
        // responses); the no-redirect STREAMING client omits the body timeout
        // (multi-MB downloads).
        let streaming_client = Self::build_streaming_client(no_proxy)?;
        let no_redirect_envelope_client = Self::build_no_redirect_envelope_client(no_proxy)?;
        let no_redirect_streaming_client = Self::build_no_redirect_streaming_client(no_proxy)?;

        Ok(Self {
            inner,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token: token.map(SecretString::from),
            detail,
            streaming_client,
            no_redirect_envelope_client,
            no_redirect_streaming_client,
        })
    }

    /// Build the dedicated streaming-download client.
    ///
    /// Carries [`STREAM_CONNECT_TIMEOUT_SECS`] connect timeout but no overall
    /// request timeout (see [`Self::streaming_client`]). Returns the builder
    /// error verbatim — there is NO fall-back to [`Self::inner`]; the only
    /// documented failure mode is a TLS-backend init issue that would already
    /// have failed `inner`, so surfacing it is correct rather than substituting
    /// a differently-configured client.
    fn build_streaming_client(no_proxy: bool) -> Result<reqwest::Client, CliError> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_USER_AGENT));
        Self::apply_no_proxy(
            reqwest::Client::builder()
                .default_headers(headers)
                .connect_timeout(Duration::from_secs(STREAM_CONNECT_TIMEOUT_SECS))
                .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
                .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST),
            no_proxy,
        )
        .build()
        .map_err(CliError::Http)
    }

    /// Disable system-proxy discovery on `builder` when `no_proxy` is set.
    fn apply_no_proxy(builder: reqwest::ClientBuilder, no_proxy: bool) -> reqwest::ClientBuilder {
        if no_proxy {
            builder.no_proxy()
        } else {
            builder
        }
    }

    /// Build the dedicated no-redirect ENVELOPE client.
    ///
    /// [`reqwest::redirect::Policy::none`] (so a 3xx is surfaced rather than
    /// auto-followed — the leak-safety property) PLUS the ordinary
    /// [`DEFAULT_TIMEOUT_SECS`] overall request timeout, because the callers
    /// (`get_with_password` / `post_with_password`) exchange a bounded JSON/form
    /// envelope and must not hang indefinitely on a stalled response. The
    /// connect timeout matches [`Self::inner`] (30s). Returns the builder error
    /// verbatim and NEVER falls back to a redirect-following client —
    /// substituting one would re-introduce the exact leak this client exists to
    /// prevent (forwarding `x-ve-password` to a `Location` target).
    fn build_no_redirect_envelope_client(no_proxy: bool) -> Result<reqwest::Client, CliError> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_USER_AGENT));
        Self::apply_no_proxy(
            reqwest::Client::builder()
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .connect_timeout(Duration::from_secs(30))
                .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
                .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST),
            no_proxy,
        )
        .build()
        .map_err(CliError::Http)
    }

    /// Build the dedicated no-redirect STREAMING client.
    ///
    /// Same connect-timeout / NO-body-timeout profile as
    /// [`Self::build_streaming_client`] (so a multi-MB download is not killed
    /// mid-stream) but with [`reqwest::redirect::Policy::none`] so a 3xx is
    /// surfaced rather than auto-followed (see
    /// [`Self::no_redirect_streaming_client`]). Returns the builder error
    /// verbatim and NEVER falls back to a redirect-following client —
    /// substituting one here would re-introduce the exact leak this client
    /// exists to prevent (forwarding `x-ve-password` to a `Location` target).
    fn build_no_redirect_streaming_client(no_proxy: bool) -> Result<reqwest::Client, CliError> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_USER_AGENT));
        Self::apply_no_proxy(
            reqwest::Client::builder()
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(STREAM_CONNECT_TIMEOUT_SECS))
                .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
                .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST),
            no_proxy,
        )
        .build()
        .map_err(CliError::Http)
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

    /// Drop the bearer token so subsequent requests are sent unauthenticated.
    #[allow(dead_code)]
    pub fn clear_token(&mut self) {
        self.token = None;
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

    /// Perform an authenticated GET that returns the response body as raw
    /// text, WITHOUT requesting `?output=markdown` and WITHOUT envelope
    /// unwrapping.
    ///
    /// This is for genuinely-raw content endpoints (e.g. a storage node's
    /// `/read/` endpoint, which streams the raw file bytes — for a `.md` file
    /// that is the markdown source). Optional query `params` are forwarded
    /// verbatim (e.g. `version_id`, `token`). On a non-2xx HTTP status the body
    /// is surfaced as `CliError::Api.message` (capped) by the shared
    /// text-response handler; a JSON error envelope from the server is left
    /// intact for the caller to inspect if needed.
    ///
    /// Note: this does NOT use [`Self::build_get`], so the `--detail`
    /// `?output=` injection never fires here — correct, because a raw content
    /// endpoint must not receive `?output=`.
    pub async fn get_raw_text(
        &self,
        path: &str,
        params: Option<&HashMap<String, String>>,
    ) -> Result<String, CliError> {
        tracing::trace!(method = "GET", path, "api request (raw text)");
        self.send_raw_text_with_retry(|| {
            let mut req = self.inner.get(self.url(path));
            if let Some(p) = params {
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
    /// paths (read/download/oauth) are excluded.
    ///
    /// **Pure reads only.** To recover a lost response body this may put the
    /// request on the wire a second time. If the server ACTS on the request —
    /// sends a message, mints a token or credential, preauthorizes, creates
    /// something — use [`Self::get_side_effecting`] instead.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path = %redact_path_for_log(path), "api request");
        self.send_with_retry(|| self.build_get(path)).await
    }

    /// Perform a GET whose handling has a SERVER-SIDE EFFECT, and unwrap the
    /// API envelope.
    ///
    /// Same URL building and same `?output=<detail>` injection as [`Self::get`].
    /// What differs is RETRY: this endpoint is never put on the wire a second
    /// time on a failure the server may already have acted on, because a lost
    /// response means the effect ALREADY happened and a second send would apply
    /// it AGAIN — a second SMS to the user's phone, a second minted token. See
    /// the door-by-door breakdown below.
    ///
    /// Use this — not [`Self::get`] — for every GET the server acts on. The HTTP
    /// method cannot distinguish them: `GET` is merely a PROXY for
    /// replay-safety, and this API breaks the proxy with endpoints such as
    /// `GET /user/auth/2factor/send/{channel}/` (delivers an SMS, phone call, or
    /// chat message), `GET /websocket/auth/{id}` and
    /// `…/transform/image/requestread/` (mint tokens), and
    /// `…/preview/{type}/preauthorize/` (preauthorizes). Only the endpoint's own
    /// builder knows, so it must opt out here.
    ///
    /// # The side-effecting-GET ledger
    ///
    /// The endpoints routed through this family were not guessed. They were
    /// derived on **2026-08-10** by enumerating every call site that can reach
    /// a RETRYING send through a GET-issuing helper — all **9** of them
    /// (`get`, `get_with_params`, `get_with_auth`, `get_with_auth_and_params`,
    /// `get_no_auth_with_params`, `get_partial_envelope`, `get_with_password`,
    /// `get_markdown`, `get_raw_text`), **172 call sites across 24 files** —
    /// and asking one question of each: **does a replay SEND, MINT, SPEND, or
    /// CREATE anything?** The nine that answer yes:
    ///
    /// | endpoint | what a replay would do |
    /// |---|---|
    /// | `/user/auth/` (sign-in) | mint a second JWT, or burn a second failed attempt against lockout |
    /// | `/user/auth/2factor/send/{channel}/` | deliver a second SMS, call, or chat message |
    /// | `/oauth/authorize/` | create a second pending authorization request |
    /// | `/websocket/auth/{id}` | mint a second realtime token |
    /// | `…/preview/{type}/preauthorize/` | preauthorize a second time |
    /// | `…/transform/image/requestread/` | mint a second download token |
    /// | `/workspace/{ws}/storage/{node}/requestread/` | mint a second download token |
    /// | `/{ctx}/{ctx_id}/storage/{node}/requestread/` | mint a second download token |
    /// | `/events/search/summarize/` | spend AI credits twice |
    ///
    /// An earlier sweep enumerated only `client.get(` call sites and therefore
    /// MISSED sign-in, which reaches the retrying path via `get_with_auth`.
    /// That is why the boundary above is stated in terms of HELPERS: enumerate
    /// every helper, not one spelling.
    ///
    /// Considered and CLEARED, deliberately left on [`Self::get`]:
    /// `GET /user/me/autosync/{state}/` does mutate, but idempotently — setting
    /// the same state twice yields the same state — so a replay is harmless.
    /// Its absence here is a decision, not an oversight; do not "fix" it.
    ///
    /// # Exactly what this protects — and what it does NOT
    ///
    /// [`ReplayPolicy::Never`] governs **all three** doors through which a
    /// request can go out twice:
    ///
    /// 1. **Lost response body** — refused outright
    ///    ([`Self::should_retry_body_read`]).
    /// 2. **HTTP 502/503/504** — refused outright, because the upstream may
    ///    have processed the request ([`Self::should_retry_gateway_error`]).
    /// 3. **Transport send failure** — refused EXCEPT for a connect error,
    ///    where the connection was provably never established so the server
    ///    never saw the request ([`Self::should_retry_transport_error`]). An
    ///    ambiguous timeout is refused; a connect failure is still retried so
    ///    these calls are not needlessly fragile.
    ///
    /// What that does NOT cover: anything the SERVER does with a request it
    /// received. If the server processes a request and the effect is not
    /// idempotent on its side, a client-side policy cannot help — and a user
    /// re-running the command is always a second call.
    ///
    /// Two gaps this design cannot detect, stated so the guarantee is not read
    /// as blanket:
    ///
    /// - **A new actionful GET that reaches for [`Self::get`]** instead of this
    ///   family. Nothing in the type system catches it; this ledger and the
    ///   warning on [`Self::get`] stand in for a compiler check.
    /// - **The generic passthrough helpers** (`api::ai::ai_api`,
    ///   `api::workspace::metadata_api`) take a caller-supplied `sub_path`, so
    ///   the endpoint is not known statically and a future actionful AI or
    ///   metadata GET could ride through them replayable. None does today.
    /// - **A 3xx redirect.** The shared client follows redirects, and that
    ///   follow is outside all three doors above — a redirect would put the
    ///   request on the wire again regardless of policy. Checked 2026-08-10:
    ///   none of the endpoints in the ledger documents a 3xx and no caller
    ///   expects one, so this is latent rather than live; an actionful GET that
    ///   started redirecting would bypass the gate.
    pub async fn get_side_effecting<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        tracing::trace!(
            method = "GET",
            path = %redact_path_for_log(path),
            "api request (side-effecting)"
        );
        self.send_with_retry_no_replay(|| self.build_get(path))
            .await
    }

    /// Perform a GET request with query parameters.
    ///
    /// **Pure reads only** — see [`Self::get`]. The side-effecting counterpart
    /// is [`Self::get_with_params_side_effecting`].
    pub async fn get_with_params<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "GET",
            path,
            params = ?redact_form_for_log(params),
            "api request"
        );
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

    /// Perform a GET with query parameters whose handling has a SERVER-SIDE
    /// EFFECT, and unwrap the API envelope.
    ///
    /// The query-parameter shape of [`Self::get_side_effecting`]: same URL and
    /// parameter handling as [`Self::get_with_params`], but never re-sent on a
    /// failure the server may already have acted on. See the ledger on
    /// [`Self::get_side_effecting`] for which endpoints belong here, why, and
    /// exactly which doors that closes.
    pub async fn get_with_params_side_effecting<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "GET",
            path,
            params = ?redact_form_for_log(params),
            "api request (side-effecting)"
        );
        let has_output = Self::params_have_output(params);
        self.send_with_retry_no_replay(|| {
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
    ///
    /// **Pure reads only** — see [`Self::get`]. The side-effecting counterpart
    /// is [`Self::get_with_auth_side_effecting`].
    ///
    /// **Sign-in was moved OFF this helper and must not be moved back.**
    /// `GET /user/auth/` MINTS A JWT, so a re-send after the server has already
    /// answered mints a second one. It rode this helper until 2026-08-10 and now
    /// uses [`Self::get_with_auth_side_effecting`]; this one is retained only for
    /// genuinely pure custom-auth reads. Restoring an actionful caller here
    /// re-opens that defect silently — nothing in the type system objects.
    #[allow(dead_code)]
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

    /// Perform a GET with a custom `Authorization` header whose handling has a
    /// SERVER-SIDE EFFECT, and unwrap the API envelope.
    ///
    /// The custom-auth shape of [`Self::get_side_effecting`]: same URL and
    /// header handling as [`Self::get_with_auth`], but never re-sent on a
    /// failure the server may already have acted on. Exists for
    /// `GET /user/auth/` (sign-in), which MINTS a JWT on every call. See the
    /// ledger on [`Self::get_side_effecting`].
    pub async fn get_with_auth_side_effecting<T: DeserializeOwned>(
        &self,
        path: &str,
        auth_value: &str,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "GET",
            path,
            "api request (custom auth, side-effecting)"
        );
        let auth_owned = auth_value.to_owned();
        self.send_with_retry_no_replay(|| {
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
        tracing::trace!(
            method = "GET",
            path,
            params = ?redact_form_for_log(params),
            "api request (custom auth)"
        );
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
        tracing::trace!(
            method = "GET",
            path,
            params = ?redact_form_for_log(params),
            "api request (no auth)"
        );
        let has_output = Self::params_have_output(params);
        self.send_with_retry(|| {
            let req = self.inner.get(self.url(path)).query(params);
            self.inject_output_query(req, path, has_output)
        })
        .await
    }

    /// Perform an UNAUTHENTICATED GET with query parameters whose handling has
    /// a SERVER-SIDE EFFECT, and unwrap the API envelope.
    ///
    /// The unauthenticated shape of [`Self::get_side_effecting`]: same URL and
    /// parameter handling as [`Self::get_no_auth_with_params`], but never
    /// re-sent on a failure the server may already have acted on. Exists for
    /// `GET /oauth/authorize/`, which CREATES a pending authorization request on
    /// every call. See the ledger on [`Self::get_side_effecting`].
    pub async fn get_no_auth_with_params_side_effecting<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "GET",
            path,
            params = ?redact_form_for_log(params),
            "api request (no auth, side-effecting)"
        );
        let has_output = Self::params_have_output(params);
        self.send_with_retry_no_replay(|| {
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
        tracing::trace!(
            method = "POST",
            path = %redact_path_for_log(path),
            form = ?redact_form_for_log(form),
            "api request"
        );
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
        tracing::trace!(
            method = "POST",
            path = %redact_path_for_log(path),
            form = ?redact_form_for_log(form),
            "api request (no auth)"
        );
        self.send_with_retry(|| self.inner.post(self.url(path)).form(form))
            .await
    }

    /// Perform a form-encoded POST without authentication, with a SINGLE attempt
    /// and NO retry loop, then unwrap the API envelope.
    ///
    /// For **non-idempotent single-use endpoints** where a retry can consume the
    /// resource and lose the response — specifically the room-invite redeem
    /// (`POST /room/invites/{token}/redeem`), which atomically burns the one-time
    /// token and mints an `api_key`. On the shared [`Self::post_no_auth`] retry
    /// path, a lost response after a server-side burn (a timeout or 502-504 that
    /// arrives AFTER the server committed) would trigger a re-POST that the
    /// server answers with the uniform not-found — silently destroying the minted
    /// key. This method therefore sends exactly once: a transient failure is
    /// surfaced to the caller (who must treat the invite as possibly-consumed)
    /// rather than retried. Rate-limit response headers are still parsed for the
    /// caller's benefit; a 429 is surfaced as [`CliError::RateLimit`].
    pub async fn post_no_auth_once<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "POST",
            path = %redact_path_for_log(path),
            form = ?redact_form_for_log(form),
            "api request (no auth, single-attempt)"
        );
        let resp = self
            .send_request_once(|| self.inner.post(self.url(path)).form(form))
            .await?;
        self.handle_response(resp).await
    }

    /// Perform an AUTHENTICATED form-encoded POST with NO retry loop — exactly
    /// one attempt on the wire.
    ///
    /// For non-idempotent endpoints where a blind retry could repeat a
    /// side-effecting mint (e.g. participant `rotate`, which mints a fresh
    /// agent key and revokes the old one on every call): a transport
    /// failure after the server processed the request must surface to the
    /// caller rather than silently minting twice. 429 maps to `RateLimit`
    /// without sleeping (mirrors [`Self::post_no_auth_once`]).
    pub async fn post_once<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "POST",
            path = %redact_path_for_log(path),
            form = ?redact_form_for_log(form),
            "api request (single-attempt)"
        );
        let resp = self
            .send_request_once(|| {
                let mut req = self.inner.post(self.url(path)).form(form);
                if let Some(auth) = self.auth_header() {
                    req = req.header(AUTHORIZATION, auth);
                }
                req
            })
            .await?;
        self.handle_response(resp).await
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
        tracing::trace!(method = "POST", path, form = ?redact_form_for_log(form), "api request (no auth, raw)");
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
        tracing::trace!(method = "POST", path, body = %redact_secret_values_for_log(body), "api request (json)");
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
        tracing::trace!(method = "POST", path, body = %redact_secret_values_for_log(body), "api request (json, raw)");
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
    /// chat cancel endpoint), where sending `{}` with
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

    /// Perform an authenticated POST with **no request body at all** and apply
    /// the shared envelope handling.
    ///
    /// The envelope-handling counterpart to [`Self::post_empty_raw`]: like
    /// [`Self::post_json`] it routes through the shared retry / rate-limit /
    /// envelope path, but the wire request carries neither a body nor a
    /// `Content-Type` header. The shared handler unwraps a nested `response`
    /// sub-object when present; otherwise it returns the full envelope verbatim
    /// (minus server bookkeeping) — so a named-key boolean envelope such as the
    /// signing `/send/` response (`{"result": true, …}`, NO `response` key) is
    /// preserved intact rather than collapsed. Use it for endpoints whose
    /// contract is literally "Body: Empty" yet still return an envelope (e.g. the
    /// verified workspace-suffixed `/workspace/{ws}/sign_envelopes/{env}/send/`
    /// action; `signing.txt`'s send body shape is authoritative, its route table
    /// is stale), where sending `{}` with `Content-Type: application/json` would
    /// diverge from the documented bodyless contract.
    pub async fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, "api request (empty body)");
        self.send_with_retry(|| {
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
        tracing::trace!(method = "PATCH", path, body = %redact_secret_values_for_log(body), "api request (json)");
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
        tracing::trace!(method = "PUT", path, body = %redact_secret_values_for_log(body), "api request (json)");
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
        tracing::trace!(method = "PATCH", path, form = ?redact_form_for_log(form), "api request");
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
        tracing::trace!(method = "PUT", path, form = ?redact_form_for_log(form), "api request");
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
        tracing::trace!(method = "DELETE", path = %redact_path_for_log(path), "api request");
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
    pub async fn delete_with_params<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "DELETE",
            path,
            params = ?redact_form_for_log(params),
            "api request"
        );
        self.send_with_retry(|| {
            let mut req = self.inner.delete(self.url(path)).query(params);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a query-parameter DELETE whose params carry a SECRET, and unwrap
    /// the API envelope.
    ///
    /// Identical to [`Self::delete_with_params`] except that a transport error
    /// has the request URL stripped before it is logged or wrapped, so the
    /// secret cannot leak through an error message. Use this whenever a value in
    /// `params` is a credential — e.g. a lock token.
    pub async fn delete_with_params_scrubbed<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(
            method = "DELETE",
            path,
            params = ?redact_form_for_log(params),
            "api request"
        );
        self.send_with_retry_scrubbed(|| {
            let mut req = self.inner.delete(self.url(path)).query(params);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Perform a form-encoded DELETE and unwrap the API envelope.
    ///
    /// # Deprecated
    ///
    /// The Fast.io API does not read a request body on `DELETE`. Every measured
    /// endpoint reports the field as missing and the operation silently does not
    /// happen — verified on `storage/{node}/lock/` (`205516`, node stayed locked)
    /// and `share/{id}/delete/` (`130987`, whose text names the query parameter
    /// explicitly). All three former callers were broken by it.
    ///
    /// Use [`ApiClient::delete_with_params`] instead, which delivers the fields
    /// as a query string.
    #[deprecated(
        note = "the Fast.io API does not read a DELETE body; use `delete_with_params` (query string) instead"
    )]
    pub async fn delete_with_form<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "DELETE", path, form = ?redact_form_for_log(form), "api request");
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
    /// (`/workspace/{ws}/sign_envelopes/{env}/audit/download/`) returns a 2xx
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
        let mut req = self.streaming_client.get(self.url(path));
        if let Some(auth) = self.auth_header() {
            req = req.header(AUTHORIZATION, auth);
        }
        let resp = req.send().await.map_err(CliError::Http)?;
        let status = resp.status();

        // Decide error-vs-stream by HTTP status only (a 2xx JSON body is a
        // valid success payload for the audit-certificate endpoint).
        if Self::stream_response_is_error(status.is_success()) {
            let http_status = status.as_u16();
            // BOUNDED. This previously called `resp.json()` on the reasoning
            // that "the error body is small" — an assumption about a remote
            // server, not a guarantee. `streaming_client` is built with a
            // CONNECT timeout only and no overall body timeout, so a non-2xx
            // that dribbles chunked bytes and never terminates would hang a
            // signing download indefinitely, and a fast enormous body would be
            // buffered whole.
            //
            // The 429 path guards the identical hang with the same bounded
            // reader. Over the cap or past the deadline yields an empty body →
            // the generic status message, which is the same fallback already
            // used for a missing or non-JSON body.
            let read = tokio::time::timeout(
                ERROR_BODY_DEADLINE,
                Self::read_body_capped(resp, ERROR_BODY_CAP),
            )
            .await;
            // `Err` = deadline; `Ok(None)` = over the cap or a transport failure
            // mid-body. Either way there IS a reason and we could not obtain it —
            // which is NOT the same as a response that carried no body, and must
            // not be reported as one. See [`crate::error::ERR_BODY_UNAVAILABLE`].
            let Ok(Some(bytes)) = read else {
                return Err(ApiError {
                    code: 0,
                    error_code: Some(crate::error::ERR_BODY_UNAVAILABLE.to_owned()),
                    // `Display` already prints `[HTTP {status}]`, so this does
                    // NOT repeat it — unlike the older generic fallback
                    // ("API request failed with HTTP 404"), which renders as
                    // "[HTTP 404] API request failed with HTTP 404". That
                    // duplication is pre-existing and used broadly; no reason to
                    // copy it into new text.
                    message: "the server's error body could not be read (too large or too \
                              slow), so the reason it gave is unavailable"
                        .to_owned(),
                    http_status,
                    details: None,
                }
                .into());
            };
            // A body we DID read but cannot parse stays on the pre-existing
            // generic fallback — that one really is "no usable body".
            let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            if tracing::enabled!(tracing::Level::TRACE) {
                tracing::trace!(
                    body = %redact_secret_values_for_log(&body),
                    "stream download error body"
                );
            }
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

    // ─── File Share consumption (optional `x-ve-password`) ──────────────────
    //
    // These helpers thread an OPTIONAL recipient link password through the
    // `x-ve-password` header. They serve BOTH authenticated and anonymous
    // consumers: the bearer token is attached only when the client holds one,
    // so the same method works for `anyone_with_link` (anonymous), the
    // registered tiers, and named-people grants. The password (when present) is
    // parsed to a sensitive `HeaderValue` ONCE — before the non-fallible retry
    // closure — and cheaply cloned inside it.

    /// Perform a GET that may carry an optional `x-ve-password` header, and
    /// unwrap the API envelope.
    ///
    /// The `Authorization: Bearer` header is attached ONLY when the client
    /// holds a token, so this single method serves both authenticated and
    /// anonymous File Share consumption (details / versions). When `password`
    /// is `Some`, it is parsed to a sensitive [`HeaderValue`] before the retry
    /// closure (so a bad value fails fast, without a panic, via
    /// [`CliError::InvalidHeaderValue`]) and cloned per attempt.
    ///
    /// **Redirect safety:** when a password is present the request goes out on
    /// the no-redirect [`Self::no_redirect_envelope_client`] and an unexpected
    /// 3xx is a terminal error (see [`Self::send_no_redirect`]) — reqwest does
    /// NOT strip custom headers across a redirect, so auto-following one would
    /// forward the `x-ve-password` header to the `Location` target. When
    /// `password` is `None` the behavior is unchanged: the request rides the
    /// ordinary redirect-following [`Self::inner`] path via [`Self::build_get`].
    ///
    /// **`--detail` injection:** the password branch routes the no-redirect GET
    /// through [`Self::inject_output_query`] so a configured `--detail`
    /// appends `?output=<detail>` on password-protected details / versions
    /// exactly as it does on the unauthenticated/authed paths — preserving the
    /// [`OUTPUT_INJECT_DENY_SUBSTRINGS`] deny-substring behavior. Previously the
    /// branch built directly on the client and silently dropped `--detail`.
    pub async fn get_with_password<T: DeserializeOwned>(
        &self,
        path: &str,
        password: Option<&SecretString>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "GET", path, "api request (optional password)");
        let Some(password) = password else {
            // No password → preserve the existing redirect-following behavior
            // exactly.
            return self.send_with_retry(|| self.build_get(path)).await;
        };
        // Password present → parse once (fail fast on a bad value) and send on
        // the no-redirect ENVELOPE client (it carries the ordinary request
        // timeout), failing closed on any 3xx.
        let password_header = build_password_header(password)?;
        let url = self.url(path);
        self.send_no_redirect(|| {
            // Apply `?output=<detail>` injection on the no-redirect GET BEFORE
            // attaching auth/password headers, honoring the deny-substring
            // guard inside `inject_output_query`.
            let req = self.no_redirect_envelope_client.get(&url);
            let mut req = self.inject_output_query(req, path, false);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req.header(PASSWORD_HEADER, password_header.clone())
        })
        .await
    }

    /// Perform a form-encoded POST that may carry an optional `x-ve-password`
    /// header, and unwrap the API envelope.
    ///
    /// Like [`Self::post`] but with the optional recipient link password. The
    /// bearer token is attached only when the client holds one. The existing
    /// form trace-redaction applies (secret-named form values are masked before
    /// logging); the password header itself is sensitive and never logged.
    ///
    /// **Redirect safety:** mirrors [`Self::get_with_password`] — a present
    /// password routes the POST onto the no-redirect client and fails closed on
    /// any 3xx; a `None` password preserves the ordinary redirect-following
    /// [`Self::inner`] behavior exactly.
    pub async fn post_with_password<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
        password: Option<&SecretString>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, form = ?redact_form_for_log(form), "api request (optional password)");
        let Some(password) = password else {
            // No password → preserve the existing redirect-following behavior
            // exactly (identical to `post`).
            return self
                .send_with_retry(|| {
                    let mut req = self.inner.post(self.url(path)).form(form);
                    if let Some(auth) = self.auth_header() {
                        req = req.header(AUTHORIZATION, auth);
                    }
                    req
                })
                .await;
        };
        let password_header = build_password_header(password)?;
        let url = self.url(path);
        // Send on the no-redirect ENVELOPE client (it carries the ordinary
        // request timeout), failing closed on any 3xx.
        self.send_no_redirect(|| {
            let mut req = self.no_redirect_envelope_client.post(&url).form(form);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req.header(PASSWORD_HEADER, password_header.clone())
        })
        .await
    }

    /// Perform a form-encoded POST whose form body may carry a sensitive value
    /// (e.g. a link `password` form field), failing closed on any 3xx so the body
    /// is never replayed to a redirect `Location` target, then unwrap the API
    /// envelope.
    ///
    /// Unlike [`Self::post`] (which rides the redirect-FOLLOWING [`Self::inner`]
    /// client, so a 307/308 would replay the ENTIRE form body — including any
    /// `password=…` field — to the untrusted `Location`), this routes onto the
    /// no-redirect [`Self::no_redirect_envelope_client`] and treats an unexpected
    /// 3xx as a TERMINAL [`CliError::Parse`] that names neither the URL nor any
    /// form value (see [`Self::send_no_redirect`] /
    /// [`Self::send_request_with_retry_inner`]). The bearer token is attached only
    /// when the client holds one. Use this for management writes whose form may
    /// contain a credential (File Share create / update). This is distinct from
    /// [`Self::post_with_password`], whose secret travels in the `x-ve-password`
    /// HEADER; here the sensitive value is a FORM FIELD.
    pub async fn post_sensitive_form<T: DeserializeOwned>(
        &self,
        path: &str,
        form: &HashMap<String, String>,
    ) -> Result<T, CliError> {
        tracing::trace!(method = "POST", path, form = ?redact_form_for_log(form), "api request (sensitive form, fail-closed)");
        let url = self.url(path);
        self.send_no_redirect(|| {
            let mut req = self.no_redirect_envelope_client.post(&url).form(form);
            if let Some(auth) = self.auth_header() {
                req = req.header(AUTHORIZATION, auth);
            }
            req
        })
        .await
    }

    /// Stream a binary GET to disk with an optional `x-ve-password` header,
    /// returning the bytes written.
    ///
    /// Mirrors [`Self::download_file_stream`] (status-based error sniff, unique
    /// temp file, atomic finalize) but also attaches the optional recipient link
    /// password. The bearer token is added only when present, so the same method
    /// serves authenticated and anonymous File Share downloads
    /// (`/storage/read/`, `/storage/versions/{v}/read/`).
    ///
    /// **Redirect safety:** when a password is present the request rides the
    /// no-redirect [`Self::no_redirect_streaming_client`] and any 3xx fails
    /// closed (it is NOT followed) — reqwest does not strip the `x-ve-password`
    /// header across a redirect, so auto-following one would leak it to the
    /// `Location` target. When `password` is `None` the request uses the
    /// redirect-following [`Self::streaming_client`] exactly as
    /// [`Self::download_file_stream`] does, preserving existing behavior.
    pub async fn download_file_stream_with_password(
        &self,
        path: &str,
        output_path: &std::path::Path,
        password: Option<&SecretString>,
    ) -> Result<u64, CliError> {
        tracing::trace!(
            method = "GET",
            path,
            "api request (stream download, optional password)"
        );
        // Password present → no-redirect STREAMING client (fail closed on a 3xx,
        // no body timeout); absent → the ordinary redirect-following streaming
        // client (unchanged behavior).
        let (client, password_header) = match password {
            Some(password) => (
                &self.no_redirect_streaming_client,
                Some(build_password_header(password)?),
            ),
            None => (&self.streaming_client, None),
        };
        let url = self.url(path);
        // Route the initial response through the shared 429 → RateLimit +
        // 502-504-retry seam BEFORE streaming the body. The body is not
        // consumed on any error path, so re-issuing the GET per retry is safe.
        let resp = self
            .send_streaming_with_retry(
                || {
                    let mut req = client.get(&url);
                    if let Some(auth) = self.auth_header() {
                        req = req.header(AUTHORIZATION, auth);
                    }
                    if let Some(value) = &password_header {
                        req = req.header(PASSWORD_HEADER, value.clone());
                    }
                    req
                },
                false,
            )
            .await?;
        let status = resp.status();

        // A password-bearing download is on the no-redirect client; a 3xx here
        // means the server redirected and we must NOT chase it (it would forward
        // the credential header). Fail closed with a resource-agnostic, secret-
        // and URL-free error rather than streaming the redirect body.
        if password.is_some() && status.is_redirection() {
            return Err(CliError::Parse(
                "the server returned an unexpected redirect for a \
                 password-protected download; refusing to follow it"
                    .to_owned(),
            ));
        }

        if Self::stream_response_is_error(status.is_success()) {
            let http_status = status.as_u16();
            // BOUNDED, and an unreadable body is MARKED rather than silently
            // becoming a bodyless-looking error. Same reasoning as
            // `download_file_stream`: this client has a CONNECT timeout only, so
            // an unbounded `resp.json()` could hang forever on a chunked body
            // that never terminates. The same pattern applies to every non-2xx
            // read in this file; all of them must stay bounded.
            let read = tokio::time::timeout(
                ERROR_BODY_DEADLINE,
                Self::read_body_capped(resp, ERROR_BODY_CAP),
            )
            .await;
            let Ok(Some(bytes)) = read else {
                return Err(ApiError {
                    code: 0,
                    error_code: Some(crate::error::ERR_BODY_UNAVAILABLE.to_owned()),
                    message: "the server's error body could not be read (too large or too \
                              slow), so the reason it gave is unavailable"
                        .to_owned(),
                    http_status,
                    details: None,
                }
                .into());
            };
            let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            if tracing::enabled!(tracing::Level::TRACE) {
                tracing::trace!(
                    body = %redact_secret_values_for_log(&body),
                    "stream download error body"
                );
            }
            return Err(Self::extract_error(&body, http_status).into());
        }

        let temp_path = Self::partial_path(output_path);
        let file = Self::create_temp(&temp_path).await?;
        let written = Self::stream_to_temp(resp, file).await;
        Self::finalize_download(written, &temp_path, output_path).await
    }

    /// Stream a File Share preview to disk, manually following at most one
    /// `307`/3xx redirect in a leak-safe way.
    ///
    /// The primary preview GET carries the bearer token (when present) plus the
    /// optional `x-ve-password` header. On ANY 3xx response
    /// ([`reqwest::StatusCode::is_redirection`]) the `Location` header is read,
    /// resolved against the request URL (relative references are joined via
    /// [`reqwest::Url::join`]), validated to be `http`/`https`, and the follow
    /// GET is re-issued on the SAME no-redirect client WITHOUT `Authorization`
    /// and WITHOUT `x-ve-password`. Dropping both headers on the follow is the
    /// key safety property: reqwest does NOT strip custom headers on a
    /// cross-origin redirect, so leaving them on would leak the link password
    /// (and bearer) to a CDN — the redirect URL embeds its own short-lived
    /// `download_token` that authorizes the follow, so neither header is needed.
    /// A SECOND redirect (or a 3xx on the follow) fails closed with a clear
    /// error; the client NEVER falls back to a redirect-following client. A
    /// `2xx` primary streams directly (single-file previews do not redirect).
    ///
    /// **Transient handling:** both the primary request and the follow GET run
    /// through [`Self::send_streaming_with_retry`], so a 429 surfaces as
    /// [`CliError::RateLimit`] and a transient HTTP 502-504 is retried before the
    /// redirect / stream decision. The body is not consumed on any error
    /// path, so re-issuing the GET per retry is safe.
    pub async fn download_preview_following_redirect(
        &self,
        path: &str,
        output_path: &std::path::Path,
        password: Option<&SecretString>,
    ) -> Result<u64, CliError> {
        tracing::trace!(
            method = "GET",
            path,
            "api request (preview, manual redirect)"
        );
        let request_url = self.url(path);
        // Parse the password ONCE (fail fast, no panic) so the retry closure is
        // non-fallible and just clones the sensitive header per attempt.
        let password_header = password.map(build_password_header).transpose()?;
        // The primary send carries the bearer + `x-ve-password`; its URL holds
        // no secret in path/query, so transport errors are left intact
        // (`scrub_url=false`). The FOLLOW URL (which embeds a short-lived
        // download_token) IS scrubbed via `scrub_url=true` below.
        let resp = self
            .send_streaming_with_retry(
                || {
                    let mut req = self.no_redirect_streaming_client.get(&request_url);
                    if let Some(auth) = self.auth_header() {
                        req = req.header(AUTHORIZATION, auth);
                    }
                    if let Some(value) = &password_header {
                        req = req.header(PASSWORD_HEADER, value.clone());
                    }
                    req
                },
                false,
            )
            .await?;
        let status = resp.status();

        // 3xx primary: follow exactly once, header-stripped, to the resolved
        // (validated) target. Anything else (2xx success or a non-redirect
        // error status) is handled by the shared streaming sink below.
        if status.is_redirection() {
            let follow_url = Self::resolve_redirect_location(resp.headers(), &request_url)?;
            // The follow rides the same 429/502-504 retry seam, with
            // `scrub_url=true` so a transport failure cannot leak the embedded
            // download_token via `reqwest::Error`'s Display.
            let follow_resp = self
                .send_streaming_with_retry(|| self.build_follow_request(follow_url.clone()), true)
                .await?;
            return self
                .finalize_streamed_response(follow_resp, output_path, true)
                .await;
        }

        self.finalize_streamed_response(resp, output_path, false)
            .await
    }

    /// Build the leak-safe preview FOLLOW request on the no-redirect streaming
    /// client.
    ///
    /// The redirect target's URL embeds a short-lived `download_token` that
    /// authorizes the read, so the follow GET deliberately carries NEITHER an
    /// `Authorization` header NOR an `x-ve-password` header — reqwest does not
    /// strip custom headers across a (cross-origin) redirect, so re-attaching
    /// either would leak a credential to the CDN. Built on the no-redirect
    /// streaming client so the follow itself cannot chase a further redirect.
    /// Extracted as a small helper (addendum F23 / H4) so a unit test can assert
    /// the built request carries no credential headers without a live server.
    fn build_follow_request(&self, url: reqwest::Url) -> reqwest::RequestBuilder {
        // No Authorization, no x-ve-password — the embedded download_token is the
        // sole authorizer for the follow.
        self.no_redirect_streaming_client.get(url)
    }

    /// Resolve and validate a redirect `Location` for the leak-safe preview
    /// follow.
    ///
    /// Pure helper (no I/O) so the relative-resolution and scheme-validation
    /// logic is unit-testable without a live server. Reads the `Location`
    /// header, resolves a relative reference against `request_url` via
    /// [`reqwest::Url::join`] (an absolute `Location` replaces it wholesale),
    /// and rejects any scheme other than `http`/`https`. Returns
    /// [`CliError::Parse`] when the header is missing, unreadable, unparseable,
    /// or carries a disallowed scheme.
    fn resolve_redirect_location(
        headers: &HeaderMap,
        request_url: &str,
    ) -> Result<reqwest::Url, CliError> {
        let location = headers
            .get(reqwest::header::LOCATION)
            .ok_or_else(|| {
                CliError::Parse("preview redirect is missing a Location header".to_owned())
            })?
            .to_str()
            .map_err(|_| {
                CliError::Parse("preview redirect Location header is not valid text".to_owned())
            })?;
        let base = reqwest::Url::parse(request_url).map_err(|e| {
            CliError::Parse(format!("could not parse the preview request URL: {e}"))
        })?;
        // `Url::join` resolves a relative reference against the base and, for an
        // absolute `Location`, returns it wholesale.
        let resolved = base.join(location).map_err(|e| {
            CliError::Parse(format!(
                "could not resolve the preview redirect target: {e}"
            ))
        })?;
        if !matches!(resolved.scheme(), "http" | "https") {
            return Err(CliError::Parse(format!(
                "preview redirect target uses an unsupported scheme: {}",
                resolved.scheme()
            )));
        }
        Ok(resolved)
    }

    /// Stream an (already-sent) response to `output_path`, treating any further
    /// redirect as a fail-closed error.
    ///
    /// Shared sink for [`Self::download_preview_following_redirect`]: a non-2xx
    /// status is surfaced as a structured [`CliError`] (a 3xx becomes an
    /// explicit "second redirect" error when `is_follow` — we never chase a
    /// chain), and a 2xx body streams to a unique temp file that is atomically
    /// finalized.
    async fn finalize_streamed_response(
        &self,
        resp: reqwest::Response,
        output_path: &std::path::Path,
        is_follow: bool,
    ) -> Result<u64, CliError> {
        let status = resp.status();
        if status.is_redirection() {
            // A redirect on the follow (or a second redirect) is a fail-closed
            // error — we never chase a redirect chain for a preview.
            return Err(CliError::Parse(if is_follow {
                "preview redirect chained to a second redirect; refusing to follow further"
                    .to_owned()
            } else {
                "preview returned an unexpected redirect".to_owned()
            }));
        }
        if Self::stream_response_is_error(status.is_success()) {
            let http_status = status.as_u16();
            // On the FOLLOW response the body comes from the redirect target (a
            // CDN) reached via the tokenized download URL. A CDN error page can
            // reflect the request URL — including the embedded `download_token` —
            // in its body text, so we must NOT mine that body into an error
            // message that reaches stderr / logs. Surface a generic, status-only
            // error (no body-derived text, no URL) for the follow. The
            // PRIMARY response is the Fast.io API envelope (no token in the body)
            // and is mined as usual so the server's real error message shows.
            if is_follow {
                if tracing::enabled!(tracing::Level::TRACE) {
                    tracing::trace!(
                        http_status,
                        "preview follow returned a non-success status (body suppressed)"
                    );
                }
                return Err(CliError::Api(ApiError {
                    code: 0,
                    error_code: None,
                    message: format!("the preview redirect target returned HTTP {http_status}"),
                    http_status,
                    details: None,
                }));
            }
            // BOUNDED, and an unreadable body is MARKED rather than silently
            // becoming a bodyless-looking error. Same reasoning as
            // `download_file_stream`: this client has a CONNECT timeout only, so
            // an unbounded `resp.json()` could hang forever on a chunked body
            // that never terminates. The same pattern applies to every non-2xx
            // read in this file; all of them must stay bounded.
            let read = tokio::time::timeout(
                ERROR_BODY_DEADLINE,
                Self::read_body_capped(resp, ERROR_BODY_CAP),
            )
            .await;
            let Ok(Some(bytes)) = read else {
                return Err(ApiError {
                    code: 0,
                    error_code: Some(crate::error::ERR_BODY_UNAVAILABLE.to_owned()),
                    message: "the server's error body could not be read (too large or too \
                              slow), so the reason it gave is unavailable"
                        .to_owned(),
                    http_status,
                    details: None,
                }
                .into());
            };
            let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            if tracing::enabled!(tracing::Level::TRACE) {
                tracing::trace!(
                    body = %redact_secret_values_for_log(&body),
                    "preview download error body"
                );
            }
            return Err(Self::extract_error(&body, http_status).into());
        }
        let temp_path = Self::partial_path(output_path);
        let file = Self::create_temp(&temp_path).await?;
        let mut written = Self::stream_to_temp(resp, file).await;
        // On the FOLLOW response the body streams from the redirect target,
        // whose URL embeds a short-lived download_token. A mid-stream
        // `reqwest::Error` carries that URL in its Display, so scrub it before it
        // can reach stderr / logs (H3 streaming-path audit). The primary
        // (non-follow) response streams from the API path, which carries no
        // secret in its URL, so it is left intact.
        if is_follow && let Err(CliError::Http(e)) = written {
            written = Err(CliError::Http(e.without_url()));
        }
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

    /// Send a streaming-download request with the SAME 429 / 502-504 / network
    /// retry semantics as [`Self::send_request_with_retry`], returning the final
    /// [`reqwest::Response`] WITHOUT consuming its body.
    ///
    /// This is the retry seam for the binary File Share consumption paths
    /// (`download_file_stream_with_password`, `download_preview_following_redirect`),
    /// which previously sent exactly once and so failed on a transient 502-504
    /// and surfaced a 429 as a generic error instead of [`CliError::RateLimit`].
    /// It re-issues `build_request` per attempt — safe because the body is NOT
    /// yet consumed on any of these error paths (the request bytes are simply
    /// re-sent). A 429 becomes [`CliError::RateLimit`]; HTTP 502-504 is retried
    /// with exponential backoff; a pre-send / mid-handshake `reqwest::Error` is
    /// retried per [`Self::is_retryable_error`]. Any other status (2xx, a
    /// non-retryable 4xx/5xx, or a 3xx) is returned to the caller to handle —
    /// the redirect / error-body / streaming decisions stay in the caller,
    /// exactly as before.
    ///
    /// `scrub_url` strips the request URL from a transport error
    /// ([`reqwest::Error::without_url`]) before it is wrapped — set it `true` on
    /// the token-bearing preview FOLLOW so a network failure cannot leak the
    /// embedded `download_token` to stderr / logs. The rate-limit-header check
    /// on a non-retried success mirrors the envelope path.
    async fn send_streaming_with_retry<F>(
        &self,
        build_request: F,
        scrub_url: bool,
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
                    "retrying streaming request after transient failure"
                );
                tokio::time::sleep(backoff).await;
            }

            match build_request().send().await {
                Ok(resp) => {
                    let status = resp.status();
                    tracing::trace!(status = status.as_u16(), "streaming api response");

                    if matches!(status.as_u16(), 502..=504) && attempt < MAX_RETRIES {
                        tracing::warn!(
                            status = status.as_u16(),
                            "received transient server error on streaming request, will retry"
                        );
                        last_error = Some(CliError::Api(ApiError {
                            code: 0,
                            error_code: None,
                            message: format!("transient server error (HTTP {})", status.as_u16()),
                            http_status: status.as_u16(),
                            details: None,
                        }));
                        continue;
                    }

                    if status.as_u16() == 429 {
                        // Body-first for the WAIT value, but never promote the
                        // body into an `ApiError` here: on the preview follow
                        // this response comes from a CDN reached via a tokenized
                        // URL and its text may reflect the `download_token`.
                        // See `allow_body_promotion` on [`Self::rate_limit_error`].
                        return Err(Self::rate_limit_error(resp, false).await);
                    }

                    Self::check_rate_limit(&resp);
                    return Ok(resp);
                }
                Err(e) if Self::is_retryable_error(&e) && attempt < MAX_RETRIES => {
                    let scrubbed = if scrub_url { e.without_url() } else { e };
                    tracing::warn!(error = %scrubbed, "transient streaming network error, will retry");
                    last_error = Some(CliError::Http(scrubbed));
                }
                Err(e) => {
                    let scrubbed = if scrub_url { e.without_url() } else { e };
                    return Err(CliError::Http(scrubbed));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            CliError::Parse("streaming request failed: all retries exhausted".to_owned())
        }))
    }

    /// Send a request with automatic retry and exponential backoff,
    /// returning the unprocessed [`reqwest::Response`] for body-shape-specific
    /// handlers to parse.
    ///
    /// Retries on connection errors, timeouts, and HTTP 502/503/504. A 429
    /// response is converted to [`CliError::RateLimit`] without retrying.
    /// Rate-limit headers are checked on every successful return. The request is
    /// built on the redirect-FOLLOWING [`Self::inner`] client (the closure picks
    /// the client), so a 3xx is transparently chased — appropriate for ordinary
    /// authenticated/anonymous traffic but NOT for password-bearing requests
    /// (see [`Self::send_no_redirect`]).
    ///
    /// The body is read by the caller and is therefore NOT covered by the retry
    /// loop; callers that want a failed body read retried too must use
    /// [`Self::send_and_read_with_retry`].
    async fn send_request_with_retry<F>(
        &self,
        build_request: F,
    ) -> Result<reqwest::Response, CliError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        // `IfMethodIsSafe` preserves this path's existing 502-504 retry
        // behavior exactly; no side-effecting endpoint uses the text path.
        Ok(self
            .send_request_with_retry_inner(
                &build_request,
                false,
                0,
                ReplayPolicy::IfMethodIsSafe,
                false,
            )
            .await?
            .resp)
    }

    /// Send a request with retry AND read its body to completion, retrying a
    /// failed BODY read as well — but only when re-sending the request is safe.
    ///
    /// The send retry and the body-read retry share ONE attempt budget
    /// ([`MAX_RETRIES`]) and one backoff schedule: a body-read retry resumes the
    /// send loop at the next attempt rather than starting a fresh budget, so the
    /// worst-case number of requests on the wire is unchanged.
    ///
    /// A body-read failure that is not retried is NOT an error here — it is
    /// handed to the caller's handler inside [`BufferedResponse::body`], so each
    /// handler keeps the exact wording and fallback behavior it has always had
    /// for an unreadable body.
    async fn send_and_read_with_retry<F>(
        &self,
        build_request: F,
        fail_closed_on_redirect: bool,
        replay: ReplayPolicy,
        scrub_url: bool,
    ) -> Result<BufferedResponse, CliError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut next_attempt = 0;
        loop {
            let sent = self
                .send_request_with_retry_inner(
                    &build_request,
                    fail_closed_on_redirect,
                    next_attempt,
                    replay,
                    scrub_url,
                )
                .await?;
            let status = sent.resp.status();
            match sent.resp.bytes().await {
                Ok(body) => {
                    return Ok(BufferedResponse {
                        status,
                        body: Ok(body),
                    });
                }
                Err(e) => {
                    if Self::should_retry_body_read(replay, &sent.method, sent.attempt) {
                        tracing::warn!(
                            error = %e,
                            method = %sent.method,
                            attempt = sent.attempt,
                            "response body read failed on a replay-safe request, will re-send"
                        );
                        next_attempt = sent.attempt + 1;
                        continue;
                    }
                    return Ok(BufferedResponse {
                        status,
                        body: Err(e),
                    });
                }
            }
        }
    }

    /// Whether a TRANSPORT-level send failure may be retried by re-sending the
    /// request.
    ///
    /// The classification of "transient" is unchanged
    /// ([`Self::is_retryable_error`]); this adds the replay question on top of
    /// it, and only ever makes the answer more conservative.
    ///
    /// **The connect/ambiguous split, from `reqwest` 0.12 semantics (verified
    /// in its `error.rs`, not assumed):**
    ///
    /// - `is_connect()` walks the source chain for a `hyper_util` connect
    ///   error, i.e. the connection was never ESTABLISHED. The request bytes
    ///   never reached the server, so re-sending cannot double-apply anything.
    ///   Safe even for a side-effecting endpoint — and keeping it retryable is
    ///   what stops these calls from being needlessly fragile on a flaky link.
    /// - `is_timeout()` is AMBIGUOUS: it fires for a connect timeout *and* for
    ///   a response timeout, and in the latter case the server may have
    ///   received, processed, and answered while the client gave up waiting.
    /// - `is_request()` is the broad `Kind::Request` bucket, which **includes
    ///   connect errors**. That is why this checks `is_connect()` to ALLOW
    ///   rather than checking `is_request()` to deny: denying on `is_request()`
    ///   would sweep up every connect error with it.
    ///
    /// So a [`ReplayPolicy::Never`] endpoint retries only the provably-unsent
    /// case. Ordinary requests keep today's behavior exactly.
    fn should_retry_transport_error(replay: ReplayPolicy, err: &reqwest::Error) -> bool {
        if !Self::is_retryable_error(err) {
            return false;
        }
        match replay {
            ReplayPolicy::IfMethodIsSafe => true,
            ReplayPolicy::Never => err.is_connect(),
        }
    }

    /// Whether an HTTP 502/503/504 may be retried by re-sending the request.
    ///
    /// A gateway error is ambiguous in exactly the way that matters: the
    /// upstream may have RECEIVED and PROCESSED the request and the gateway
    /// then lost the response. For a side-effecting endpoint that is the same
    /// double-apply hazard as a lost body arriving through a different door, so
    /// [`ReplayPolicy::Never`] closes this one too.
    ///
    /// Deliberately does NOT consult the HTTP method. Every ordinary request —
    /// including every POST / PUT / PATCH / DELETE — keeps retrying 502-504
    /// exactly as it always has. Whether the client should retry MUTATIONS on a
    /// gateway error at all is a genuine reliability-vs-correctness tradeoff
    /// affecting every mutating call in the CLI; it is PRE-EXISTING, untouched
    /// here, and not a decision to make as a side effect of this fix.
    fn should_retry_gateway_error(replay: ReplayPolicy) -> bool {
        matches!(replay, ReplayPolicy::IfMethodIsSafe)
    }

    /// Decide whether a FAILED RESPONSE-BODY READ may be retried by re-sending
    /// the request.
    ///
    /// **Why this is restricted — do not "improve" it by retrying everything.**
    /// A body-read failure is categorically different from a send failure. It
    /// means the server already RECEIVED, PROCESSED, and ANSWERED the request
    /// and only the answer was lost in transit: the server-side effect has
    /// ALREADY HAPPENED. Re-sending would therefore DOUBLE-APPLY it — a second
    /// room created, a second comment posted, a second SMS sent, a second
    /// credential minted — an intermittent, near-unreproducible corruption far
    /// worse than the lost response it papers over. The send retry is a weaker
    /// case of the same question, handled by
    /// [`Self::should_retry_transport_error`]: a CONNECT failure provably never
    /// reached the server, while a timeout is as ambiguous as a lost body.
    ///
    /// Two independent conditions must BOTH allow the re-send, and either one
    /// alone denies it:
    ///
    /// 1. The endpoint must be replay-safe ([`ReplayPolicy::IfMethodIsSafe`]).
    ///    A GET the server ACTS on passes [`ReplayPolicy::Never`] and is never
    ///    re-sent — `GET` is only a proxy for replay-safety and this API breaks
    ///    the proxy (see [`ApiClient::get_side_effecting`]).
    /// 2. The HTTP method must itself be safe to repeat: `GET` or `HEAD`.
    ///    Everything else — including `PUT` and `DELETE`, which this API drives
    ///    with form bodies as ordinary mutations — is denied. This keys off the
    ///    ACTUAL method of the built request, never the endpoint path or the
    ///    calling helper's name, because this codebase POSTs for many reads and
    ///    DELETEs with a body.
    ///
    /// This gate covers the LOST-BODY door only; the gateway-error door is
    /// [`Self::should_retry_gateway_error`].
    fn should_retry_body_read(
        replay: ReplayPolicy,
        method: &reqwest::Method,
        attempt: u32,
    ) -> bool {
        match replay {
            ReplayPolicy::Never => false,
            ReplayPolicy::IfMethodIsSafe => {
                attempt < MAX_RETRIES
                    && (*method == reqwest::Method::GET || *method == reqwest::Method::HEAD)
            }
        }
    }

    /// Send a request EXACTLY ONCE — no retry loop, no transient-error backoff.
    ///
    /// The single-attempt counterpart to [`Self::send_request_with_retry`], for
    /// non-idempotent single-use endpoints (see [`Self::post_no_auth_once`])
    /// where a re-send after a lost-but-committed response destroys the resource.
    /// A 429 is still surfaced as [`CliError::RateLimit`] and rate-limit response
    /// headers are still parsed, but a network error, timeout, or 502-504 is
    /// returned to the caller as-is rather than retried — because retrying is
    /// exactly the unsafe behavior here.
    async fn send_request_once<F>(&self, build_request: F) -> Result<reqwest::Response, CliError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let resp = build_request().send().await.map_err(CliError::Http)?;
        let status = resp.status();
        tracing::trace!(status = status.as_u16(), "api response (single-attempt)");
        if status.as_u16() == 429 {
            // Body-first, and the account lockout keeps its identity rather than
            // being flattened into a generic rate limit — see
            // [`Self::rate_limit_error`]. This is the API-envelope path (the one
            // sign-in uses), so body promotion is permitted.
            return Err(Self::rate_limit_error(resp, true).await);
        }
        Self::check_rate_limit(&resp);
        Ok(resp)
    }

    /// Core retry/rate-limit send loop shared by the redirect-following and
    /// fail-closed-on-redirect paths.
    ///
    /// When `fail_closed_on_redirect` is `true`, a 3xx response is a TERMINAL
    /// error ([`CliError::Parse`], resource-agnostic, embedding no secret or
    /// URL) rather than something to retry or follow — the caller is on the
    /// no-redirect client precisely so an unexpected redirect cannot forward a
    /// credential header to the `Location` target. The 429 / 502-504 / network
    /// retry semantics are identical in both modes.
    ///
    /// `first_attempt` is where this call enters the shared attempt budget: a
    /// caller that already consumed attempts (see
    /// [`Self::send_and_read_with_retry`], which re-enters after a failed body
    /// read) passes the next unused index so the budget and the backoff
    /// schedule continue rather than restart. Ordinary callers pass `0`.
    async fn send_request_with_retry_inner<F>(
        &self,
        build_request: &F,
        fail_closed_on_redirect: bool,
        first_attempt: u32,
        replay: ReplayPolicy,
        scrub_url: bool,
    ) -> Result<SentResponse, CliError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut last_error: Option<CliError> = None;

        for attempt in first_attempt..=MAX_RETRIES {
            if attempt > 0 {
                let backoff = INITIAL_BACKOFF * 2u32.saturating_pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                    "retrying request after transient failure"
                );
                tokio::time::sleep(backoff).await;
            }

            // Build and split rather than `send()` (which is exactly
            // `build_split()` + `execute()`) so the METHOD is known before the
            // request is consumed: it is what decides, later, whether a failed
            // BODY read may be re-sent (see [`Self::should_retry_body_read`]).
            // A builder error is terminal here exactly as it was when `send()`
            // surfaced it — `is_retryable_error` never matched it.
            let (client, request) = build_request().build_split();
            let request = request.map_err(CliError::Http)?;
            let method = request.method().clone();
            match client.execute(request).await {
                Ok(resp) => {
                    let status = resp.status();
                    tracing::trace!(status = status.as_u16(), "api response");

                    // A gateway error is NOT retried for a side-effecting
                    // endpoint: the upstream may have processed the request and
                    // the gateway lost the response, so re-sending could apply
                    // the effect twice. Falling through hands the 502-504 to the
                    // body handler — exactly what today's LAST attempt already
                    // does once the budget is spent, so the terminal error shape
                    // is unchanged.
                    if matches!(status.as_u16(), 502..=504)
                        && attempt < MAX_RETRIES
                        && Self::should_retry_gateway_error(replay)
                    {
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
                        // Body-first, and the account lockout keeps its identity
                        // — this is the API-envelope retry path (the one sign-in
                        // reaches via `get_with_auth_side_effecting`), so body
                        // promotion is permitted. See `allow_body_promotion` on
                        // [`Self::rate_limit_error`].
                        return Err(Self::rate_limit_error(resp, true).await);
                    }

                    // Fail closed on an unexpected redirect for password-bearing
                    // sends: the no-redirect client never followed it, and we
                    // must NOT chase it ourselves (the `Location` target is
                    // untrusted for a credential header). The error names no
                    // resource and embeds neither the secret nor any URL.
                    if fail_closed_on_redirect && status.is_redirection() {
                        return Err(CliError::Parse(
                            "the server returned an unexpected redirect for a \
                             password-protected request; refusing to follow it"
                                .to_owned(),
                        ));
                    }

                    Self::check_rate_limit(&resp);
                    return Ok(SentResponse {
                        resp,
                        method,
                        attempt,
                    });
                }
                Err(e)
                    if Self::should_retry_transport_error(replay, &e) && attempt < MAX_RETRIES =>
                {
                    // Scrub BEFORE logging, not just before wrapping: a retried
                    // attempt logs too, and reqwest attaches the full URL —
                    // query string included — to a transport error.
                    let e = if scrub_url { e.without_url() } else { e };
                    tracing::warn!(error = %e, "transient network error, will retry");
                    last_error = Some(CliError::Http(e));
                }
                Err(e) => {
                    return Err(CliError::Http(if scrub_url { e.without_url() } else { e }));
                }
            }
        }

        // `last_error` is always `Some` here because the loop body sets it
        // on every retryable failure, but we handle `None` defensively.
        Err(last_error
            .unwrap_or_else(|| CliError::Parse("request failed: all retries exhausted".to_owned())))
    }

    /// Send a password-bearing request on the no-redirect client, with the same
    /// retry / rate-limit semantics as [`Self::send_with_retry`] but failing
    /// closed on any 3xx, then unwrap the API envelope.
    ///
    /// This is the sole send path for the password-bearing ENVELOPE requests
    /// (`get_with_password` / `post_with_password`). The `build_request` closure
    /// MUST build on [`Self::no_redirect_envelope_client`] (those helpers do) so
    /// reqwest never auto-follows a redirect and forwards the credential header
    /// to the `Location` target; an unexpected 3xx becomes a terminal
    /// [`CliError::Parse`] (see [`Self::send_request_with_retry_inner`]).
    async fn send_no_redirect<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let buffered = self
            .send_and_read_with_retry(build_request, true, ReplayPolicy::IfMethodIsSafe, false)
            .await?;
        Self::handle_envelope_body(buffered.status, buffered.body)
    }

    /// Send a request with retry; deserialize the unwrapped envelope payload.
    ///
    /// For endpoints that are pure READS. A GET the server acts on must use
    /// [`Self::send_with_retry_no_replay`] instead.
    async fn send_with_retry<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let buffered = self
            .send_and_read_with_retry(build_request, false, ReplayPolicy::IfMethodIsSafe, false)
            .await?;
        Self::handle_envelope_body(buffered.status, buffered.body)
    }

    /// [`Self::send_with_retry`], but strips the request URL from any transport
    /// error before it is logged or wrapped.
    ///
    /// For requests whose QUERY STRING carries a secret. `reqwest` attaches the
    /// full URL to a transport error, so without this a connection failure
    /// renders the secret to stderr, logs, or an MCP tool result. Same guarantee
    /// the token-bearing download FOLLOW already gets — the envelope
    /// path simply had no equivalent until a secret first appeared in a query
    /// string here.
    async fn send_with_retry_scrubbed<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let buffered = self
            .send_and_read_with_retry(build_request, false, ReplayPolicy::IfMethodIsSafe, true)
            .await?;
        Self::handle_envelope_body(buffered.status, buffered.body)
    }

    /// Send a request with retry; deserialize the unwrapped envelope payload —
    /// under [`ReplayPolicy::Never`], so the request is NOT put on the wire a
    /// second time on a failure the server may already have acted on.
    ///
    /// This governs all three retry doors, not just the body read: a lost body
    /// and a 502-504 are refused outright, and of the transport failures only a
    /// CONNECT error (connection provably never established) is still retried.
    /// For endpoints whose handling has a server-side effect, where a lost
    /// response means the effect already happened (see
    /// [`Self::get_side_effecting`]).
    async fn send_with_retry_no_replay<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let buffered = self
            .send_and_read_with_retry(build_request, false, ReplayPolicy::Never, false)
            .await?;
        Self::handle_envelope_body(buffered.status, buffered.body)
    }

    /// Send a request with retry; deserialize the full JSON body without
    /// envelope unwrapping.
    async fn send_with_retry_raw<T, F>(&self, build_request: F) -> Result<T, CliError>
    where
        T: DeserializeOwned,
        F: Fn() -> reqwest::RequestBuilder,
    {
        let buffered = self
            .send_and_read_with_retry(build_request, false, ReplayPolicy::IfMethodIsSafe, false)
            .await?;
        Self::handle_raw_body(buffered.status, buffered.body)
    }

    /// Send a request with retry; return `(http_status, body)` for both
    /// HTTP 200 and HTTP 404 responses without envelope unwrapping. Used
    /// by `get_partial_envelope`; see that method for the contract.
    async fn send_with_retry_partial<F>(&self, build_request: F) -> Result<(u16, Value), CliError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let buffered = self
            .send_and_read_with_retry(build_request, false, ReplayPolicy::IfMethodIsSafe, false)
            .await?;
        Self::handle_partial_body(buffered.status, buffered.body)
    }

    /// Send a request with retry; return the raw response body as text
    /// (no JSON parse, no envelope unwrap).
    ///
    /// Used by the markdown fetch path: the server emits
    /// `Content-Type: text/markdown; charset=UTF-8` which cannot be parsed as
    /// JSON. Non-success HTTP statuses are surfaced as `CliError::Api` with
    /// the markdown body included as the error message, matching the
    /// behavior of `handle_raw_body` for JSON error responses. (The
    /// server renders error envelopes as markdown when `?output=markdown`
    /// is set, so the message is still human-readable.)
    ///
    /// Unlike the JSON paths this reads the body OUTSIDE the retry loop, so a
    /// failed body read is not re-sent: the text decode is charset-aware
    /// (`Response::text` honors the `Content-Type` charset), which the buffered
    /// path's byte-level handling does not reproduce.
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

    /// Process an API response: read the body, parse JSON, check the envelope,
    /// return the unwrapped payload.
    ///
    /// The body is read HERE, outside any retry loop — this is the handler for
    /// the deliberately SINGLE-ATTEMPT send paths ([`Self::send_request_once`]),
    /// which must never re-send. The retrying paths read the body inside
    /// [`Self::send_and_read_with_retry`] and call
    /// [`Self::handle_envelope_body`] directly.
    async fn handle_response<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, CliError> {
        let status = resp.status();
        let body = resp.bytes().await;
        Self::handle_envelope_body(status, body)
    }

    /// Interpret an already-read API response body: parse JSON, check the
    /// envelope, return the unwrapped payload.
    ///
    /// `body` is `Err` only when the body could not be READ (a transport-level
    /// failure); a body that arrived intact but is not valid JSON arrives as
    /// `Ok` and fails in the parse below. Both surface as [`CliError::Parse`]
    /// with the wording this path has always used.
    fn handle_envelope_body<T: DeserializeOwned>(
        status: reqwest::StatusCode,
        body: Result<Bytes, reqwest::Error>,
    ) -> Result<T, CliError> {
        let raw =
            body.map_err(|e| CliError::Parse(format!("failed to parse API response: {e}")))?;
        let body: Value = serde_json::from_slice(&raw)
            .map_err(|e| CliError::Parse(format!("failed to parse API response: {e}")))?;
        // Redact one-time secrets (subscription create/rotate, realtime token,
        // etc.) BEFORE tracing — the raw body must never reach the log even at
        // `RUST_LOG=trace`. Only build the redacted clone when the trace level
        // is actually enabled so the common (untraced) path stays allocation-free.
        if tracing::enabled!(tracing::Level::TRACE) {
            let redacted = redact_secret_values_for_log(&body);
            tracing::trace!(body = %redacted, "api response body");
        }

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

    /// Interpret an already-read response body without envelope unwrapping:
    /// deserialize the full JSON body directly into `T`.
    ///
    /// See [`Self::handle_envelope_body`] for what an `Err` body means.
    fn handle_raw_body<T: DeserializeOwned>(
        status: reqwest::StatusCode,
        body: Result<Bytes, reqwest::Error>,
    ) -> Result<T, CliError> {
        if !status.is_success() {
            // An unreadable or non-JSON error body still yields the
            // status-derived error rather than collapsing into a parse error.
            let body: Value = body
                .as_ref()
                .ok()
                .and_then(|raw| serde_json::from_slice(raw).ok())
                .unwrap_or_default();
            if tracing::enabled!(tracing::Level::TRACE) {
                tracing::trace!(
                    body = %redact_secret_values_for_log(&body),
                    "api error response body (raw)"
                );
            }
            return Err(Self::extract_error(&body, status.as_u16()).into());
        }

        let raw =
            body.map_err(|e| CliError::Parse(format!("failed to read response body: {e}")))?;
        // The raw path serves non-envelope endpoints — notably the OAuth token
        // exchange/refresh, whose success body carries `access_token` /
        // `refresh_token`. Redact before tracing so `RUST_LOG=trace` cannot leak
        // them (only render for redaction when trace is actually enabled).
        if tracing::enabled!(tracing::Level::TRACE) {
            tracing::trace!(
                body = %redact_text_body_for_log(&String::from_utf8_lossy(&raw)),
                "api response body (raw)"
            );
        }

        serde_json::from_slice(&raw)
            .map_err(|e| CliError::Parse(format!("failed to deserialize response: {e}")))
    }

    /// Interpret an already-read response body for the bulk-details
    /// partial-envelope contract.
    ///
    /// Returns `(http_status, body)` for both HTTP 200 and HTTP 404 so the
    /// caller can distinguish full success from the all-errored case (which
    /// the server signals as 404 + `result: "no"`). Other statuses produce
    /// a structured error. See [`Self::handle_envelope_body`] for what an `Err`
    /// body means.
    fn handle_partial_body(
        status: reqwest::StatusCode,
        body: Result<Bytes, reqwest::Error>,
    ) -> Result<(u16, Value), CliError> {
        let parse_error = |detail: &dyn std::fmt::Display| {
            CliError::Parse(format!(
                "failed to parse {} response body: {detail}",
                status.as_u16()
            ))
        };

        // Non-200/404 responses may legitimately have a non-JSON body
        // (proxy HTML error pages, empty 401s); fall back to an empty
        // Value so `extract_error` can still return its default message
        // rather than collapsing to a parse error.
        let body: Value = if matches!(status.as_u16(), 200 | 404) {
            let raw = body.map_err(|e| parse_error(&e))?;
            serde_json::from_slice(&raw).map_err(|e| parse_error(&e))?
        } else {
            body.as_ref()
                .ok()
                .and_then(|raw| serde_json::from_slice(raw).ok())
                .unwrap_or_default()
        };
        if tracing::enabled!(tracing::Level::TRACE) {
            tracing::trace!(
                body = %redact_secret_values_for_log(&body),
                "api response body (partial envelope)"
            );
        }

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
            // as a successful empty result.
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
            // The live framework returns string-encoded codes for some errors
            // (`"code": "400"` / `"405"`) while richer handler errors are
            // numeric. Accept either: a JSON number OR a string that parses as a
            // u64. An unparseable string falls back to 0 (the HTTP status still
            // drives the suggestion) — never a panic. Server codes may exceed
            // u32; emit a trace warning when narrowing collapses a non-zero code
            // to 0 so support can correlate to the raw body if needed (parity
            // with the `error_id` branch below).
            // NOTE the `trim()`. Without it a whitespace-padded string code
            // (`"code": " 10175 "`) failed to parse and collapsed to 0 —
            // defeating BOTH `10175` protections at once: the dedicated hint
            // never fired (so the user got the harmful "run `fastio auth login`"
            // advice) and `dead_session_401` no longer saw an excluded code, so
            // `signout` treated it as a dead session and could WIPE STORED
            // CREDENTIALS. A hostile-wire case; cheap to close, and it keeps
            // this parse consistent with `error::json_u64`, which has always
            // trimmed.
            let raw_code = err.get("code").and_then(|c| {
                c.as_u64()
                    .or_else(|| c.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
            });
            let code = raw_code
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or_else(|| {
                    if let Some(n) = raw_code {
                        tracing::warn!(
                            error_code = n,
                            http_status,
                            "API error code exceeds u32; truncating ApiError.code to 0"
                        );
                    }
                    0
                });
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
                code,
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
            // Trimmed for the same reason as the nested `code` branch above: a
            // whitespace-padded string collapses to 0 and silently disables every
            // code-keyed protection downstream.
            let raw_id = body.get("error_id").and_then(crate::error::json_u64);
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
        // Allow-list: anything not named here is DROPPED. A consumer that reads
        // a field off `details` is therefore only reachable if the field is in
        // this list — adding the read without adding the key yields a branch
        // that silently never fires.
        const DETAIL_KEYS: &[&str] = &[
            "params",
            "validation_report",
            "reason",
            "documentation_url",
            "resource",
            // NOTE: no conflict/contention keys here, deliberately.
            //
            // `contested` was cut 2026-08-24, and `rebase_count` /
            // `contested_since` are not part of the contract — so allow-listing
            // them would preserve fields the server does not send, and a reader
            // added later would look supported.
            //
            // The CAS conflict enrichment needs nothing extra: the server puts
            // `reason` and `current_version_id` INSIDE `params`, which is
            // already listed above, so the whole object survives intact.
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

    /// Resolve how long to wait after a 429, **preferring the response BODY over
    /// the rate-limit headers**, and consuming the response to do it.
    ///
    /// [`Self::parse_rate_limit_expiry`] reads `x-ve-limit-expires` and falls
    /// back to a hardcoded 60 seconds when it is absent. That is wrong for the
    /// per-account failed-login lockout, and the platform says so explicitly.
    /// The origin's own comment above the lockout emission reads:
    ///
    /// > Advertise the wait through the standard rate-limit headers **as a
    /// > convenience**. The **authoritative** machine-readable value is
    /// > `error.params.retry_after_seconds` in the body — the edge's
    /// > response-header filter is not guaranteed to pass these through.
    ///
    /// **Measured 2026-08-23**, after exhausting a throwaway account's
    /// five attempts: the lockout returns HTTP 429 / code `10760` with
    /// `error.params.retry_after_seconds: 1790` and **no** `x-ve-limit-expires`
    /// header — so the header path fell to its 60-second default and the CLI
    /// told the user *"Retry in 60 seconds"* for a **30-minute** lockout. A user
    /// following that advice is refused four more times before the wait is
    /// actually over.
    ///
    /// Order: body value → header value → the 60-second default.
    /// # Why the read is BOUNDED
    ///
    /// `resp.text().await` buffers the entire body, which is a hang: the
    /// streaming clients
    /// ([`Self::build_streaming_client`] /
    /// [`Self::build_no_redirect_streaming_client`]) are deliberately built with
    /// a CONNECT timeout only and no overall body timeout, so a 429 using
    /// chunked transfer-encoding that dribbles a byte occasionally and never
    /// terminates would block a download **forever**. A fast multi-gigabyte body
    /// is the same class.
    ///
    /// A **decompression bomb is not in play**, and the cap does not bound one:
    /// this build enables none of reqwest's `gzip`/`brotli`/`deflate`/`zstd`
    /// features, so nothing is ever decompressed (see [`ERROR_BODY_CAP`]). The
    /// threat is absent rather than mitigated — stated explicitly because a
    /// plausible-sounding mitigation claim is the kind that survives review
    /// unchallenged; disproving it means reading `Cargo.toml`.
    ///
    /// Both a **byte cap** and a **deadline** are required:
    /// `Content-Length` alone is not sufficient, because a chunked or simply
    /// dishonest response never has to declare one.
    ///
    /// Over the cap, past the deadline, unreadable, or not JSON ⇒ fall back to
    /// the header/60s path. The wait value is a convenience; it is never worth
    /// stalling a download for.
    /// # Why this returns an ERROR and not just an integer
    ///
    /// An account lockout and an API rate limit are different conditions with
    /// different remedies, and collapsing both into [`CliError::RateLimit`] told
    /// a locked-out user *"API rate limit exceeded"* while discarding the
    /// server's own clear sentence (*"Too many failed sign-in attempts. Try
    /// again in 30 minutes."*), the code `10760`, and the params. That identity
    /// loss is visible on the MCP surface too, where the generic error path
    /// renders only `to_string()`.
    ///
    /// So the lockout — and ONLY the lockout — is surfaced as a
    /// [`CliError::Api`], which keeps its code, message, params and
    /// [`ApiError::lockout_note`]. **Every other 429 is unchanged** and still
    /// becomes `RateLimit { retry_after_secs }`, because poll loops depend on
    /// that variant to decide how long to sleep and this must not disturb them.
    ///
    /// # `allow_body_promotion` — a CREDENTIAL LEAK this closes
    ///
    /// Promoting a body into an `ApiError` renders that body's text to the user,
    /// so it may only happen for responses from an endpoint we trust. The
    /// **preview follow** is not one: it is a redirect to a CDN reached through a
    /// URL that embeds a short-lived `download_token`, and a CDN error page can
    /// reflect the request URL back in its body. `download_preview_...` already
    /// refuses to mine that body for exactly this reason — but the 429 intercept
    /// runs FIRST, so an unconditional promotion reached it before the
    /// suppression could.
    ///
    /// Concrete attack: a CDN answers the tokenized follow with
    ///
    /// ```text
    /// HTTP/1.1 429
    /// {"error":{"code":10760,"text":"failed URL https://cdn/obj?download_token=SECRET"}}
    /// ```
    ///
    /// and the reflected token is printed. Before this change the body was
    /// discarded, because every 429 became `RateLimit`.
    ///
    /// The streaming paths therefore pass `false` and can never promote. That
    /// costs nothing: `10760` is emitted only by `GET /user/auth/`, which is not
    /// a streaming download, so a streaming response has no lockout identity to
    /// preserve and a credential to lose.
    async fn rate_limit_error(resp: reqwest::Response, allow_body_promotion: bool) -> CliError {
        // Read the header BEFORE consuming the response for its body.
        let header_secs = Self::parse_rate_limit_expiry(&resp);
        let fallback = |secs: u64| {
            Self::emit_rate_limit_error(secs);
            CliError::RateLimit {
                retry_after_secs: secs,
            }
        };
        let read = tokio::time::timeout(
            ERROR_BODY_DEADLINE,
            Self::read_body_capped(resp, ERROR_BODY_CAP),
        );
        // `Err` = deadline elapsed; `Ok(None)` = exceeded the cap or a transport
        // error mid-body. Both fall back rather than guess.
        let Ok(Some(body)) = read.await else {
            return fallback(header_secs);
        };
        let Ok(json) = serde_json::from_slice::<Value>(&body) else {
            return fallback(header_secs);
        };
        // The account lockout keeps its identity — but ONLY from a trusted
        // endpoint. See `allow_body_promotion` above: an untrusted body may
        // reflect a `download_token`, and promoting it renders it.
        if allow_body_promotion {
            let api = Self::extract_error(&json, 429);
            if api.code == crate::error::ERR_LOGIN_LOCKED {
                return CliError::Api(api);
            }
        }
        let secs = json
            .get("error")
            .and_then(|e| e.get("params"))
            .and_then(|p| p.get("retry_after_seconds"))
            // SHARED coercion with `ApiError::param_u64` — see `error::json_u64`.
            // Previously this had its own weaker parse that failed OPEN to the
            // 60s default where `param_u64` failed CLOSED; an array-shaped or
            // unparseable wait therefore reinstated the very
            // "60 seconds for a 30-minute lockout" bug this function exists to
            // fix.
            .and_then(crate::error::json_u64)
            .unwrap_or(header_secs);
        fallback(secs)
    }

    /// Read at most `cap` bytes of a response body, or give up.
    ///
    /// Returns `None` if the body exceeds `cap` or the transport fails
    /// mid-stream — deliberately NOT a truncated buffer, since a partial JSON
    /// document cannot be parsed and a partial read must never be mistaken for
    /// a complete one. Chunk-wise so an oversized body is abandoned as soon as
    /// it crosses the cap rather than after it has all been allocated.
    async fn read_body_capped(mut resp: reqwest::Response, cap: usize) -> Option<Vec<u8>> {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if buf.len().saturating_add(chunk.len()) > cap {
                        return None;
                    }
                    buf.extend_from_slice(&chunk);
                }
                Ok(None) => return Some(buf),
                Err(_) => return None,
            }
        }
    }

    /// Emit a clear rate-limit error to stderr.
    ///
    /// Waits of a minute or more are rendered in minutes: this same line now
    /// carries the **account-lockout** wait, which is 30 minutes, and
    /// *"Retry in 1790 seconds"* is a number the reader has to convert before it
    /// means anything.
    fn emit_rate_limit_error(retry_secs: u64) {
        // Round UP, matching `ApiError::lockout_note`. The first version mapped
        // the whole 60..=119 range to "about 1 minute", so a 119-second wait
        // advertised one minute — rounding DOWN, and contradicting the
        // round-up rule stated one function away. Advising a retry slightly too
        // late is harmless, too early is another refused request.
        let wait = match retry_secs {
            0..=59 => format!("{retry_secs} seconds"),
            60..=60 => "about 1 minute".to_owned(),
            secs => format!("about {} minutes", secs.div_ceil(60)),
        };
        eprintln!(
            "{} API rate limit exceeded. Retry in {}.",
            "error:".red().bold(),
            wait
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
    fn redact_secret_values_masks_credential_keys_and_keeps_structure() {
        let body = serde_json::json!({
            "result": "yes",
            "response": {
                "id": "sub_123",
                "token": "tok_LIVE_should_never_log",
                "secret": "shh",
                "auth_token": "bearer_xyz",
                "nested": {
                    "access_token": "at_abc",
                    "description": "keep me",
                },
                "items": [
                    {"api_key": "k_1", "label": "first"},
                    {"refresh_token": "r_1", "label": "second"},
                ],
                // Preview-access JWT (camelCase, must match case-insensitively)
                // and invitation bearer capability.
                "downloadToken": "dl_jwt_should_hide",
                "invitation_key": "inv_key_should_hide",
            },
        });
        let redacted = redact_secret_values_for_log(&body);
        let rendered = redacted.to_string();

        // Every secret value is masked, regardless of nesting / arrays.
        assert!(
            !rendered.contains("tok_LIVE_should_never_log"),
            "top-level token leaked: {rendered}"
        );
        assert!(!rendered.contains("shh"), "secret leaked: {rendered}");
        assert!(!rendered.contains("bearer_xyz"), "auth_token leaked");
        assert!(!rendered.contains("at_abc"), "nested access_token leaked");
        assert!(!rendered.contains("k_1"), "array api_key leaked");
        assert!(!rendered.contains("r_1"), "array refresh_token leaked");
        assert!(
            !rendered.contains("dl_jwt_should_hide"),
            "downloadToken (preview JWT) leaked: {rendered}"
        );
        assert!(
            !rendered.contains("inv_key_should_hide"),
            "invitation_key leaked: {rendered}"
        );

        // The placeholder is present and non-secret fields are untouched.
        assert_eq!(redacted["response"]["token"], REDACTED_PLACEHOLDER);
        assert_eq!(redacted["response"]["secret"], REDACTED_PLACEHOLDER);
        assert_eq!(redacted["response"]["auth_token"], REDACTED_PLACEHOLDER);
        assert_eq!(
            redacted["response"]["nested"]["access_token"],
            REDACTED_PLACEHOLDER
        );
        assert_eq!(
            redacted["response"]["downloadToken"], REDACTED_PLACEHOLDER,
            "downloadToken must redact case-insensitively"
        );
        assert_eq!(redacted["response"]["invitation_key"], REDACTED_PLACEHOLDER);
        assert_eq!(redacted["result"], "yes");
        assert_eq!(redacted["response"]["id"], "sub_123");
        assert_eq!(redacted["response"]["nested"]["description"], "keep me");
        assert_eq!(redacted["response"]["items"][0]["label"], "first");
        assert_eq!(redacted["response"]["items"][1]["label"], "second");
        // Case-insensitive: the original (unredacted) body is unchanged.
        assert_eq!(body["response"]["token"], "tok_LIVE_should_never_log");
    }

    #[test]
    fn redact_secret_values_masks_billing_url_and_key_fields() {
        // Billing responses nest a one-time client_secret, a publishable
        // public_key, and access-granting invoice URLs (see the published API docs);
        // all three new keys must be masked, top-level and nested.
        let body = serde_json::json!({
            "response": {
                "setup_intent": {"client_secret": "seti_secret_LIVE"},
                "public_key": "pk_live_should_hide",
                "hosted_invoice_url": "https://pay.example/i/secret",
                "invoice_pdf": "https://pay.example/invoice/secret.pdf",
                "id": "in_keep_me",
            },
        });
        let redacted = redact_secret_values_for_log(&body);
        let rendered = redacted.to_string();
        assert!(
            !rendered.contains("seti_secret_LIVE"),
            "client_secret leaked: {rendered}"
        );
        assert!(
            !rendered.contains("pk_live_should_hide"),
            "public_key leaked: {rendered}"
        );
        assert!(
            !rendered.contains("pay.example/i/secret"),
            "hosted_invoice_url leaked: {rendered}"
        );
        assert!(
            !rendered.contains("pay.example/invoice/secret.pdf"),
            "invoice_pdf leaked: {rendered}"
        );
        assert_eq!(
            redacted["response"]["setup_intent"]["client_secret"],
            REDACTED_PLACEHOLDER
        );
        assert_eq!(redacted["response"]["public_key"], REDACTED_PLACEHOLDER);
        assert_eq!(
            redacted["response"]["hosted_invoice_url"],
            REDACTED_PLACEHOLDER
        );
        assert_eq!(redacted["response"]["invoice_pdf"], REDACTED_PLACEHOLDER);
        // Non-secret sibling preserved.
        assert_eq!(redacted["response"]["id"], "in_keep_me");
    }

    #[test]
    fn redact_form_masks_secret_valued_fields() {
        let mut form = HashMap::new();
        form.insert("grant_type".to_owned(), "refresh_token".to_owned());
        form.insert("refresh_token".to_owned(), "rt_LIVE_secret".to_owned());
        form.insert("client_id".to_owned(), "cid_123".to_owned());
        form.insert("client_secret".to_owned(), "cs_should_hide".to_owned());
        let redacted = redact_form_for_log(&form);
        assert_eq!(redacted.get("refresh_token"), Some(&REDACTED_PLACEHOLDER));
        // `client_secret` is a secret key and is masked; `client_id` is not.
        assert_eq!(redacted.get("client_secret"), Some(&REDACTED_PLACEHOLDER));
        assert_eq!(redacted.get("client_id"), Some(&"cid_123"));
        // `grant_type` carries the literal string "refresh_token" as a VALUE but
        // its KEY is not secret, so the value is preserved (we redact by key).
        assert_eq!(redacted.get("grant_type"), Some(&"refresh_token"));
    }

    #[test]
    fn redact_form_masks_email_token_and_client_info() {
        // `email_token` (email-verification one-time code) and `client_info`
        // (file-lock device fingerprint) are sensitive form fields and must be
        // masked by key name; a non-secret sibling (`email`) is preserved.
        let mut form = HashMap::new();
        form.insert("email".to_owned(), "user@example.test".to_owned());
        form.insert("email_token".to_owned(), "evt_LIVE_secret".to_owned());
        form.insert(
            "client_info".to_owned(),
            r#"{"device":"laptop"}"#.to_owned(),
        );
        let redacted = redact_form_for_log(&form);
        assert_eq!(redacted.get("email_token"), Some(&REDACTED_PLACEHOLDER));
        assert_eq!(redacted.get("client_info"), Some(&REDACTED_PLACEHOLDER));
        assert_eq!(redacted.get("email"), Some(&"user@example.test"));
    }

    #[test]
    fn redact_params_masks_invitation_key_query_value() {
        // GET query maps are redacted via the same `redact_form_for_log` path
        // (Fix 2): a sensitive query value such as `invitation_key` must not
        // appear cleartext in a trace, while a non-secret param is preserved.
        let mut params = HashMap::new();
        params.insert("invitation_key".to_owned(), "ik_LIVE_secret".to_owned());
        params.insert("page".to_owned(), "1".to_owned());
        let redacted = redact_form_for_log(&params);
        assert_eq!(redacted.get("invitation_key"), Some(&REDACTED_PLACEHOLDER));
        assert_eq!(redacted.get("page"), Some(&"1"));
    }

    #[test]
    fn redact_path_masks_password_reset_and_2factor_secrets() {
        // Password-reset code in the path (both the complete and the
        // `/details/` check variant).
        assert_eq!(
            redact_path_for_log("/user/password/RESETCODE/"),
            "/user/password/[redacted]/"
        );
        assert_eq!(
            redact_path_for_log("/user/password/RESETCODE/details/"),
            "/user/password/[redacted]/details/"
        );
        // 2FA post-sign-in code and TOTP-setup token.
        assert_eq!(
            redact_path_for_log("/user/auth/2factor/auth/123456/"),
            "/user/auth/2factor/auth/[redacted]/"
        );
        assert_eq!(
            redact_path_for_log("/user/auth/2factor/verify/SETUPTOK/"),
            "/user/auth/2factor/verify/[redacted]/"
        );
        // 2FA disable token (DELETE) is masked...
        assert_eq!(
            redact_path_for_log("/user/auth/2factor/987654/"),
            "/user/auth/2factor/[redacted]/"
        );
        // ...but the non-secret enable channel at the same position is preserved.
        assert_eq!(
            redact_path_for_log("/user/auth/2factor/totp/"),
            "/user/auth/2factor/totp/"
        );
        // The `send` channel route (one segment deeper) is left intact.
        assert_eq!(
            redact_path_for_log("/user/auth/2factor/send/sms/"),
            "/user/auth/2factor/send/sms/"
        );
        // Ordinary paths whose ids are NOT secrets are never altered.
        assert_eq!(
            redact_path_for_log("/workspace/123/storage/abc/lock/"),
            "/workspace/123/storage/abc/lock/"
        );
        assert_eq!(
            redact_path_for_log("/user/auth/2factor/"),
            "/user/auth/2factor/"
        );
    }

    #[test]
    fn redact_path_masks_room_invite_redeem_token() {
        // The room-invite redeem token is a one-time bearer capability embedded
        // in the PATH (`/room/invites/{token}/redeem`); mask it while keeping the
        // route structure visible.
        assert_eq!(
            redact_path_for_log("/room/invites/tok_LIVE_secret/redeem"),
            "/room/invites/[redacted]/redeem"
        );
        // A URL-encoded token segment is still a single segment and is masked.
        assert_eq!(
            redact_path_for_log("/room/invites/abc%2Fdef/redeem"),
            "/room/invites/[redacted]/redeem"
        );
        // The invites LIST/create route (`/share/{id}/room/invites/`) carries no
        // token in the path and must be left intact.
        assert_eq!(
            redact_path_for_log("/share/123/room/invites/"),
            "/share/123/room/invites/"
        );
    }

    #[test]
    fn redact_body_masks_invite_url_capability() {
        // `invite_url` is the one-time capability URL returned by create_invite;
        // it must never appear cleartext in a traced response body, while
        // non-secret siblings (expires_at, suggested_label) are preserved.
        let body = serde_json::json!({
            "invite_url": "https://api.fast.io/v1.0/room/invites/tok_LIVE_secret/redeem",
            "expires_at": "2026-07-23 12:00:00 UTC",
            "suggested_label": "build-agent"
        });
        let redacted = redact_secret_values_for_log(&body);
        assert_eq!(redacted["invite_url"], serde_json::json!("[redacted]"));
        assert!(
            !redacted.to_string().contains("tok_LIVE_secret"),
            "invite_url token leaked: {redacted}"
        );
        assert_eq!(
            redacted["expires_at"],
            serde_json::json!("2026-07-23 12:00:00 UTC")
        );
        assert_eq!(
            redacted["suggested_label"],
            serde_json::json!("build-agent")
        );
    }

    #[test]
    fn redact_text_body_masks_oauth_tokens_and_passes_non_json() {
        let oauth = r#"{"access_token":"at_live","refresh_token":"rt_live","token_type":"Bearer"}"#;
        let redacted = redact_text_body_for_log(oauth);
        assert!(
            !redacted.contains("at_live"),
            "access_token leaked: {redacted}"
        );
        assert!(
            !redacted.contains("rt_live"),
            "refresh_token leaked: {redacted}"
        );
        assert!(redacted.contains("[redacted]"));
        // token_type is not a secret key.
        assert!(redacted.contains("Bearer"));
        // Non-JSON text (proxy HTML) cannot carry a structured secret field;
        // returned as-is.
        let html = "<html>Bad Gateway</html>";
        assert_eq!(redact_text_body_for_log(html), html);
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
    fn extract_error_accepts_string_encoded_code() {
        // Live framework validation errors carry STRING codes (`"code": "400"`
        // / `"405"`) while richer handler errors are numeric. Both must parse.
        let body = serde_json::json!({
            "result": false,
            "error": {"code": "400", "text": "bad request"},
        });
        let err = ApiClient::extract_error(&body, 400);
        assert_eq!(err.code, 400, "string-encoded code must parse to 400");
        assert_eq!(err.http_status, 400);

        let m405 = serde_json::json!({"error": {"code": "405", "text": "method"}});
        assert_eq!(ApiClient::extract_error(&m405, 405).code, 405);

        // A numeric code still parses unchanged.
        let numeric = serde_json::json!({"error": {"code": 9992, "text": "no route"}});
        assert_eq!(ApiClient::extract_error(&numeric, 404).code, 9992);

        // An unparseable string code falls back to 0 (no panic); the HTTP
        // status still drives the suggestion.
        let junk = serde_json::json!({"error": {"code": "not-a-number", "text": "x"}});
        let err = ApiClient::extract_error(&junk, 500);
        assert_eq!(err.code, 0);
        assert_eq!(err.http_status, 500);
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
                "resource": "decision:123",
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
            Some("decision:123"),
        );
    }

    /// The CAS conflict enrichment survives extraction — via `params`, which is
    /// allow-listed, NOT via per-field entries.
    ///
    /// `extract_error_details` is an allow-list, so a consumer reading a field
    /// off `details` is unreachable unless its key is listed. `reason` and
    /// `current_version_id` ride INSIDE `params`, so listing `params` carries
    /// them whole — which is why no contention-specific keys are needed.
    #[test]
    fn extract_error_details_preserves_conflict_params() {
        let body = serde_json::json!({
            "result": "no",
            "error": {
                "code": 113_958,
                "text": "version conflict",
                "params": {
                    "reason": "conflict_version_mismatch",
                    "current_version_id": "v3",
                },
            },
        });
        let err = ApiClient::extract_error(&body, 409);
        let details = err.details.as_deref().expect("params extracted");
        let params = details.get("params").expect("params survives whole");
        assert_eq!(
            params.get("reason").and_then(Value::as_str),
            Some("conflict_version_mismatch")
        );
        assert_eq!(
            params.get("current_version_id").and_then(Value::as_str),
            Some("v3")
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
        // The extracted-text routes take `?output=` for chunk verbosity, and
        // `files content` documents that the global `--detail` reaches them.
        assert!(ApiClient::output_injectable(
            "/workspace/1/storage/abc/content/"
        ));
        assert!(ApiClient::output_injectable(
            "/workspace/1/storage/content/"
        ));
    }

    #[test]
    fn output_injectable_denies_download_content_oauth() {
        assert!(!ApiClient::output_injectable("/storage/abc/read/"));
        assert!(!ApiClient::output_injectable("/storage/abc/download/"));
        assert!(!ApiClient::output_injectable("/oauth/token/"));
        assert!(!ApiClient::output_injectable("/user/u/assets/a/read/"));
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
    fn build_get_injects_output_on_metadata_paths() {
        // The metadata family accepts the documented terse/standard/full
        // tokens (ai.txt "Compact Responses"), so `--detail` must thread
        // through to its envelope GETs — list/detail/eligible/details.
        let client = ApiClient::with_detail(
            "https://api.example/current",
            Some("tok".to_owned()),
            Some(OutputDetail::Terse),
        )
        .expect("client builds");
        for path in [
            "/workspace/ws/metadata/eligible/",
            "/workspace/ws/metadata/templates/tid/nodes/",
            "/workspace/ws/storage/abc,def/metadata/details/",
        ] {
            let req = client.build_get(path).build().expect("request builds");
            let query = req.url().query().unwrap_or_default();
            assert!(
                query.contains("output=terse"),
                "metadata path {path} must be --detail-injectable, got query: {query}"
            );
        }
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

    /// Like [`spawn_one_shot_server`] but emits ONE extra response header.
    ///
    /// Exists so a test can make the rate-limit header and the body DISAGREE —
    /// the only way to actually pin body-over-header precedence rather than
    /// body-over-absent-header.
    async fn spawn_one_shot_server_with_header(
        status_line: &'static str,
        content_type: &'static str,
        body: &'static [u8],
        extra_header: &'static str,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let header = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\n\
                     {extra_header}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
    async fn account_lockout_keeps_its_identity_instead_of_becoming_a_rate_limit() {
        // The account lockout is NOT an API rate limit, and flattening it into
        // `CliError::RateLimit` told a locked-out user "API rate limit exceeded"
        // while discarding code 10760, the server's own sentence, and the params.
        // It stays an `ApiError`,
        // so the code, message, `lockout_note()` and the dedicated hint all
        // survive — including on the MCP surface, which renders `to_string()`.
        let body = br#"{"result":false,"error":{"code":10760,"text":"Too many failed sign-in attempts. Try again in 30 minutes.","params":{"retry_after_seconds":1790}}}"#;
        let addr = spawn_one_shot_server("429 Too Many Requests", "application/json", body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = client
            .get::<serde_json::Value>("/user/auth/")
            .await
            .expect_err("429 must error");
        match err {
            CliError::Api(api) => {
                assert_eq!(api.code, crate::error::ERR_LOGIN_LOCKED);
                assert!(
                    api.message.contains("Too many failed sign-in attempts"),
                    "the server's own message must survive: {}",
                    api.message
                );
                assert_eq!(
                    api.lockout_note().as_deref(),
                    Some("Account temporarily locked. Try again in about 30 minutes."),
                    "body-first wait, interpreted"
                );
                assert_eq!(
                    api.suggestion(),
                    Some(crate::error::HINT_LOGIN_LOCKED),
                    "and the lockout hint must be reachable on the real path"
                );
            }
            other => panic!("lockout must stay an ApiError, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn ordinary_429_still_becomes_rate_limit_and_prefers_the_body() {
        // Everything that is NOT the account lockout keeps the old variant —
        // poll loops depend on `RateLimit { retry_after_secs }` to decide how
        // long to sleep, and this change must not disturb them. The body value
        // still wins over the header, which is the precedence the platform
        // documents (headers a convenience, body authoritative).
        let body = br#"{"result":false,"error":{"code":10368,"text":"slow down","params":{"retry_after_seconds":150}}}"#;
        let addr = spawn_one_shot_server_with_header(
            "429 Too Many Requests",
            "application/json",
            body,
            // Far-future epoch: the header path would yield a wildly different
            // value, so this pins body-over-PRESENT-header, not merely
            // body-over-absent-header (the earlier test name overclaimed that).
            "x-ve-limit-expires: 99999999999",
        )
        .await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = client
            .get::<serde_json::Value>("/some/path/")
            .await
            .expect_err("429 must error");
        match err {
            CliError::RateLimit { retry_after_secs } => assert_eq!(
                retry_after_secs, 150,
                "the BODY is authoritative; a present header must not win"
            ),
            other => panic!("a non-lockout 429 must stay RateLimit: {other:?}"),
        }
    }

    #[tokio::test]
    async fn untrusted_429_body_is_never_promoted_into_an_error() {
        // Guards a CREDENTIAL LEAK. The preview follow is a redirect to a CDN
        // reached through a URL embedding a `download_token`, and a CDN error
        // page can reflect that URL in its body. `download_preview_...` refuses
        // to mine that body for exactly this reason — but the 429 intercept runs
        // FIRST, so promoting a `10760` body into an `ApiError` would render the
        // reflected token before the suppression was reached.
        let leaky = br#"{"error":{"code":10760,"text":"failed URL https://cdn.example/obj?download_token=SUPERSECRET"}}"#;
        let addr = spawn_one_shot_server("429 Too Many Requests", "application/json", leaky).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        // Drive a path that goes through `send_streaming_with_retry` — the same
        // send the tokenized preview FOLLOW uses (`scrub_url=true`, client.rs
        // ~1862). Streaming sends pass `allow_body_promotion: false`
        // unconditionally, so no streaming response can promote regardless of
        // which one it is.
        let dir = std::env::temp_dir().join(format!("fastio-leak-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let out = dir.join("x.bin");
        let err = client
            .download_file_stream_with_password("/preview/thing/", &out, None)
            .await
            .expect_err("429 must error");

        let rendered = err.to_string();
        assert!(
            !rendered.contains("SUPERSECRET"),
            "an untrusted 429 body must never reach the user: {rendered}"
        );
        assert!(
            matches!(err, CliError::RateLimit { .. }),
            "streaming 429 must stay RateLimit, got: {err:?}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn rate_limit_wait_refuses_an_oversized_body() {
        // Guards a HANG. An unbounded `resp.text()` in `rate_limit_wait_secs`,
        // combined with streaming clients that have NO overall body timeout,
        // lets an endless or enormous 429 body block a download forever.
        //
        // A body past the cap must be ABANDONED and the wait must fall back,
        // rather than being buffered, truncated, or parsed. Here the oversized
        // body even contains a valid `retry_after_seconds`, so a still-unbounded
        // implementation would return 1790 and this test would fail — which is
        // exactly the mutation signal wanted.
        let mut oversized =
            br#"{"result":false,"error":{"code":10760,"params":{"retry_after_seconds":1790}},"pad":""#
                .to_vec();
        oversized.resize(oversized.len() + ERROR_BODY_CAP + 1024, b'x');
        oversized.extend_from_slice(br#""}"#);
        let body: &'static [u8] = Box::leak(oversized.into_boxed_slice());

        let addr = spawn_one_shot_server("429 Too Many Requests", "application/json", body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = client
            .get::<serde_json::Value>("/user/auth/")
            .await
            .expect_err("429 must error");
        match err {
            CliError::RateLimit { retry_after_secs } => assert_eq!(
                retry_after_secs, 60,
                "an oversized body must be abandoned, not read for its wait value"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn error_code_is_trimmed_so_scope_protections_still_apply() {
        // Hostile-wire case: a whitespace-padded
        // string code (`" 10175 "`) previously failed to parse and collapsed to
        // 0, defeating BOTH `10175` protections at once — the dedicated hint
        // never fired, and `dead_session_401` stopped excluding it, so `signout`
        // could WIPE STORED CREDENTIALS for a credential that was merely
        // out-of-scope.
        let body = br#"{"result":"no","error":{"code":" 10175 ","text":"scope incorrect"}}"#;
        let parsed: serde_json::Value = serde_json::from_slice(body).expect("valid json");
        let err = ApiClient::extract_error(&parsed, 401);
        assert_eq!(
            err.code, 10175,
            "a padded string code must parse, not collapse to 0"
        );
        assert_eq!(
            err.suggestion(),
            Some(crate::error::HINT_SCOPE_INCORRECT),
            "and must still select the scope hint"
        );
    }

    #[tokio::test]
    async fn rate_limit_wait_falls_back_when_the_body_carries_no_value() {
        // A 429 with no `retry_after_seconds` (an ordinary rate limit, not the
        // account lockout) must still yield the previous header/default
        // behaviour rather than 0 or a guess — fail back, not closed.
        let body = br#"{"result":false,"error":{"code":429,"text":"slow down"}}"#;
        let addr = spawn_one_shot_server("429 Too Many Requests", "application/json", body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = client
            .get::<serde_json::Value>("/some/path/")
            .await
            .expect_err("429 must error");
        match err {
            CliError::RateLimit { retry_after_secs } => {
                assert_eq!(retry_after_secs, 60, "header-absent default preserved");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn download_file_stream_bounds_an_oversized_error_body() {
        // Pre-existing hang, adjacent to the one the gate found in the 429 path.
        // `streaming_client` has a CONNECT timeout only, so the old
        // `resp.json()` on a non-2xx buffered without limit — a signing download
        // could hang forever on a body that never terminates.
        //
        // Over the cap, the body is abandoned and the error degrades to the
        // generic status message rather than being buffered whole. The oversized
        // body here contains a REAL error code, so an unbounded implementation
        // would surface 143705 and fail this test.
        let mut oversized =
            br#"{"result":"no","error":{"code":143705,"text":"gone"},"pad":""#.to_vec();
        oversized.resize(oversized.len() + ERROR_BODY_CAP + 1024, b'x');
        oversized.extend_from_slice(br#""}"#);
        let body: &'static [u8] = Box::leak(oversized.into_boxed_slice());

        let addr = spawn_one_shot_server("404 Not Found", "application/json", body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let dir = std::env::temp_dir().join(format!("fastio-dl-cap-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let out = dir.join("x.bin");

        let err = client
            .download_file_stream("/thing/", &out)
            .await
            .expect_err("non-2xx must error");
        match err {
            CliError::Api(api) => {
                assert_eq!(api.http_status, 404, "status is preserved");
                assert_ne!(
                    api.code, 143_705,
                    "an oversized body must be abandoned, not mined for its code"
                );
                // The degraded state must be DISTINGUISHABLE from a genuinely
                // bodyless response. Both have `code: 0`, so without this marker
                // a downstream classifier keying on status alone draws a
                // conclusion the server never supported — e.g. the signing
                // mapper turning a slow `9992` into "poll forever".
                assert_eq!(api.code, 0);
                assert_eq!(
                    api.error_code.as_deref(),
                    Some(crate::error::ERR_BODY_UNAVAILABLE),
                    "an unreadable body must be marked, not silently look empty"
                );
                assert!(
                    api.message.contains("could not be read"),
                    "and must say so: {}",
                    api.message
                );
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        assert!(
            tokio::fs::metadata(&out).await.is_err(),
            "no output file on error"
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
    /// Empty"), while still attaching the bearer token.
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

    /// End-to-end: `post_empty` (the envelope-unwrapping bodyless POST used by
    /// `sign envelope send`) sends a POST with a zero-length body and no
    /// `Content-Type`, AND unwraps the standard `{"result","response"}`
    /// envelope (unlike `post_empty_raw`, which returns the body verbatim).
    #[tokio::test]
    async fn post_empty_sends_no_body_and_unwraps_envelope() {
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
                let body = br#"{"result":"yes","response":{"id":"env1","status":"sent"}}"#;
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
        let resp: Value = client
            .post_empty("/workspace/ws1/sign_envelopes/env1/send/")
            .await
            .expect("bodyless send succeeds");
        // Envelope unwrapped: the `response` object is returned, not the wrapper.
        assert_eq!(resp["status"], Value::String("sent".to_owned()));
        assert_eq!(resp["id"], Value::String("env1".to_owned()));

        let request = rx.await.expect("server captured request");
        assert!(
            request.starts_with("POST /workspace/ws1/sign_envelopes/env1/send/"),
            "expected bodyless POST to the send path, got: {request}"
        );
        let lower = request.to_ascii_lowercase();
        assert!(
            !lower.contains("content-type:"),
            "bodyless send must not set a Content-Type: {request}"
        );
        let after_headers = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            after_headers.is_empty(),
            "bodyless send must send no payload, got body: {after_headers:?}"
        );
    }

    /// End-to-end: a signing-style named-key BOOLEAN envelope
    /// (`{"result": true, "sign_envelope": {...}}`, with no `response` key) is
    /// preserved VERBATIM by `post_empty` — the shared handler only unwraps a
    /// `response` sub-object when present, otherwise it returns the full
    /// envelope (so the named `sign_envelope` payload and `result: true` survive
    /// intact, mirroring the documented signing send/details/list shapes).
    #[tokio::test]
    async fn post_empty_preserves_named_key_boolean_envelope() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await.unwrap_or(0);
                // Boolean `result`, NO `response` key, named `sign_envelope` payload.
                let body = br#"{"result":true,"sign_envelope":{"id":"env1","status":"sent"}}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
                let _ = tx.send(());
            }
        });
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let resp: Value = client
            .post_empty("/workspace/ws1/sign_envelopes/env1/send/")
            .await
            .expect("bodyless send succeeds");
        // No `response` key → the full envelope is preserved verbatim, including
        // the boolean `result` and the named-key `sign_envelope` payload.
        assert_eq!(resp["result"], Value::Bool(true));
        assert_eq!(
            resp["sign_envelope"]["id"],
            Value::String("env1".to_owned())
        );
        assert_eq!(
            resp["sign_envelope"]["status"],
            Value::String("sent".to_owned())
        );
        // The payload was NOT mistakenly unwrapped to a missing `response`.
        assert!(
            resp.get("response").is_none(),
            "named-key envelope must not gain a synthetic `response` key"
        );
        let () = rx.await.expect("server captured request");
    }

    #[test]
    fn password_header_name_is_in_secret_log_keys() {
        // Defense-in-depth: if the password ever lands in a logged body/form,
        // the redaction layer must mask it by key name.
        assert!(
            SECRET_LOG_KEYS
                .iter()
                .any(|k| k.eq_ignore_ascii_case(PASSWORD_HEADER)),
            "x-ve-password must be registered in SECRET_LOG_KEYS"
        );
    }

    #[test]
    fn build_password_header_marks_value_sensitive() {
        let pw = SecretString::from("hunter2".to_owned());
        let value = build_password_header(&pw).expect("valid password header");
        assert!(
            value.is_sensitive(),
            "the password header value must be marked sensitive"
        );
        // A well-formed value round-trips to the same bytes.
        assert_eq!(value.to_str().ok(), Some("hunter2"));
    }

    #[test]
    fn build_password_header_accepts_non_ascii_utf8_password() {
        // The link-password contract allows any 1-255 char UTF-8 value.
        // `HeaderValue::from_bytes` accepts the non-ASCII bytes (which
        // `from_str` would reject), so a password the user could set via the
        // management form is sendable from the CLI. It is still marked sensitive
        // and round-trips to the original BYTES.
        let secret = "pässwört→";
        let pw = SecretString::from(secret.to_owned());
        let value = build_password_header(&pw).expect("utf-8 password must be accepted");
        assert!(
            value.is_sensitive(),
            "the password header value must be marked sensitive"
        );
        assert_eq!(value.as_bytes(), secret.as_bytes());
    }

    #[test]
    fn build_password_header_rejects_control_chars_without_leaking_secret() {
        // A newline cannot be carried in a header value. The error must be the
        // dedicated InvalidHeaderValue variant, name only the header, and NEVER
        // echo the offending secret.
        let secret = "abc\ndef-SUPER-SECRET";
        let pw = SecretString::from(secret.to_owned());
        let err = build_password_header(&pw).expect_err("control char must be rejected");
        match &err {
            CliError::InvalidHeaderValue { header } => {
                assert_eq!(*header, PASSWORD_HEADER);
            }
            other => panic!("expected InvalidHeaderValue, got {other:?}"),
        }
        let rendered = err.to_string();
        assert!(
            !rendered.contains("SUPER-SECRET"),
            "the secret must never appear in the error message: {rendered}"
        );
        assert!(
            !rendered.contains("def"),
            "no fragment of the secret may appear in the error message: {rendered}"
        );
    }

    #[tokio::test]
    async fn get_with_password_fails_closed_on_redirect() {
        // H1: a password-bearing GET that receives a 3xx must FAIL CLOSED (the
        // no-redirect client never follows it, so the x-ve-password header can
        // never be forwarded to the Location target). The error names no
        // resource and embeds neither the secret nor the redirect URL.
        let addr = spawn_one_shot_redirect(
            "302 Found",
            "https://cdn.example.com/leak-target".to_owned(),
        )
        .await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let pw = SecretString::from("hunter2-SECRET".to_owned());
        let err = client
            .get_with_password::<Value>("/fileshare/1/details/", Some(&pw))
            .await
            .expect_err("a redirect on a password-bearing GET must fail closed");
        match &err {
            CliError::Parse(msg) => {
                assert!(
                    msg.contains("unexpected redirect"),
                    "expected a fail-closed redirect error, got: {msg}"
                );
                assert!(
                    !msg.contains("hunter2-SECRET"),
                    "the secret must never appear in the error: {msg}"
                );
                assert!(
                    !msg.contains("cdn.example.com"),
                    "the redirect target URL must not appear in the error: {msg}"
                );
            }
            other => panic!("expected CliError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn resolve_redirect_location_resolves_relative_against_request_url() {
        // A relative Location resolves against the request URL's origin + path.
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_static("/cdn/token/abc/file/clip.ts"),
        );
        let resolved = ApiClient::resolve_redirect_location(
            &headers,
            "https://api.fast.io/current/fileshare/1/storage/preview/hls_stream/read/",
        )
        .expect("relative Location resolves");
        assert_eq!(
            resolved.as_str(),
            "https://api.fast.io/cdn/token/abc/file/clip.ts"
        );
    }

    #[test]
    fn resolve_redirect_location_accepts_absolute_cross_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_static("https://cdn.example.com/dl/token123/file/clip.ts?x=1"),
        );
        let resolved = ApiClient::resolve_redirect_location(
            &headers,
            "https://api.fast.io/current/fileshare/1/storage/preview/hls_stream/read/",
        )
        .expect("absolute Location is taken wholesale");
        assert_eq!(
            resolved.as_str(),
            "https://cdn.example.com/dl/token123/file/clip.ts?x=1"
        );
    }

    #[test]
    fn resolve_redirect_location_rejects_non_http_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_static("file:///etc/passwd"),
        );
        let err = ApiClient::resolve_redirect_location(
            &headers,
            "https://api.fast.io/current/fileshare/1/storage/preview/pdf/read/",
        )
        .expect_err("non-http scheme must be rejected");
        assert!(matches!(err, CliError::Parse(_)));

        // A missing Location header is also a clear error, not a panic.
        let empty = HeaderMap::new();
        let err = ApiClient::resolve_redirect_location(
            &empty,
            "https://api.fast.io/current/fileshare/1/storage/preview/pdf/read/",
        )
        .expect_err("missing Location must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn build_follow_request_strips_authorization_and_password_headers() {
        // H4 (addendum F23): the leak-safe preview FOLLOW request must carry
        // NEITHER an Authorization header NOR an x-ve-password header — reqwest
        // does not strip custom headers across a redirect, so re-attaching
        // either would leak a credential to the CDN. The embedded download_token
        // in the URL is the sole authorizer.
        let client = ApiClient::new(
            "https://api.fast.io/current",
            Some("super-secret-bearer".to_owned()),
        )
        .expect("client builds");
        let follow_url = reqwest::Url::parse("https://cdn.example.com/dl/token123/file/clip.ts")
            .expect("valid url");
        let req = client
            .build_follow_request(follow_url)
            .build()
            .expect("request builds");

        assert_eq!(req.method(), reqwest::Method::GET);
        assert!(
            req.headers().get(AUTHORIZATION).is_none(),
            "the follow request must NOT carry an Authorization header"
        );
        assert!(
            req.headers().get(PASSWORD_HEADER).is_none(),
            "the follow request must NOT carry an x-ve-password header"
        );
        // Sanity: the bearer never appears anywhere in the built request.
        let serialized = format!("{:?}", req.headers());
        assert!(
            !serialized.contains("super-secret-bearer"),
            "the bearer token must not appear on the follow request: {serialized}"
        );
    }

    /// Serve a single HTTP/1.1 redirect (`status_line` + `Location: location`)
    /// then close. Returns the bound `127.0.0.1:<port>` address. Used to drive
    /// the manual preview-redirect follow without a live API.
    async fn spawn_one_shot_redirect(status_line: &'static str, location: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let header = format!(
                    "HTTP/1.1 {status_line}\r\nLocation: {location}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn preview_follows_one_redirect_then_streams_body() {
        // The primary preview GET 307s to a CDN URL (the embedded download_token
        // authorizes the follow); the follow streams the body to disk.
        let body = b"PREVIEW-BYTES";
        let cdn_addr = spawn_one_shot_server("200 OK", "video/mp2t", body).await;
        let primary_addr = spawn_one_shot_redirect(
            "307 Temporary Redirect",
            format!("http://{cdn_addr}/clip.ts"),
        )
        .await;
        let client = ApiClient::new(&format!("http://{primary_addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let dir = std::env::temp_dir().join(format!("fastio-preview-ok-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("clip.ts");

        let written = client
            .download_preview_following_redirect("/preview/", &output, None)
            .await
            .expect("preview follow streams to disk");
        assert_eq!(written, body.len() as u64);
        assert_eq!(
            tokio::fs::read(&output).await.expect("read output"),
            body,
            "the followed body must be written verbatim"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn preview_second_redirect_fails_closed() {
        // H4 / addendum F23: a redirect ON THE FOLLOW response (a second
        // redirect) must fail closed — never be chased, never written to disk.
        // Primary 307 → server B; server B 307s AGAIN → the follow sink rejects.
        let second_addr = spawn_one_shot_redirect(
            "307 Temporary Redirect",
            "http://example.invalid/x".to_owned(),
        )
        .await;
        let primary_addr = spawn_one_shot_redirect(
            "307 Temporary Redirect",
            format!("http://{second_addr}/again"),
        )
        .await;
        let client = ApiClient::new(&format!("http://{primary_addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let dir =
            std::env::temp_dir().join(format!("fastio-preview-2redir-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&dir).await;
        let output = dir.join("preview.ts");

        let err = client
            .download_preview_following_redirect("/preview/", &output, None)
            .await
            .expect_err("a second redirect must fail closed");
        match &err {
            CliError::Parse(msg) => assert!(
                msg.contains("second redirect"),
                "expected a clear second-redirect error, got: {msg}"
            ),
            other => panic!("expected CliError::Parse, got {other:?}"),
        }
        assert!(
            tokio::fs::metadata(&output).await.is_err(),
            "a fail-closed redirect must not write an output file"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ----- response-body read: idempotency-gated retry -----

    /// Serve `truncated` deliberately INCOMPLETE chunked responses (complete
    /// headers, one partial chunk, then close mid-body — the shape a
    /// `Transfer-Encoding: chunked` + `Connection: close` origin produces when
    /// the body read is cut short), then complete `Content-Length` responses
    /// carrying `body`. Returns the bound address and a counter of the requests
    /// the server ACTUALLY received — the count is what proves whether a
    /// request was re-sent.
    async fn spawn_flaky_body_server(
        truncated: usize,
        body: &'static [u8],
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let index = counter.fetch_add(1, Ordering::SeqCst);
                if index < truncated {
                    let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                  Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
                    let _ = sock.write_all(header.as_bytes()).await;
                    // A chunk header promising 0x20 bytes, fewer bytes than
                    // promised, and no terminator: the body never completes.
                    let _ = sock.write_all(b"20\r\n{\"result\":\"yes\",\"resp").await;
                    let _ = sock.flush().await;
                    continue;
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
            }
        });
        (addr, seen)
    }

    #[test]
    fn body_read_retry_is_allowed_only_for_idempotent_methods() {
        // The safety property in one assertion block: a lost RESPONSE means the
        // server already applied the request, so only methods that are safe to
        // repeat may be re-sent.
        for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
            assert!(
                ApiClient::should_retry_body_read(ReplayPolicy::IfMethodIsSafe, &method, 0),
                "{method} is idempotent and must be retried"
            );
        }
        for method in [
            reqwest::Method::POST,
            reqwest::Method::PUT,
            reqwest::Method::PATCH,
            reqwest::Method::DELETE,
        ] {
            assert!(
                !ApiClient::should_retry_body_read(ReplayPolicy::IfMethodIsSafe, &method, 0),
                "{method} mutates: re-sending it could DOUBLE-APPLY the mutation"
            );
        }
    }

    #[test]
    fn body_read_retry_is_refused_for_side_effecting_endpoints() {
        // The second, independent gate: an ACTIONFUL GET is never re-sent even
        // though its method is idempotent. `GET` is only a proxy for
        // replay-safety and this API breaks the proxy.
        for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
            assert!(
                !ApiClient::should_retry_body_read(ReplayPolicy::Never, &method, 0),
                "a side-effecting {method} must never be re-sent"
            );
        }
    }

    #[test]
    fn body_read_retry_respects_the_shared_attempt_budget() {
        assert!(
            ApiClient::should_retry_body_read(
                ReplayPolicy::IfMethodIsSafe,
                &reqwest::Method::GET,
                MAX_RETRIES - 1
            ),
            "the last retry must still be available"
        );
        assert!(
            !ApiClient::should_retry_body_read(
                ReplayPolicy::IfMethodIsSafe,
                &reqwest::Method::GET,
                MAX_RETRIES
            ),
            "the budget must be exhausted after MAX_RETRIES"
        );
    }

    #[tokio::test]
    async fn get_body_read_is_retried_after_a_truncated_response() {
        // The observed bug: the server answers with chunked + Connection: close and
        // the body read occasionally comes up short. A GET is idempotent, so
        // the client re-sends and the caller sees success.
        let body = br#"{"result":"yes","response":{"id":"n1"}}"#;
        let (addr, seen) = spawn_flaky_body_server(1, body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let resp: Value = client
            .get("/user/shares/")
            .await
            .expect("a GET must survive one truncated body");
        assert_eq!(resp["id"], Value::String("n1".to_owned()));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            2,
            "the GET must have been re-sent exactly once"
        );
    }

    #[tokio::test]
    async fn post_body_read_is_never_retried_so_a_mutation_cannot_double_apply() {
        // THE safety test. The server received and processed the POST; only the
        // response was lost. Re-sending would apply the mutation a SECOND time,
        // so the error must surface instead — and the server must have seen
        // exactly ONE request.
        let body = br#"{"result":"yes","response":{"id":"n1"}}"#;
        let (addr, seen) = spawn_flaky_body_server(1, body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let mut form = HashMap::new();
        form.insert("name".to_owned(), "room-1".to_owned());
        let err = client
            .post::<Value>("/room/", &form)
            .await
            .expect_err("a lost POST response must surface, never be re-sent");
        match &err {
            CliError::Parse(msg) => assert!(
                msg.starts_with("failed to parse API response"),
                "expected the unchanged parse-error wording, got: {msg}"
            ),
            other => panic!("expected CliError::Parse, got {other:?}"),
        }
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "the mutation must have been sent EXACTLY once"
        );
    }

    #[tokio::test]
    async fn malformed_json_body_is_not_retried() {
        // The body arrived intact — it is simply not JSON. That is a server-side
        // defect, not a transport failure: re-sending it would only loop.
        let (addr, seen) = spawn_flaky_body_server(0, b"<html>gateway error</html>").await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = client
            .get::<Value>("/user/shares/")
            .await
            .expect_err("a non-JSON body must be an error");
        match &err {
            CliError::Parse(msg) => assert!(
                msg.starts_with("failed to parse API response"),
                "expected a parse error, got: {msg}"
            ),
            other => panic!("expected CliError::Parse, got {other:?}"),
        }
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "a JSON syntax error must NOT be retried"
        );
    }

    #[tokio::test]
    async fn get_body_read_retries_are_bounded_by_the_retry_budget() {
        // A GET whose body read fails every time must still terminate, having
        // spent the SAME budget the send retry has always had.
        let body = br#"{"result":"yes","response":{"id":"n1"}}"#;
        let (addr, seen) = spawn_flaky_body_server(usize::MAX, body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = client
            .get::<Value>("/user/shares/")
            .await
            .expect_err("an always-truncated body must eventually error");
        assert!(
            matches!(&err, CliError::Parse(msg) if msg.starts_with("failed to parse API response")),
            "expected the unchanged parse-error wording, got: {err:?}"
        );
        assert_eq!(
            seen.load(Ordering::SeqCst),
            (MAX_RETRIES + 1) as usize,
            "the body-read retry must share the send retry's budget"
        );
    }

    // ----- actionful GETs: pinned to the non-replaying path -----
    //
    // Each of these endpoints is a GET the server ACTS on. The assertion that
    // matters is the request COUNT: the server must have seen the request
    // exactly ONCE even though its response body was lost, because the effect
    // (an SMS delivered, a token minted, an access preauthorized) already
    // happened and a second send would apply it again.

    /// A flaky-body server that loses the FIRST response body and would serve a
    /// good one on any re-send — so a replayed request is visible as a second
    /// request, and a non-replayed one surfaces the error.
    async fn spawn_lost_body_server() -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ApiClient,
    ) {
        // The good body carries every field the pinned endpoints deserialize
        // (a token, a sent flag, an auth_request_id) so that a REPLAY would
        // genuinely SUCCEED. Otherwise a replayed call could still fail — on
        // the deserialize step — and the test would go red for the wrong
        // reason, hiding the very replay it exists to catch.
        let body = br#"{"result":"yes","response":{"token":"t1","sent":true,"auth_request_id":"ar1","expires_in":3600,"auth_token":"jwt-1"}}"#;
        let (addr, seen) = spawn_flaky_body_server(1, body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");
        (addr, seen, client)
    }

    /// Assert the call failed with the unchanged parse wording and that the
    /// server saw the request exactly once.
    fn assert_not_replayed<T: std::fmt::Debug>(
        result: &Result<T, CliError>,
        seen: &std::sync::atomic::AtomicUsize,
        what: &str,
    ) {
        match result {
            Err(CliError::Parse(msg)) => assert!(
                msg.starts_with("failed to parse API response"),
                "expected the unchanged parse-error wording for {what}, got: {msg}"
            ),
            Err(other) => panic!("expected CliError::Parse for {what}, got {other:?}"),
            Ok(value) => panic!("{what} must NOT be re-sent to recover its body, got: {value:?}"),
        }
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "{what} must have reached the server EXACTLY once"
        );
    }

    #[tokio::test]
    async fn two_factor_send_is_never_replayed() {
        // The one that matters most: a replay sends the user a SECOND SMS /
        // phone call / WhatsApp message.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let result = crate::api::auth::two_factor_send(&client, "sms").await;
        assert_not_replayed(&result, &seen, "the 2FA send");
    }

    #[tokio::test]
    async fn websocket_auth_is_never_replayed() {
        // Mints a realtime-channel token; a replay mints a second.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let result = crate::api::fileshare::websocket_auth(&client, "fs1").await;
        assert_not_replayed(&result, &seen, "the websocket-auth mint");
    }

    #[tokio::test]
    async fn preview_preauthorize_is_never_replayed() {
        // Preauthorizes preview access; a replay preauthorizes again.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let result =
            crate::api::preview::get_preview_url(&client, "workspace", "123", "n1", "thumbnail")
                .await;
        assert_not_replayed(&result, &seen, "the preview preauthorize");
    }

    #[tokio::test]
    async fn transform_requestread_is_never_replayed() {
        // Mints a download token; a replay mints a second.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let params = crate::api::preview::TransformUrlParams {
            context_type: "workspace",
            context_id: "123",
            node_id: "n1",
            transform_name: "image",
            width: Some(100),
            height: None,
            output_format: None,
            size: None,
            crop_width: None,
            crop_height: None,
            crop_x: None,
            crop_y: None,
            rotate: None,
        };
        let result = crate::api::preview::get_transform_url(&client, &params).await;
        assert_not_replayed(&result, &seen, "the transform requestread");
    }

    #[tokio::test]
    async fn download_requestread_is_never_replayed() {
        // Mints a download token; a replay mints a second.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let result = crate::api::download::get_download_url(&client, "123", "n1").await;
        assert_not_replayed(&result, &seen, "the workspace requestread");
    }

    #[tokio::test]
    async fn download_requestread_ctx_is_never_replayed() {
        // The context-aware twin of the above — same mint, same rule.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let result =
            crate::api::download::get_download_url_ctx(&client, "workspace", "123", "n1", None)
                .await;
        assert_not_replayed(&result, &seen, "the context requestread");
    }

    #[tokio::test]
    async fn event_summarize_is_never_replayed_on_either_branch() {
        // Runs an AI pass that SPENDS credits; a replay double-spends. The
        // endpoint has two send shapes (bare vs. query-parameter) and BOTH must
        // be pinned — a fix applied to only one branch would leave the other
        // replayable.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let bare = crate::api::event::SummarizeEventsParams::default();
        let result = crate::api::event::summarize_events(&client, &bare).await;
        assert_not_replayed(&result, &seen, "the bare event summarize");

        let (_addr2, seen2, client2) = spawn_lost_body_server().await;
        let filtered = crate::api::event::SummarizeEventsParams {
            user_context: Some("why did uploads spike"),
            ..Default::default()
        };
        let result2 = crate::api::event::summarize_events(&client2, &filtered).await;
        assert_not_replayed(&result2, &seen2, "the filtered event summarize");
    }

    #[tokio::test]
    async fn pkce_authorize_is_never_replayed() {
        // CREATES a pending authorization request; a replay creates a second.
        // Also covers the unauthenticated query-parameter shape.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        let result = crate::api::auth::pkce_authorize(
            &client,
            "client-1",
            "challenge",
            "state-1",
            "http://127.0.0.1/callback",
            None,
        )
        .await;
        assert_not_replayed(&result, &seen, "the PKCE authorize");
    }

    #[tokio::test]
    async fn sign_in_is_never_replayed() {
        // MINTS A JWT. A replay yields two live tokens on success, and on a
        // failed sign-in burns a second attempt against lockout / rate-limit
        // accounting. Reached via `get_with_auth`, which is why an enumeration
        // of `client.get(` call sites could not see it.
        let (_addr, seen, client) = spawn_lost_body_server().await;
        // Project to a NON-SECRET field before asserting: `SignInResponse`
        // carries the JWT and deliberately has no `Debug`, so the failure
        // message can never print a token.
        let result = crate::api::auth::sign_in(&client, "user@example.com", "pw")
            .await
            .map(|resp| resp.expires_in);
        assert_not_replayed(&result, &seen, "the sign-in");
    }

    // ----- gateway errors (502-504): the second door -----

    /// Serve `gateway_errors` HTTP 502 responses (with the JSON error envelope
    /// this API renders), then complete 200 responses carrying `body`. Counts
    /// the requests received.
    async fn spawn_gateway_error_server(
        gateway_errors: usize,
        body: &'static [u8],
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let (status, payload): (&str, &[u8]) = if index < gateway_errors {
                    (
                        "502 Bad Gateway",
                        br#"{"result":"no","error":{"code":502,"text":"bad gateway"}}"#,
                    )
                } else {
                    ("200 OK", body)
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(payload).await;
                let _ = sock.flush().await;
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn side_effecting_endpoint_does_not_retry_a_gateway_error() {
        // A 502 can mean the upstream PROCESSED the request and the gateway
        // lost the response — the same double-apply hazard as a lost body,
        // through a different door. The 2FA send must not go out twice.
        let body = br#"{"result":"yes","response":{"sent":true}}"#;
        let (addr, seen) = spawn_gateway_error_server(1, body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = crate::api::auth::two_factor_send(&client, "sms")
            .await
            .expect_err("a gateway error on a side-effecting GET must surface");
        match &err {
            CliError::Api(api) => assert_eq!(api.http_status, 502, "the 502 must survive"),
            other => panic!("expected CliError::Api, got {other:?}"),
        }
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "the user must receive exactly ONE code even on a gateway error"
        );
    }

    #[tokio::test]
    async fn gateway_error_retry_is_unchanged_for_ordinary_requests() {
        // REGRESSION GUARD for the narrow scope of the change above. Ordinary
        // requests — every POST/PUT/PATCH/DELETE and every pure-read GET — must
        // keep retrying 502-504 exactly as they always have. If a future edit
        // widens the side-effecting gate into a blanket "never retry gateway
        // errors", this test goes red.
        let body = br#"{"result":"yes","response":{"id":"n1"}}"#;

        let (post_addr, post_seen) = spawn_gateway_error_server(1, body).await;
        let post_client = ApiClient::new(&format!("http://{post_addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let mut form = HashMap::new();
        form.insert("name".to_owned(), "room-1".to_owned());
        let posted: Value = post_client
            .post("/room/", &form)
            .await
            .expect("an ordinary POST must still retry a 502 and succeed");
        assert_eq!(posted["id"], Value::String("n1".to_owned()));
        assert_eq!(
            post_seen.load(Ordering::SeqCst),
            2,
            "the POST must have been retried exactly once"
        );

        let (get_addr, get_seen) = spawn_gateway_error_server(1, body).await;
        let get_client = ApiClient::new(&format!("http://{get_addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let got: Value = get_client
            .get("/user/shares/")
            .await
            .expect("an ordinary GET must still retry a 502 and succeed");
        assert_eq!(got["id"], Value::String("n1".to_owned()));
        assert_eq!(
            get_seen.load(Ordering::SeqCst),
            2,
            "the GET must have been retried exactly once"
        );
    }

    // ----- transport send failures: the third door -----

    /// Accept `dropped` connections, READ the request, then close without
    /// sending any response — the shape a server produces when it received (and
    /// may already have processed) the request but the connection died before
    /// the reply. Later connections get a complete `body`. The counter is the
    /// proof: the server demonstrably SAW the request it never answered.
    async fn spawn_dropping_server(
        dropped: usize,
        body: &'static [u8],
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let index = counter.fetch_add(1, Ordering::SeqCst);
                if index < dropped {
                    // Close with no response at all.
                    continue;
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn side_effecting_endpoint_does_not_retry_an_ambiguous_transport_failure() {
        // The third door. The server RECEIVED the request (the counter proves
        // it) and the connection then died before the response — so the SMS may
        // already be on its way. Re-sending would send a second one.
        let body = br#"{"result":"yes","response":{"sent":true}}"#;
        let (addr, seen) = spawn_dropping_server(1, body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let err = crate::api::auth::two_factor_send(&client, "sms")
            .await
            .expect_err("an ambiguous transport failure must surface, not re-send");
        assert!(
            matches!(&err, CliError::Http(_)),
            "expected the transport error to surface verbatim, got: {err:?}"
        );
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "the user must receive exactly ONE code"
        );
    }

    #[tokio::test]
    async fn transport_error_retry_is_unchanged_for_ordinary_requests() {
        // Mirror regression guard: ordinary requests must keep retrying the
        // same ambiguous transport failure exactly as they always have.
        let body = br#"{"result":"yes","response":{"id":"n1"}}"#;
        let (addr, seen) = spawn_dropping_server(1, body).await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let got: Value = client
            .get("/user/shares/")
            .await
            .expect("an ordinary GET must still retry a dropped connection");
        assert_eq!(got["id"], Value::String("n1".to_owned()));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            2,
            "the ordinary GET must have been retried exactly once"
        );
    }

    /// Produce a REAL `reqwest` connect error by dialing a port that was bound
    /// and then released, so nothing is listening.
    async fn real_connect_error() -> reqwest::Error {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);
        reqwest::Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect_err("connecting to a closed port must fail")
    }

    /// Produce a REAL `reqwest` timeout error: a server that accepts and never
    /// answers, against a client with a short timeout.
    async fn real_timeout_error() -> reqwest::Error {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                // Hold the connection open, answering nothing.
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        reqwest::Client::builder()
            .timeout(Duration::from_millis(150))
            .build()
            .expect("client builds")
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect_err("a silent server must time out")
    }

    #[tokio::test]
    async fn connect_failure_is_still_retried_for_a_side_effecting_endpoint() {
        // Asserted against a REAL reqwest error, not a hand-built one: the
        // connection was never established, so the server provably never saw
        // the request and re-sending cannot double-apply. Keeping this
        // retryable is what stops actionful calls being needlessly fragile.
        let err = real_connect_error().await;
        assert!(err.is_connect(), "expected a connect error, got: {err:?}");
        assert!(
            ApiClient::should_retry_transport_error(ReplayPolicy::Never, &err),
            "a connect failure never reached the server and stays retryable"
        );
    }

    #[tokio::test]
    async fn ambiguous_timeout_is_refused_for_a_side_effecting_endpoint() {
        // The distinction that matters, again against a REAL reqwest error: a
        // timeout does NOT prove the server was untouched — it may have
        // received, processed and answered while the client gave up waiting.
        let err = real_timeout_error().await;
        assert!(err.is_timeout(), "expected a timeout error, got: {err:?}");
        assert!(
            !err.is_connect(),
            "a response timeout must not be classified as a connect failure"
        );
        assert!(
            !ApiClient::should_retry_transport_error(ReplayPolicy::Never, &err),
            "an ambiguous timeout must NOT be re-sent for a side-effecting endpoint"
        );
        assert!(
            ApiClient::should_retry_transport_error(ReplayPolicy::IfMethodIsSafe, &err),
            "ordinary requests must keep retrying timeouts exactly as before"
        );
    }
}
