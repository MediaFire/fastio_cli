//! Command implementations for the Fast.io CLI.
//!
//! Each sub-module handles a top-level command group (e.g., `auth`, `user`).

use std::path::Path;

use anyhow::Context;

use fastio_cli::output::OutputConfig;

/// Common parameters shared by all authenticated command handlers.
pub struct CommandContext<'a> {
    pub output: &'a OutputConfig,
    pub profile_name: &'a str,
    pub api_base: &'a str,
    pub flag_token: Option<&'a str>,
    pub config_dir: &'a Path,
}

impl CommandContext<'_> {
    /// Resolve authentication and build an API client.
    ///
    /// The client inherits the `--detail` server-verbosity level from the
    /// active [`OutputConfig`], so allowlisted envelope GETs append
    /// `?output=<detail>` automatically.
    pub fn build_client(&self) -> anyhow::Result<fastio_cli::client::ApiClient> {
        build_client_with_detail(
            self.api_base,
            self.profile_name,
            self.flag_token,
            self.config_dir,
            self.output.detail,
        )
    }
}

/// Resolve authentication and build an API client with an explicit
/// server-verbosity [`fastio_cli::output::OutputDetail`].
///
/// This is the shared helper used by every command module that needs an
/// authenticated HTTP client. Prefer [`CommandContext::build_client`], which
/// threads the active `--detail` automatically; call this directly only when
/// you have a detail level outside a [`CommandContext`].
pub fn build_client_with_detail(
    api_base: &str,
    profile_name: &str,
    flag_token: Option<&str>,
    config_dir: &Path,
    detail: Option<fastio_cli::output::OutputDetail>,
) -> anyhow::Result<fastio_cli::client::ApiClient> {
    let resolved = fastio_cli::auth::token::resolve_token(flag_token, profile_name, config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    fastio_cli::client::ApiClient::with_detail(api_base, Some(t), detail)
        .context("failed to create API client")
}

impl CommandContext<'_> {
    /// Build an API client that tolerates the absence of authentication, for
    /// File Share **consumption** reads (details / download / versions /
    /// preview) which may be served anonymously per the share's access tier.
    ///
    /// Auth resolution (File Share addendum F5):
    ///
    /// - A resolved token (`--token` / env / a live profile) → an authenticated
    ///   client, exactly as [`Self::build_client`].
    /// - No credentials at all (`resolve_token` → `Ok(None)`) → an ANONYMOUS
    ///   client (no bearer). An `anyone_with_link` share still serves.
    /// - EXPIRED stored PROFILE credentials (`resolve_token` →
    ///   `Err(CliError::Auth)`) → fall back to an anonymous client with a single
    ///   stderr warning (suppressed under `--quiet`). The user explicitly asked
    ///   to read a public link; an expired stored token should not hard-block a
    ///   read that may not need auth at all.
    ///
    /// An EXPLICIT `--token` / env token failure can only arrive here as an
    /// authenticated client (those are returned by `resolve_token` as
    /// `Ok(Some)` before any expiry check), so this never silently downgrades an
    /// explicit credential. Management, upload, ws-token, and activity stay on
    /// the always-authed [`Self::build_client`].
    pub fn build_client_allow_anonymous(&self) -> anyhow::Result<fastio_cli::client::ApiClient> {
        let token = match fastio_cli::auth::token::resolve_token(
            self.flag_token,
            self.profile_name,
            self.config_dir,
        ) {
            Ok(token) => token,
            Err(fastio_cli::error::CliError::Auth(_)) => {
                // Expired PROFILE credentials: proceed anonymously (the link may
                // be public). Warn once so the user knows the read was not
                // authenticated.
                if !self.output.quiet {
                    eprintln!(
                        "warning: stored credentials expired — proceeding without \
                         authentication (the File Share may require a link password or a \
                         grant; run `fastio auth login` to authenticate)"
                    );
                }
                None
            }
            Err(other) => {
                return Err(anyhow::Error::from(other).context("failed to resolve token"));
            }
        };
        fastio_cli::client::ApiClient::with_detail(self.api_base, token, self.output.detail)
            .context("failed to create API client")
    }
}

/// Parse an optional JSON-object argument, supporting an `@path` form that reads
/// the JSON from a file (`@@` escapes a literal leading `@`).
///
/// Shared by the command modules that accept inline `{json}` or `@file.json`
/// object arguments (e.g. `comment create`'s `--reference` / `--properties`), so
/// the resolver stays in one place rather than being re-implemented per module.
/// Returns `Ok(None)` when `raw` is absent.
pub(crate) fn parse_json_object_arg(
    raw: Option<&str>,
    label: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let Some(raw) = raw else { return Ok(None) };
    let text = if let Some(path) = raw.strip_prefix('@') {
        if let Some(literal) = path.strip_prefix('@') {
            literal.to_owned()
        } else {
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {label} from file '{path}'"))?
        }
    } else {
        raw.to_owned()
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("{label} must be valid JSON"))?;
    anyhow::ensure!(
        value.is_object(),
        "{label} must be a JSON object (e.g. {{\"key\":\"value\"}})"
    );
    Ok(Some(value))
}

/// How a poll loop should react to an error from one poll tick.
///
/// Distinguishes the three cases the previous `Err(_) => {}` collapsed into one
/// (silent loop-to-timeout):
/// - [`PollAction::RateLimited`] — honor the server's `retry_after`;
/// - [`PollAction::RetryTransient`] — a 5xx / network / I/O blip; back off and
///   retry on the next tick;
/// - [`PollAction::Fatal`] — a persistent, non-transient error (404 / 403 /
///   400 / parse / a non-rate-limit 4xx). Surface it instead of looping.
///
/// `pub(crate)` so the Ripley `ask`/`chat` and metadata `extract --wait` poll
/// loops (CLI + MCP) reuse the SAME classification rather than each
/// re-collapsing every error into a silent timeout.
pub(crate) enum PollAction {
    /// Server asked us to wait this many seconds before the next request.
    RateLimited { retry_after_secs: u64 },
    /// A transient failure worth one more poll on the regular cadence.
    RetryTransient,
    /// A persistent error the caller should see now (returned, not swallowed).
    Fatal(fastio_cli::error::CliError),
}

/// Classify a poll-tick [`CliError`] into a [`PollAction`].
///
/// The 401 re-auth short-circuit is handled by the caller before this is
/// reached. Rate limits sleep their advertised interval; all 5xx (`500..=599`),
/// request timeouts, transport, and I/O errors are transient; everything else
/// (4xx other than 408/429, parse, config) is fatal so a 404/403 no longer
/// loops silently to the deadline.
///
/// `pub(crate)` so the Ripley/metadata wait loops share this exact policy.
pub(crate) fn classify_poll_error(err: fastio_cli::error::CliError) -> PollAction {
    use fastio_cli::error::CliError;
    match err {
        CliError::RateLimit { retry_after_secs } => PollAction::RateLimited { retry_after_secs },
        CliError::Api(ref e) => match e.http_status {
            429 | 408 => PollAction::RateLimited {
                retry_after_secs: 0,
            },
            // All server errors are transient — a 500 during a long-running
            // poll is typically a momentary backend blip, not a permanent
            // condition, so it's worth another tick.
            500..=599 => PollAction::RetryTransient,
            _ => PollAction::Fatal(err),
        },
        // Transport/timeout and transient I/O are worth another tick.
        CliError::Http(_) | CliError::Io(_) => PollAction::RetryTransient,
        // Parse / config / auth(other) — and, conservatively, any future
        // non-exhaustive variant — are surfaced rather than looped.
        _ => PollAction::Fatal(err),
    }
}

/// AI chat and prompt commands.
pub mod ai;
/// Connected-app management commands.
pub mod apps;
/// Asset metadata and transformation commands.
pub mod asset;
/// Authentication commands (login, logout, status).
pub mod auth;
/// File and folder comment commands.
pub mod comment;
/// CLI configuration commands (profiles, defaults).
pub mod configure;
/// Per-workspace dashboard (actionable card feed) commands.
pub mod dashboard;
/// File and folder download commands.
pub mod download;
/// Audit and activity event commands.
pub mod event;
/// File and folder management commands.
pub mod files;
/// File Share (durable single-file link) commands.
pub mod fileshare;
/// How-To grounded product-guidance command (`fastio how-to`).
pub mod howto;
/// Offline OpaqueId classification command (`fastio id info`).
pub mod id;
/// External storage import commands.
pub mod import;
/// Workspace invitation commands.
pub mod invitation;
/// File locking commands.
pub mod lock;
/// Organization and workspace member commands.
pub mod member;
/// Metadata extraction and template management commands.
pub mod metadata;
/// Organization management commands.
pub mod org;
/// File preview commands.
pub mod preview;
/// Unified (grouped-bucket) search commands.
pub mod search;
/// Shared one-time-secret output helpers (extract / write 0600 / redact).
pub mod secret_output;
/// Share link management commands.
pub mod share;
/// E-signature (SignEnvelope) commands.
pub mod sign;
/// System health and status commands.
pub mod system;
/// File upload commands.
pub mod upload;
/// User profile commands.
pub mod user;
/// Terminal markdown viewer command (`fastio view`).
pub mod view;
/// Workspace management commands.
pub mod workspace;

#[cfg(test)]
mod tests {
    use super::{PollAction, classify_poll_error};
    use fastio_cli::error::CliError;

    fn api_err(http_status: u16) -> CliError {
        CliError::Api(fastio_cli::error::ApiError::new(
            0,
            None,
            "boom".to_owned(),
            http_status,
        ))
    }

    #[test]
    fn classify_poll_error_rate_limit_uses_retry_after() {
        match classify_poll_error(CliError::RateLimit {
            retry_after_secs: 12,
        }) {
            PollAction::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, 12),
            _ => panic!("rate limit must map to RateLimited"),
        }
        // A 429/408 Api error is also rate-limit-like.
        assert!(matches!(
            classify_poll_error(api_err(429)),
            PollAction::RateLimited { .. }
        ));
        assert!(matches!(
            classify_poll_error(api_err(408)),
            PollAction::RateLimited { .. }
        ));
    }

    #[test]
    fn classify_poll_error_5xx_transient_4xx_fatal() {
        // All 5xx (including 500) are transient — a momentary backend blip.
        for s in [500u16, 502, 503, 504, 599] {
            assert!(
                matches!(classify_poll_error(api_err(s)), PollAction::RetryTransient),
                "{s} should be transient"
            );
        }
        // Persistent client errors must be surfaced, not looped.
        for s in [400u16, 403, 404] {
            assert!(
                matches!(classify_poll_error(api_err(s)), PollAction::Fatal(_)),
                "{s} should be fatal"
            );
        }
        // Parse errors are fatal.
        assert!(matches!(
            classify_poll_error(CliError::Parse("x".to_owned())),
            PollAction::Fatal(_)
        ));
    }
}
