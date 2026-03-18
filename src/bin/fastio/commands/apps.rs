/// Apps command implementations for `fastio apps *`.
///
/// Handles app/widget discovery, metadata, and launch operations.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::client::ApiClient;

/// Apps subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AppsCommand {
    /// List all available apps.
    List,
    /// Get details for a specific app.
    Details {
        /// App identifier.
        app_id: String,
    },
    /// Launch an app in a context.
    Launch {
        /// App identifier.
        app_id: String,
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
    },
    /// List apps available for a specific tool.
    GetToolApps {
        /// Tool name.
        tool_name: String,
    },
}

/// Execute an apps subcommand.
pub async fn execute(command: &AppsCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        AppsCommand::List => list(ctx).await,
        AppsCommand::Details { app_id } => details(ctx, app_id).await,
        AppsCommand::Launch {
            app_id,
            context_type,
            context_id,
        } => launch(ctx, app_id, context_type, context_id).await,
        AppsCommand::GetToolApps { tool_name } => get_tool_apps(ctx, tool_name).await,
    }
}

/// Build an unauthenticated client.
fn build_client_unauth(api_base: &str) -> Result<ApiClient> {
    ApiClient::new(api_base, None::<String>).context("failed to create API client")
}

/// List available apps.
async fn list(ctx: &CommandContext<'_>) -> Result<()> {
    let client = build_client_unauth(ctx.api_base)?;
    let value = api::apps::list_apps(&client)
        .await
        .context("failed to list apps")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get app details.
async fn details(ctx: &CommandContext<'_>, app_id: &str) -> Result<()> {
    let client = build_client_unauth(ctx.api_base)?;
    let value = api::apps::app_details(&client, app_id)
        .await
        .context("failed to get app details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Launch an app.
async fn launch(
    ctx: &CommandContext<'_>,
    app_id: &str,
    context_type: &str,
    context_id: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::apps::launch_app(&client, app_id, context_type, context_id)
        .await
        .context("failed to launch app")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List apps for a tool.
async fn get_tool_apps(ctx: &CommandContext<'_>, tool_name: &str) -> Result<()> {
    let client = build_client_unauth(ctx.api_base)?;
    let value = api::apps::get_tool_apps(&client, tool_name)
        .await
        .context("failed to get tool apps")?;
    ctx.output.render(&value)?;
    Ok(())
}
