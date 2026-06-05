/// Error types for the Fast.io CLI.
///
/// Uses `thiserror` for structured error variants. Command handlers
/// convert these into `anyhow::Error` with user-friendly context.
use std::fmt;

use colored::Colorize;

/// Top-level error type for the CLI.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CliError {
    /// An error returned by the Fast.io API.
    #[error("{0}")]
    Api(#[from] ApiError),

    /// Authentication failure (missing or expired credentials).
    #[error("Authentication error: {0}")]
    Auth(String),

    /// Configuration file error.
    #[error("Configuration error: {0}")]
    Config(String),

    /// File system I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// HTTP transport error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Data parsing or serialization error.
    #[error("Parse error: {0}")]
    Parse(String),

    /// API rate limit exceeded.
    #[error("Rate limit exceeded. Retry after {retry_after_secs} seconds.")]
    RateLimit {
        /// Seconds until the rate limit resets.
        retry_after_secs: u64,
    },

    /// An asynchronously-generated artifact (e.g. a signed PDF or audit
    /// certificate) is not ready yet.
    ///
    /// Surfaced as an HTTP 404 by the server, but it is NOT a genuine
    /// not-found: the resource ids are correct, the artifact simply has not
    /// been rendered yet. This variant exists so the rendered `hint:` line is
    /// the poll-and-retry guidance (see [`CliError::suggestion`]) instead of
    /// the misleading generic-404 "Verify the ID or path is correct" — the ids
    /// are fine.
    ///
    /// It WRAPS the original [`ApiError`] BY VALUE in a plain `api` field — NOT
    /// `#[source]` / `#[from]` — so:
    /// - its `Display` is the FULL server error verbatim (`[HTTP …] … (code …)`
    ///   plus any `see:` / `resource:` details), rendered EXACTLY ONCE because
    ///   no `source()` link is generated for a plain field, so the `anyhow`
    ///   chain carries no duplicate (LV-1); and
    /// - `suggestion()` can return the poll hint instead of the inner
    ///   `ApiError`'s generic-404 hint (LV-2).
    ///
    /// Command handlers construct it via the signing error-mapping layer; the
    /// shared hint is deliberately generic (no resource wording) so the variant
    /// stays usable for any async artifact and the signing-specific phrasing
    /// lives in the mapping layer's `.context(...)`, not here.
    #[error("{api}")]
    ArtifactNotReady {
        /// The original server error, preserved verbatim for `Display` (carries
        /// the HTTP status, code, and any `documentation_url` / `resource`
        /// details). Intentionally a plain field, NOT `#[source]`, so it adds
        /// no duplicate link to the rendered `anyhow` chain.
        api: ApiError,
    },

    /// A user-supplied secret (e.g. a link password) cannot be carried in an
    /// HTTP header value.
    ///
    /// The header value is built from the secret's raw bytes via
    /// [`reqwest::header::HeaderValue::from_bytes`], which accepts any byte
    /// EXCEPT control characters (including CR/LF) and a few disallowed header
    /// bytes — so a non-ASCII UTF-8 value (e.g. `"pässwört→"`) is fine, but a
    /// value containing a newline or other control byte cannot be turned into a
    /// [`reqwest::header::HeaderValue`]. This variant is raised at the seam that
    /// builds such a header so the failure is a clear client-side validation
    /// error rather than a panic or a confusing transport error.
    ///
    /// `header` names ONLY the header that could not be built — the offending
    /// VALUE is NEVER embedded, because it is a secret (this is the whole point
    /// of failing here). The wording is deliberately resource-agnostic so the
    /// variant stays reusable for any header-bound secret; the `suggestion()`
    /// hint explains the likely cause (the value contains characters HTTP
    /// headers cannot carry) without naming any specific feature.
    #[error("invalid value for {header} header")]
    InvalidHeaderValue {
        /// The name of the header that could not be constructed (e.g.
        /// `x-ve-password`). The disallowed value is intentionally NOT carried
        /// here — it is a secret.
        header: &'static str,
    },

    /// An [`ApiError`] for which a command-layer error mapper has supplied an
    /// OVERRIDE recovery hint (or chosen to SUPPRESS the generic one).
    ///
    /// Motivation: a command-scoped mapper (e.g. a File-Share or signing mapper)
    /// re-frames a raw API error with a resource-specific `.context(...)` message
    /// — but that context is layered ON TOP OF a `CliError::Api`, which the render
    /// layer still downcasts to in order to fetch `suggestion()`. The inner
    /// `ApiError`'s GENERIC status hint (e.g. "Run `fastio auth login`" for a 401,
    /// "Verify the ID or path is correct." for a 404, the generic-403 line) then
    /// gets appended UNDERNEATH the mapper's careful wording, contradicting it.
    /// Wrapping the error in this variant lets the mapper own the rendered `hint:`
    /// line: `suggestion()` returns the override (or `None` to print no hint at
    /// all), instead of the inner `ApiError`'s status default.
    ///
    /// Like [`CliError::ArtifactNotReady`], `api` is a PLAIN field (NOT
    /// `#[source]` / `#[from]`), so:
    /// - its `Display` is the inner `ApiError`'s `Display` verbatim (`[HTTP …] …
    ///   (code …)` plus any `see:` / `resource:` details), preserving the exit
    ///   code and the full server message; and
    /// - no `source()` link is generated, so the rendered `anyhow` chain carries
    ///   no duplicate `ApiError` block (the dedup invariant in `main.rs`'s
    ///   `render_chain_dedup` is untouched).
    ///
    /// `hint` is `Option<&'static str>` so the MAPPING LAYER owns the override
    /// TEXT (passed in as a `const` from the command module) — keeping this
    /// variant RESOURCE-AGNOSTIC: it embeds no feature-specific wording itself,
    /// only relaying whatever static string the caller provided (or `None`).
    #[error("{api}")]
    MappedApi {
        /// The original server error, preserved verbatim for `Display` and for
        /// the HTTP status / code it carries. A plain field (NOT `#[source]`), so
        /// it adds no duplicate link to the rendered `anyhow` chain — identical to
        /// [`CliError::ArtifactNotReady`].
        api: ApiError,
        /// The override recovery hint the mapping layer chose. `Some(text)`
        /// replaces the inner `ApiError`'s generic status hint with `text`;
        /// `None` SUPPRESSES the hint entirely (no `hint:` line is printed). The
        /// text is owned by the caller (a command-module `const`), so this variant
        /// stays resource-agnostic.
        hint: Option<&'static str>,
    },

    /// A compare-and-swap (optimistic-concurrency) write was rejected because
    /// the target moved on since the version the caller based their change on.
    ///
    /// Surfaced when a server returns a version-mismatch on a conditional write
    /// (the caller passed an "if the current version is X" precondition and the
    /// current version is no longer X). The wording is deliberately
    /// resource-AGNOSTIC — "the target file changed" rather than naming any one
    /// feature — so the variant is reusable for any CAS write; feature-specific
    /// phrasing belongs in the command layer's `.context(...)`, never here. The
    /// `suggestion()` hint is the rebase recipe: re-fetch the latest, re-apply
    /// the change, and retry with the current version id.
    #[error(
        "the target file changed since the version you supplied (current version: {current_version})"
    )]
    VersionConflict {
        /// The version id that is now current on the server — the value the
        /// caller should rebase onto and retry with. Not a secret.
        current_version: String,
    },
}

impl CliError {
    /// Return a human-readable suggestion for recovering from this error.
    #[must_use]
    pub fn suggestion(&self) -> Option<&'static str> {
        match self {
            Self::Auth(_) => Some("Run `fastio auth login` to sign in."),
            Self::Config(_) => {
                Some("Run `fastio configure init` to set up a profile, or check your config file.")
            }
            Self::RateLimit { .. } => {
                Some("Wait for the rate limit window to reset, then retry your request.")
            }
            Self::Api(api_err) => api_err.suggestion(),
            // The mapping layer OWNS the rendered hint: `Some(text)` overrides the
            // inner ApiError's generic status hint; `None` suppresses it entirely.
            Self::MappedApi { hint, .. } => *hint,
            Self::ArtifactNotReady { .. } => Some(HINT_ARTIFACT_NOT_READY),
            Self::InvalidHeaderValue { .. } => Some(HINT_INVALID_HEADER_VALUE),
            Self::VersionConflict { .. } => Some(HINT_VERSION_CONFLICT),
            Self::Http(_) => {
                Some("Check your network connection and verify the API base URL is correct.")
            }
            Self::Io(_) | Self::Parse(_) => None,
        }
    }

    /// Format the error for display on stderr with colors.
    pub fn render_stderr(&self) {
        eprintln!("{} {self}", "error:".red().bold());
        if let Some(hint) = self.suggestion() {
            eprintln!("{} {hint}", "hint:".yellow().bold());
        }
    }
}

/// Shared "no active plan / credits exhausted" upgrade hint.
///
/// Single source of truth so billing, signing, Ripley, and workflow all emit
/// a consistent recovery path for HTTP 402 and code `1688`. Plan IDs/names are
/// deliberately NOT hardcoded here — callers drive them off
/// `GET /org/billing/plan/list/`. References only commands that exist in the
/// current CLI surface (`org billing plans`, `org billing subscribe`, both
/// landed in Phase 7); the onboarding URL is the canonical recovery path.
pub const HINT_SUBSCRIPTION_REQUIRED: &str = "No active paid plan or credits exhausted. \
     Run `fastio org billing plans` to see options, then `fastio org billing subscribe <org> --plan <id>`, \
     or visit https://go.fast.io/onboarding.";

/// Shared "feature requires a higher tier" upgrade hint (code `1695`).
pub const HINT_UPGRADE_REQUIRED: &str =
    "This feature requires a higher plan tier. See `fastio org billing plans`.";

/// Shared "credit limit reached" hint (code `1696`).
pub const HINT_CREDIT_LIMIT: &str =
    "Credit limit reached. See `fastio org billing plans`, or visit https://go.fast.io/onboarding.";

/// Shared generic "access restricted" hint (code `1670`).
///
/// Code `1670` is a GENERAL-purpose restricted/access-denied code in this API
/// (2FA restrictions, workspace/share/upload limits, workflow availability,
/// etc.), so this hint is deliberately resource-agnostic. The server's own
/// `error.text` carries the specifics.
pub const HINT_RESTRICTED: &str =
    "Access restricted — your plan, role, or account state does not permit this action.";

/// Shared generic "feature/plan limit reached" hint (code `1685`).
///
/// Code `1685` is a GENERAL-purpose feature-limit / precondition-failed code in
/// this API (workspace/share/upload limits, workflow availability, etc.), so
/// this hint is deliberately resource-agnostic.
pub const HINT_FEATURE_LIMIT: &str =
    "A plan or feature limit was reached for this operation; a higher plan tier may be required.";

/// Shared generic "router does not recognize this path" hint (code `9992`).
///
/// Code `9992` is a ROUTER-level "no such route" error — NOT specific to any
/// resource. It commonly means the path was removed or renamed; this hint is
/// deliberately resource-agnostic (it must NOT mention signing — that wording
/// lives in `map_signing_error` / the MCP `sign_err_to_result`).
pub const HINT_UNKNOWN_ROUTE: &str = "The server does not recognize this API path — the route may have been removed or renamed. \
     Check for a `fastio` CLI update.";

/// Shared "not a member of this workspace" hint (code `10545`).
///
/// Code `10545` is a GENERIC workspace-access code (the caller is authenticated
/// but lacks membership of the target workspace), not specific to signing — so
/// this hint is resource-agnostic and must NOT mention signing. It exists so the
/// rendered `hint:` line steers to the access problem instead of the misleading
/// generic-401 "run `fastio auth login`" suggestion (the caller IS authenticated).
pub const HINT_WORKSPACE_MEMBERSHIP: &str = "You are not a member of this workspace. \
     Ask a workspace admin to add you, or verify the workspace ID.";

/// Shared "resource access not granted" hint (code `115069`).
///
/// Code `115069` is an access-denied code (the caller is authenticated but the
/// specific resource is not shared with their account). This hint is
/// deliberately resource-agnostic and must NOT mention signing; it replaces the
/// misleading generic-401 "run `fastio auth login`" suggestion.
pub const HINT_RESOURCE_ACCESS: &str = "Access to this resource is not granted to your account. \
     Verify the resource ID and that you have permission on it.";

/// Shared "asynchronously-generated artifact not ready" hint
/// ([`CliError::ArtifactNotReady`]).
///
/// The server returns HTTP 404 until the artifact (a signed PDF, audit
/// certificate, etc.) is rendered, but it is NOT a genuine not-found — the ids
/// are correct. This hint therefore must NOT steer the user to re-check the id
/// (the generic-404 "Verify the ID or path is correct."): the recovery is to
/// poll and retry. Kept resource-agnostic (no "sign" wording) so it stays a
/// generic hint; the signing-specific phrasing lives in the mapping layer's
/// added `.context(...)`.
pub const HINT_ARTIFACT_NOT_READY: &str = "The requested artifact is generated asynchronously and is not ready yet. \
     Poll the resource and retry once it reaches the required (terminal) stage.";

/// Shared "secret cannot be carried in an HTTP header" hint
/// ([`CliError::InvalidHeaderValue`]).
///
/// The value is sent via `HeaderValue::from_bytes`, which accepts non-ASCII
/// UTF-8 but rejects control characters (including newlines) and a few
/// disallowed header bytes — so only such bytes can trip this error. The hint
/// stays resource-agnostic — it names no specific feature — and NEVER echoes the
/// offending value (it is a secret).
pub const HINT_INVALID_HEADER_VALUE: &str = "The supplied value contains a control character or newline, which cannot be \
     carried in an HTTP header. Non-ASCII letters are fine; re-check the value and remove any control characters or line breaks.";

/// Shared compare-and-swap (optimistic-concurrency) conflict hint
/// ([`CliError::VersionConflict`]).
///
/// The conditional write was rejected because the target advanced past the
/// version the caller based their change on. The recovery is the rebase recipe:
/// re-fetch the latest, re-apply the change, and retry with the now-current
/// version id. Kept resource-agnostic (no feature wording) so the variant stays
/// reusable; the conflict error's `Display` carries the current version id.
pub const HINT_VERSION_CONFLICT: &str = "The target changed since the version you supplied. \
     Re-fetch the latest, re-apply your changes, then retry using the current version id shown above.";

/// An error returned by the Fast.io REST API.
///
/// The optional [`ApiError::details`] field carries structured server
/// diagnostics (field-validation `params[]`, a `validation_report`, a
/// conflict/fire `reason`, plus `documentation_url`/`resource`) so command
/// handlers can surface them. The contained JSON is server diagnostics, never
/// a credential, so the derived `Debug` is safe (no token-bearing fields).
#[derive(Debug)]
#[non_exhaustive]
pub struct ApiError {
    /// Numeric API error code (e.g. 1650).
    pub code: u32,
    /// Machine-readable error identifier (e.g. `APP_AUTH_INVALID`).
    pub error_code: Option<String>,
    /// Human-readable error message.
    pub message: String,
    /// HTTP status code of the response.
    pub http_status: u16,
    /// Structured server diagnostics preserved from the error envelope
    /// (`params[]`, `validation_report`, `reason`, `documentation_url`,
    /// `resource`). `None` when the envelope carried no extra detail.
    ///
    /// Boxed to keep `ApiError` (and therefore `CliError`) small: a
    /// `serde_json::Value` is a large enum, and `Result<_, CliError>` appears
    /// on nearly every function in the crate, so an inline `Value` would
    /// bloat every `Result`'s error variant.
    pub details: Option<Box<serde_json::Value>>,
}

impl ApiError {
    /// Construct an `ApiError` with no structured `details`.
    ///
    /// Convenience constructor for the common case; equivalent to setting
    /// `details: None` on the struct literal. Keeps call sites terse now that
    /// the struct is `#[non_exhaustive]`.
    #[must_use]
    pub fn new(code: u32, error_code: Option<String>, message: String, http_status: u16) -> Self {
        Self {
            code,
            error_code,
            message,
            http_status,
            details: None,
        }
    }

    /// Return a human-readable suggestion based on the HTTP status or error code.
    #[must_use]
    pub fn suggestion(&self) -> Option<&'static str> {
        // Check specific error codes before falling back to HTTP status.
        if self.code == 10587 {
            return Some(
                "Account email not verified. Run `fastio auth verify --email <your-email>` to resend the verification email.",
            );
        }
        // Billing / entitlement codes share centrally-defined hint strings so
        // billing, signing, Ripley, and workflow stay consistent. These are
        // checked before the HTTP-status fallback because several map onto
        // 402/403 where the generic status hint is less actionable.
        match self.code {
            1688 => return Some(HINT_SUBSCRIPTION_REQUIRED),
            1695 => return Some(HINT_UPGRADE_REQUIRED),
            1696 => return Some(HINT_CREDIT_LIMIT),
            1670 => return Some(HINT_RESTRICTED),
            1685 => return Some(HINT_FEATURE_LIMIT),
            9992 => return Some(HINT_UNKNOWN_ROUTE),
            // Access-denied codes that surface as HTTP 401 but are NOT a missing
            // login (the caller IS authenticated). Without these arms the bare
            // 401 fallback below would emit the misleading "run `fastio auth
            // login`" hint.
            10545 => return Some(HINT_WORKSPACE_MEMBERSHIP),
            115_069 => return Some(HINT_RESOURCE_ACCESS),
            _ => {}
        }
        match self.http_status {
            401 => Some("Authentication failed. Run `fastio auth login` to sign in."),
            // 402 with no recognized billing code still steers to the plan
            // surface — the shared subscription-required hint is the right
            // default recovery path.
            402 => Some(HINT_SUBSCRIPTION_REQUIRED),
            403 => Some("Permission denied. Check that your account has the required role."),
            404 => Some("Resource not found. Verify the ID or path is correct."),
            409 => Some(
                "A conflicting request is in progress for this resource. Wait a moment and retry.",
            ),
            429 => Some("Rate limited. Wait a moment and try again."),
            500..=599 => Some("Server error. The Fast.io API may be experiencing issues."),
            _ => None,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[HTTP {}] {}", self.http_status, self.message)?;
        if self.code > 0 {
            write!(f, " (code {})", self.code)?;
        }
        if let Some(ref ec) = self.error_code {
            write!(f, " [{ec}]")?;
        }
        if let Some(ref details) = self.details {
            render_details(f, details)?;
        }
        Ok(())
    }
}

/// Render the structured server `details` onto an [`ApiError`]'s `Display`.
///
/// Without this, the Phase-0 enrichment (`reason`, `validation_report`,
/// `params[]`, `documentation_url`, `resource`) harvested into
/// [`ApiError::details`] is invisible to both the CLI (anyhow → `Display` →
/// stderr) and MCP (`cli_err_to_result` → `to_string`). The rendering is a
/// compact, multi-line digest appended after the headline so a 422 template
/// `validation_report`, a trigger-fire 409 `reason`, and 400 `params[]`
/// surface through one shared path.
///
/// The contained JSON is server diagnostics (never a credential), but it is
/// still untrusted text; long values are truncated so the message stays
/// readable on a terminal.
fn render_details(f: &mut fmt::Formatter<'_>, details: &serde_json::Value) -> fmt::Result {
    use serde_json::Value;

    // `reason` (409 fire/conflict): a string or a structured object.
    if let Some(reason) = details.get("reason").filter(|v| !v.is_null()) {
        match reason {
            Value::String(s) => write!(f, "\n  reason: {}", truncate_detail(s))?,
            other => write!(f, "\n  reason: {}", truncate_detail(&compact_json(other)))?,
        }
    }

    // `params[]` (400): per-field validation failures. Cap the number of
    // rendered entries so a pathological response can't flood stderr / MCP.
    if let Some(Value::Array(params)) = details.get("params") {
        for p in params.iter().take(MAX_RENDERED_PARAMS) {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
            let msg = p
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| p.get("kind").and_then(Value::as_str))
                .unwrap_or("invalid");
            // Both the field name and the message are untrusted server text;
            // bound each so a pathological name can't blow up the render either.
            write!(
                f,
                "\n  param {}: {}",
                truncate_detail(name),
                truncate_detail(msg)
            )?;
        }
        if params.len() > MAX_RENDERED_PARAMS {
            write!(f, "\n  … ({} more)", params.len() - MAX_RENDERED_PARAMS)?;
        }
    }

    // `validation_report` (422): structured template/schema report.
    if let Some(report) = details.get("validation_report").filter(|v| !v.is_null()) {
        write!(
            f,
            "\n  validation_report: {}",
            truncate_detail(&compact_json(report))
        )?;
    }

    // Doc + resource links, when present. Bounded like every other detail
    // value so an oversized server-supplied link can't blow up stderr / MCP.
    if let Some(url) = details.get("documentation_url").and_then(Value::as_str) {
        write!(f, "\n  see: {}", truncate_detail(url))?;
    }
    if let Some(res) = details.get("resource").and_then(Value::as_str) {
        write!(f, "\n  resource: {}", truncate_detail(res))?;
    }
    Ok(())
}

/// Maximum number of `params[]` entries rendered onto an [`ApiError`]'s
/// `Display`; further entries are summarized as `… (N more)` so a pathological
/// validation response cannot flood stderr or an MCP error payload.
const MAX_RENDERED_PARAMS: usize = 10;

/// Maximum rendered length of a single detail value (keeps stderr readable).
const DETAIL_MAX_LEN: usize = 400;

/// Serialize a JSON value compactly, falling back to its `Debug` form if
/// serialization somehow fails (it cannot for in-memory `Value`s, but the
/// no-`unwrap` rule forbids relying on that).
fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}

/// Truncate a detail string on a char boundary, appending an ellipsis marker
/// so the reader knows it was clipped.
fn truncate_detail(s: &str) -> std::borrow::Cow<'_, str> {
    if s.len() <= DETAIL_MAX_LEN {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut end = DETAIL_MAX_LEN;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!("{}… (truncated)", &s[..end]))
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_err(code: u32, http_status: u16) -> ApiError {
        ApiError::new(code, None, "boom".to_owned(), http_status)
    }

    #[test]
    fn suggestion_billing_codes_use_shared_hints() {
        assert_eq!(
            api_err(1688, 402).suggestion(),
            Some(HINT_SUBSCRIPTION_REQUIRED)
        );
        assert_eq!(api_err(1695, 402).suggestion(), Some(HINT_UPGRADE_REQUIRED));
        assert_eq!(api_err(1696, 402).suggestion(), Some(HINT_CREDIT_LIMIT));
    }

    #[test]
    fn suggestion_restricted_and_feature_limit_codes_use_generic_hints() {
        // Codes 1670/1685 are general-purpose in this API, so their hints must
        // be resource-agnostic and must NOT mention signing.
        assert_eq!(api_err(1670, 403).suggestion(), Some(HINT_RESTRICTED));
        assert_eq!(api_err(1685, 402).suggestion(), Some(HINT_FEATURE_LIMIT));
        assert!(!HINT_RESTRICTED.to_lowercase().contains("sign"));
        assert!(!HINT_FEATURE_LIMIT.to_lowercase().contains("sign"));
    }

    #[test]
    fn suggestion_unknown_route_9992_is_generic_non_signing() {
        // 9992 is a router-level "no such route" code; its hint must be
        // resource-agnostic and must NOT mention signing (that wording lives in
        // map_signing_error / the MCP sign_err_to_result, never here).
        assert_eq!(api_err(9992, 404).suggestion(), Some(HINT_UNKNOWN_ROUTE));
        assert!(!HINT_UNKNOWN_ROUTE.to_lowercase().contains("sign"));
    }

    #[test]
    fn suggestion_access_codes_override_generic_401_hint() {
        // 10545 (workspace membership) and 115069 (resource access) surface as
        // HTTP 401 but are NOT a missing login — the caller IS authenticated. The
        // rendered hint must steer to the access problem, never to `auth login`.
        // Both hints must also stay resource-agnostic (no "sign" wording — that
        // lives in map_signing_error / the MCP sign_err_to_result, never here).
        let m = api_err(10545, 401).suggestion().unwrap_or_default();
        assert_eq!(
            api_err(10545, 401).suggestion(),
            Some(HINT_WORKSPACE_MEMBERSHIP)
        );
        assert!(
            !m.to_lowercase().contains("auth login"),
            "10545 hint must not steer to auth login: {m}"
        );
        assert!(m.to_lowercase().contains("workspace"));

        let r = api_err(115_069, 401).suggestion().unwrap_or_default();
        assert_eq!(
            api_err(115_069, 401).suggestion(),
            Some(HINT_RESOURCE_ACCESS)
        );
        assert!(
            !r.to_lowercase().contains("auth login"),
            "115069 hint must not steer to auth login: {r}"
        );
        assert!(r.to_lowercase().contains("access"));

        assert!(!HINT_WORKSPACE_MEMBERSHIP.to_lowercase().contains("sign"));
        assert!(!HINT_RESOURCE_ACCESS.to_lowercase().contains("sign"));
    }

    #[test]
    fn billing_hints_reference_only_existing_commands() {
        // The hints must not reference commands that don't exist in the current
        // CLI surface. `billing usage` and `billing subscribe` BOTH landed in
        // Phase 7, so they are now valid targets; the deprecated `--plan-id`
        // flag (the canonical flag is `--plan`) must NOT appear.
        for hint in [
            HINT_SUBSCRIPTION_REQUIRED,
            HINT_UPGRADE_REQUIRED,
            HINT_CREDIT_LIMIT,
        ] {
            assert!(
                !hint.contains("--plan-id"),
                "hint references the removed `--plan-id` flag (use `--plan`): {hint}"
            );
        }
    }

    #[test]
    fn invalid_header_value_display_and_hint_are_resource_agnostic() {
        // The variant names ONLY the header (its whole purpose) and NEVER the
        // offending value (it is a secret). The Display deliberately carries the
        // header name `x-ve-password`, but neither the Display nor the hint may
        // carry FEATURE wording (fileshare / file share / sign) — so the variant
        // stays reusable for any header-bound secret. The hint additionally must
        // never name a concrete header or secret kind.
        let err = CliError::InvalidHeaderValue {
            header: "x-ve-password",
        };
        let rendered = err.to_string();
        assert_eq!(rendered, "invalid value for x-ve-password header");
        let hint = err.suggestion().unwrap_or_default();
        assert_eq!(hint, HINT_INVALID_HEADER_VALUE);
        // No FEATURE wording in either the Display or the hint.
        for needle in ["fileshare", "file share", "sign", "envelope"] {
            assert!(
                !rendered.to_lowercase().contains(needle),
                "InvalidHeaderValue Display must not carry resource wording ({needle}): {rendered}"
            );
            assert!(
                !hint.to_lowercase().contains(needle),
                "InvalidHeaderValue hint must not carry resource wording ({needle}): {hint}"
            );
        }
        // The hint stays generic: it names no specific header or secret kind.
        assert!(!hint.to_lowercase().contains("x-ve-password"));
        assert!(!hint.to_lowercase().contains("password"));
    }

    #[test]
    fn mapped_api_delegates_display_and_owns_hint() {
        // Display delegates to the inner ApiError verbatim (preserving the server
        // headline + code), so the exit-code-bearing message survives.
        let api = ApiError::new(1650, None, "boom".to_owned(), 401);
        let want_display = api.to_string();
        let mapped = CliError::MappedApi {
            api,
            hint: Some("use --password"),
        };
        assert_eq!(
            mapped.to_string(),
            want_display,
            "MappedApi Display must delegate to the inner ApiError"
        );
        // The hint is whatever the mapping layer supplied — NOT the inner
        // ApiError's generic 401 "auth login" default.
        assert_eq!(mapped.suggestion(), Some("use --password"));

        // `hint: None` SUPPRESSES the hint entirely (no generic fallback).
        let suppressed = CliError::MappedApi {
            api: ApiError::new(0, None, "boom".to_owned(), 404),
            hint: None,
        };
        assert_eq!(
            suppressed.suggestion(),
            None,
            "MappedApi with hint None must print no hint (no generic-404 fallback)"
        );
    }

    #[test]
    fn mapped_api_variant_carries_no_resource_wording() {
        // The variant itself must stay RESOURCE-AGNOSTIC: its Display delegates to
        // the inner (server-supplied) ApiError and it relays only the caller's
        // `hint`. With a plain server message and no hint, nothing fileshare /
        // share / sign specific may appear from the variant's own structure.
        let err = CliError::MappedApi {
            api: ApiError::new(1609, None, "not available".to_owned(), 404),
            hint: None,
        };
        let rendered = err.to_string();
        for needle in ["fileshare", "file share", "sign", "envelope"] {
            assert!(
                !rendered.to_lowercase().contains(needle),
                "MappedApi Display must not carry resource wording ({needle}): {rendered}"
            );
        }
    }

    #[test]
    fn version_conflict_display_and_hint_are_resource_agnostic() {
        // The conflict wording must be resource-AGNOSTIC ("the target file
        // changed") — never naming a feature — so the variant stays reusable for
        // any compare-and-swap write; the current version id is carried for the
        // rebase, but no fileshare/share/sign wording may appear.
        let err = CliError::VersionConflict {
            current_version: "v9xQ2-abc12".to_owned(),
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("v9xQ2-abc12"),
            "current version id must surface for the rebase: {rendered}"
        );
        let hint = err.suggestion().unwrap_or_default();
        assert_eq!(hint, HINT_VERSION_CONFLICT);
        for needle in ["fileshare", "file share", "share", "sign", "envelope"] {
            assert!(
                !rendered.to_lowercase().contains(needle),
                "VersionConflict Display must not carry resource wording ({needle}): {rendered}"
            );
            assert!(
                !hint.to_lowercase().contains(needle),
                "VersionConflict hint must not carry resource wording ({needle}): {hint}"
            );
        }
        // The CAS conflict hint must NOT collide with the Config hint (which
        // wrongly says "run fastio configure init" for this case).
        assert!(!hint.to_lowercase().contains("configure init"));
    }

    #[test]
    fn suggestion_bare_402_falls_back_to_subscription_hint() {
        // A 402 with an unrecognized code still steers to the plan surface.
        assert_eq!(
            api_err(0, 402).suggestion(),
            Some(HINT_SUBSCRIPTION_REQUIRED)
        );
    }

    #[test]
    fn suggestion_code_takes_precedence_over_http_status() {
        // 1695 maps to 402 server-side, but a 403-status response carrying
        // code 1695 must still yield the upgrade hint, not the 403 default.
        assert_eq!(api_err(1695, 403).suggestion(), Some(HINT_UPGRADE_REQUIRED));
    }

    #[test]
    fn suggestion_unverified_email_code_unchanged() {
        assert!(
            api_err(10587, 403)
                .suggestion()
                .is_some_and(|s| s.contains("auth verify"))
        );
    }

    #[test]
    fn suggestion_plain_status_arms_unchanged() {
        assert!(api_err(0, 401).suggestion().is_some());
        assert!(api_err(0, 404).suggestion().is_some());
        assert_eq!(api_err(0, 418).suggestion(), None);
    }

    #[test]
    fn details_round_trips_through_cli_error() {
        let details = serde_json::json!({"params": [{"name": "x"}]});
        let api = ApiError {
            code: 1660,
            error_code: None,
            message: "conflict".to_owned(),
            http_status: 409,
            details: Some(Box::new(details.clone())),
        };
        let cli: CliError = api.into();
        match cli {
            CliError::Api(e) => assert_eq!(e.details.as_deref(), Some(&details)),
            _ => panic!("expected CliError::Api"),
        }
    }

    #[test]
    fn new_constructor_defaults_details_none() {
        assert!(
            ApiError::new(0, None, "x".to_owned(), 500)
                .details
                .is_none()
        );
    }

    #[test]
    fn display_without_details_is_unchanged() {
        // The headline shape must be byte-stable when there are no details.
        let e = ApiError::new(1650, Some("APP_X".to_owned()), "boom".to_owned(), 400);
        assert_eq!(e.to_string(), "[HTTP 400] boom (code 1650) [APP_X]");
    }

    #[test]
    fn display_renders_422_validation_report() {
        // A 422 template-validation report must surface in Display so the CLI
        // (anyhow→Display→stderr) and MCP (to_string) both show it.
        let details = serde_json::json!({
            "validation_report": {"ok": false, "fields": ["name", "steps[0].kind"]},
        });
        let e = ApiError {
            code: 1665,
            error_code: None,
            message: "template invalid".to_owned(),
            http_status: 422,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(rendered.contains("validation_report:"), "got: {rendered}");
        assert!(rendered.contains("name"), "field name surfaced: {rendered}");
        assert!(
            rendered.contains("steps[0].kind"),
            "nested field surfaced: {rendered}"
        );
    }

    #[test]
    fn display_renders_409_reason() {
        // A trigger-fire 409 reason must surface in Display.
        let details = serde_json::json!({"reason": "dedup_hit"});
        let e = ApiError {
            code: 1660,
            error_code: None,
            message: "fire denied".to_owned(),
            http_status: 409,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(rendered.contains("reason: dedup_hit"), "got: {rendered}");
    }

    #[test]
    fn display_renders_400_params_and_links() {
        let details = serde_json::json!({
            "params": [
                {"name": "agent_credit_cap", "message": "must be a positive integer"},
                {"name": "visibility", "kind": "enum"},
            ],
            "documentation_url": "https://api.fast.io/docs",
            "resource": "workflow/123",
        });
        let e = ApiError {
            code: 1640,
            error_code: None,
            message: "bad request".to_owned(),
            http_status: 400,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(
            rendered.contains("param agent_credit_cap: must be a positive integer"),
            "got: {rendered}"
        );
        // A param without a message falls back to its kind.
        assert!(
            rendered.contains("param visibility: enum"),
            "got: {rendered}"
        );
        assert!(rendered.contains("see: https://api.fast.io/docs"));
        assert!(rendered.contains("resource: workflow/123"));
    }

    #[test]
    fn display_bounds_doc_url_resource_and_param_count() {
        let long_url = format!("https://api.fast.io/docs/{}", "u".repeat(1000));
        let long_resource = format!("workflow/{}", "r".repeat(1000));
        // 25 params — well past the MAX_RENDERED_PARAMS cap of 10.
        let params: Vec<_> = (0..25)
            .map(|i| serde_json::json!({"name": format!("field_{i}"), "message": "bad"}))
            .collect();
        let details = serde_json::json!({
            "documentation_url": long_url,
            "resource": long_resource,
            "params": params,
        });
        let e = ApiError {
            code: 1640,
            error_code: None,
            message: "bad request".to_owned(),
            http_status: 400,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();

        // The doc URL and resource are truncated, not emitted raw.
        assert!(
            rendered.contains("(truncated)"),
            "doc/resource must be bounded: {}",
            rendered.len()
        );
        assert!(
            !rendered.contains(&"u".repeat(1000)),
            "raw oversized doc URL leaked"
        );
        assert!(
            !rendered.contains(&"r".repeat(1000)),
            "raw oversized resource leaked"
        );
        // Only the first 10 params render, plus a "… (15 more)" note.
        assert!(rendered.contains("param field_0: bad"));
        assert!(rendered.contains("param field_9: bad"));
        assert!(
            !rendered.contains("param field_10:"),
            "params past the cap must not render: {rendered}"
        );
        assert!(
            rendered.contains("… (15 more)"),
            "must summarize the elided params: {rendered}"
        );
    }

    #[test]
    fn display_truncates_overlong_detail() {
        let long = "x".repeat(1000);
        let details = serde_json::json!({ "reason": long });
        let e = ApiError {
            code: 0,
            error_code: None,
            message: "m".to_owned(),
            http_status: 409,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(
            rendered.contains("(truncated)"),
            "got len {}",
            rendered.len()
        );
        assert!(
            rendered.len() < 600,
            "render stayed bounded: {}",
            rendered.len()
        );
    }
}
