/// System health command implementations for `fastio system *`.
///
/// Handles health check and system status endpoints that do not require
/// authentication.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::client::ApiClient;

/// System subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SystemCommand {
    /// Health check (ping).
    Ping,
    /// System status.
    Status,
}

/// Execute a system subcommand.
pub async fn execute(command: &SystemCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        SystemCommand::Ping => ping(ctx).await,
        SystemCommand::Status => status(ctx).await,
    }
}

/// Build an unauthenticated API client for system health endpoints.
fn build_unauthed_client(api_base: &str) -> anyhow::Result<ApiClient> {
    ApiClient::new(api_base, None).context("failed to create API client")
}

/// Health check (ping).
async fn ping(ctx: &CommandContext<'_>) -> Result<()> {
    let client = build_unauthed_client(ctx.api_base)?;
    let value = api::system::ping(&client)
        .await
        .context("health check failed")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// System status.
async fn status(ctx: &CommandContext<'_>) -> Result<()> {
    let client = build_unauthed_client(ctx.api_base)?;
    let value = api::system::system_status(&client)
        .await
        .context("failed to get system status")?;
    ctx.output.render(&value)?;
    Ok(())
}
