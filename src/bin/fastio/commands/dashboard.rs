//! Dashboard command implementations for `fastio dashboard *`.
//!
//! Thin command surface over [`fastio_cli::api::dashboard`]: read the calling
//! member's per-workspace actionable card feed, and dismiss / snooze /
//! undismiss a card. Dismiss and snooze are per-member and out-of-band — they
//! only hide a card from the caller's own feed.

use anyhow::{Context, Result};

use crate::cli::DashboardCommands;
use fastio_cli::api;

use super::CommandContext;

/// Execute a dashboard subcommand.
pub async fn execute(command: DashboardCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        DashboardCommands::Get {
            workspace,
            limit,
            offset,
        } => get(ctx, &workspace, limit, offset).await,
        DashboardCommands::Dismiss {
            card_key,
            workspace,
            snooze_until,
        } => dismiss(ctx, &workspace, &card_key, snooze_until.as_deref()).await,
        DashboardCommands::Undismiss {
            card_key,
            workspace,
        } => undismiss(ctx, &workspace, &card_key).await,
    }
}

/// Get the calling member's dashboard card feed for a workspace.
async fn get(
    ctx: &CommandContext<'_>,
    workspace: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::dashboard::get_dashboard(&client, workspace, limit, offset)
        .await
        .context("failed to get dashboard")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Dismiss or snooze a dashboard card.
async fn dismiss(
    ctx: &CommandContext<'_>,
    workspace: &str,
    card_key: &str,
    snooze_until: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::dashboard::dismiss_card(&client, workspace, card_key, snooze_until)
        .await
        .context("failed to dismiss dashboard card")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Undismiss a dashboard card, restoring it to the caller's feed.
async fn undismiss(ctx: &CommandContext<'_>, workspace: &str, card_key: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::dashboard::undismiss_card(&client, workspace, card_key)
        .await
        .context("failed to undismiss dashboard card")?;
    ctx.output.render(&value)?;
    Ok(())
}
