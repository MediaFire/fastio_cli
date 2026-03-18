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

/// An error returned by the Fast.io REST API.
#[derive(Debug)]
pub struct ApiError {
    /// Numeric API error code (e.g. 1650).
    pub code: u32,
    /// Machine-readable error identifier (e.g. `APP_AUTH_INVALID`).
    pub error_code: Option<String>,
    /// Human-readable error message.
    pub message: String,
    /// HTTP status code of the response.
    pub http_status: u16,
}

impl ApiError {
    /// Return a human-readable suggestion based on the HTTP status or error code.
    #[must_use]
    pub fn suggestion(&self) -> Option<&'static str> {
        // Check specific error codes before falling back to HTTP status.
        if self.code == 10587 {
            return Some(
                "Account email not verified. Run `fastio auth verify --email <your-email>` to resend the verification email.",
            );
        }
        match self.http_status {
            401 => Some("Authentication failed. Run `fastio auth login` to sign in."),
            403 => Some("Permission denied. Check that your account has the required role."),
            404 => Some("Resource not found. Verify the ID or path is correct."),
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
