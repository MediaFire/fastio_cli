/// Apps command implementations for `fastio apps *`.
///
/// Only `list` exists: the deployed app surface is `/user/apps/` plus
/// `install` / `uninstall` / `heartbeat`. The former `details`, `launch`, and
/// `tool-apps` subcommands called a `/apps/` prefix that has never existed and
/// have no route anywhere in the deployed tree, so they were removed rather
/// than re-pointed.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Apps subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AppsCommand {
    /// List the authenticated user's installed apps.
    List,
}

/// Execute an apps subcommand.
pub async fn execute(command: &AppsCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        AppsCommand::List => list(ctx).await,
    }
}

/// List the user's installed apps.
///
/// Authenticated: `/user/apps/` is a per-user list and rejects an anonymous
/// call. The previous implementation used an unauthenticated client against a
/// route that did not exist.
async fn list(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::apps::list_installed_apps(&client)
        .await
        .context("failed to list installed apps")?;
    ctx.output.render(&value)?;
    Ok(())
}
