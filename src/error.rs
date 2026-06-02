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
/// current CLI surface (`org billing plans`); subscription creation is via
/// `org billing create`, but the onboarding URL is the canonical recovery path.
pub const HINT_SUBSCRIPTION_REQUIRED: &str = "No active paid plan or credits exhausted. \
     Run `fastio org billing plans` to see options, then `fastio org billing create <org> --plan-id <id>`, \
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
/// this API (workspace/share/upload/QuickShare limits, workflow availability,
/// etc.), so this hint is deliberately resource-agnostic.
pub const HINT_FEATURE_LIMIT: &str =
    "A plan or feature limit was reached for this operation; a higher plan tier may be required.";

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
        Ok(())
    }
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
    fn billing_hints_reference_only_existing_commands() {
        // The hints must not reference commands that don't exist in the current
        // CLI surface. `subscribe` and `usage` are planned-but-absent today.
        for hint in [
            HINT_SUBSCRIPTION_REQUIRED,
            HINT_UPGRADE_REQUIRED,
            HINT_CREDIT_LIMIT,
        ] {
            assert!(
                !hint.contains("billing subscribe"),
                "hint references non-existent `billing subscribe`: {hint}"
            );
            assert!(
                !hint.contains("billing usage"),
                "hint references non-existent `billing usage`: {hint}"
            );
        }
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
}
