/// Event command implementations for `fastio event *`.
///
/// Handles event listing, details, and activity polling.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Event subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EventCommand {
    /// List/search events.
    List {
        /// Filter by workspace ID.
        workspace: Option<String>,
        /// Filter by share ID.
        share: Option<String>,
        /// Filter by event name.
        event: Option<String>,
        /// Filter by category.
        category: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Get event details.
    Info {
        /// Event ID.
        event_id: String,
    },
    /// Long-poll for activity updates.
    Poll {
        /// Workspace or share ID.
        entity_id: String,
        /// Last activity timestamp.
        lastactivity: Option<String>,
        /// Max wait time in seconds.
        wait: Option<u32>,
    },
    /// Acknowledge an event.
    Ack {
        /// Event ID to acknowledge.
        event_id: String,
    },
    /// AI-powered event summary.
    Summarize {
        /// Filter by workspace ID.
        workspace: Option<String>,
        /// Filter by share ID.
        share: Option<String>,
        /// Filter by event name.
        event: Option<String>,
        /// Filter by category.
        category: Option<String>,
        /// Filter by subcategory.
        subcategory: Option<String>,
        /// Free-text context for the AI summarizer.
        user_context: Option<String>,
        /// Max events to include.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
}

/// Execute an event subcommand.
pub async fn execute(command: &EventCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        EventCommand::List {
            workspace,
            share,
            event,
            category,
            limit,
            offset,
        } => {
            list(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                event.as_deref(),
                category.as_deref(),
                *limit,
                *offset,
            )
            .await
        }
        EventCommand::Info { event_id } => info(ctx, event_id).await,
        EventCommand::Poll {
            entity_id,
            lastactivity,
            wait,
        } => poll(ctx, entity_id, lastactivity.as_deref(), *wait).await,
        EventCommand::Ack { event_id } => ack(ctx, event_id).await,
        EventCommand::Summarize {
            workspace,
            share,
            event,
            category,
            subcategory,
            user_context,
            limit,
            offset,
        } => {
            summarize(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                event.as_deref(),
                category.as_deref(),
                subcategory.as_deref(),
                user_context.as_deref(),
                *limit,
                *offset,
            )
            .await
        }
    }
}

/// List/search events.
async fn list(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    event: Option<&str>,
    category: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::search_events(
        &client,
        &api::event::SearchEventsParams {
            workspace_id: workspace,
            share_id: share,
            event,
            category,
            limit,
            offset,
            ..Default::default()
        },
    )
    .await
    .context("failed to search events")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get event details.
async fn info(ctx: &CommandContext<'_>, event_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::get_event_details(&client, event_id)
        .await
        .context("failed to get event details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Long-poll for activity updates.
async fn poll(
    ctx: &CommandContext<'_>,
    entity_id: &str,
    lastactivity: Option<&str>,
    wait: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::poll_activity(&client, entity_id, lastactivity, wait, false)
        .await
        .context("failed to poll activity")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Acknowledge an event.
async fn ack(ctx: &CommandContext<'_>, event_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::acknowledge_event(&client, event_id)
        .await
        .context("failed to acknowledge event")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get an AI-powered summary of events.
#[allow(clippy::too_many_arguments)]
async fn summarize(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    event: Option<&str>,
    category: Option<&str>,
    subcategory: Option<&str>,
    user_context: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::summarize_events(
        &client,
        &api::event::SummarizeEventsParams {
            workspace_id: workspace,
            share_id: share,
            event,
            category,
            subcategory,
            user_context,
            limit,
            offset,
            ..Default::default()
        },
    )
    .await
    .context("failed to summarize events")?;
    ctx.output.render(&value)?;
    Ok(())
}
