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
    pub fn build_client(&self) -> anyhow::Result<fastio_cli::client::ApiClient> {
        build_client(
            self.api_base,
            self.profile_name,
            self.flag_token,
            self.config_dir,
        )
    }
}

/// Resolve authentication and build an API client.
///
/// This is the shared helper used by every command module that needs an
/// authenticated HTTP client.
pub fn build_client(
    api_base: &str,
    profile_name: &str,
    flag_token: Option<&str>,
    config_dir: &Path,
) -> anyhow::Result<fastio_cli::client::ApiClient> {
    let resolved = fastio_cli::auth::token::resolve_token(flag_token, profile_name, config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    fastio_cli::client::ApiClient::new(api_base, Some(t)).context("failed to create API client")
}

/// AI chat and prompt commands.
pub mod ai;
/// Approval workflow commands.
pub mod approval;
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
/// File and folder download commands.
pub mod download;
/// Audit and activity event commands.
pub mod event;
/// File and folder management commands.
pub mod files;
/// External storage import commands.
pub mod import;
/// AI instructions commands (user / org / workspace / share).
pub mod instructions;
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
/// Share link management commands.
pub mod share;
/// System health and status commands.
pub mod system;
/// Task management commands.
pub mod task;
/// To-do item commands.
pub mod todo;
/// File upload commands.
pub mod upload;
/// User profile commands.
pub mod user;
/// Work log commands.
pub mod worklog;
/// Workspace management commands.
pub mod workspace;
