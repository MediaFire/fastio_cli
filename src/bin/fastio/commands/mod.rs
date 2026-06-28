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
/// object arguments (e.g. `comment create`'s `--reference` / `--properties` and
/// `task comment post`), so the resolver stays in one place rather than being
/// re-implemented per module. Returns `Ok(None)` when `raw` is absent.
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
/// Task management commands.
pub mod task;
/// File upload commands.
pub mod upload;
/// User profile commands.
pub mod user;
/// Terminal markdown viewer command (`fastio view`).
pub mod view;
/// Workflow Orchestration (v3.2) commands.
pub mod workflow;
/// Workspace management commands.
pub mod workspace;
