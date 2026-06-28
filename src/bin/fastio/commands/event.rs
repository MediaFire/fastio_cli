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
        /// Filter by acting user profile ID.
        user_id: Option<String>,
        /// Filter by organization ID.
        org_id: Option<String>,
        /// Filter by event name.
        event: Option<String>,
        /// Filter by category.
        category: Option<String>,
        /// Filter by subcategory.
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children.
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event.
        calling_user_id: Option<String>,
        /// Filter by related object ID.
        object_id: Option<String>,
        /// Audit-log read filter: `external_audit_log` or `external`.
        visibility: Option<String>,
        /// Filter by acknowledgment status.
        acknowledged: Option<bool>,
        /// Lower bound for event creation time.
        created_min: Option<String>,
        /// Upper bound for event creation time.
        created_max: Option<String>,
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
        /// Filter by acting user profile ID.
        user_id: Option<String>,
        /// Filter by organization ID.
        org_id: Option<String>,
        /// Filter by event name.
        event: Option<String>,
        /// Filter by category.
        category: Option<String>,
        /// Filter by subcategory.
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children.
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event.
        calling_user_id: Option<String>,
        /// Filter by related object ID.
        object_id: Option<String>,
        /// Audit-log read filter: `external_audit_log` or `external`.
        visibility: Option<String>,
        /// Filter by acknowledgment status.
        acknowledged: Option<bool>,
        /// Lower bound for event creation time.
        created_min: Option<String>,
        /// Upper bound for event creation time.
        created_max: Option<String>,
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
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            limit,
            offset,
        } => {
            let params = api::event::SearchEventsParams {
                workspace_id: workspace.as_deref(),
                share_id: share.as_deref(),
                user_id: user_id.as_deref(),
                org_id: org_id.as_deref(),
                event: event.as_deref(),
                category: category.as_deref(),
                subcategory: subcategory.as_deref(),
                parent_event_id: parent_event_id.as_deref(),
                calling_user_id: calling_user_id.as_deref(),
                object_id: object_id.as_deref(),
                visibility: visibility.as_deref(),
                acknowledged: *acknowledged,
                created_min: created_min.as_deref(),
                created_max: created_max.as_deref(),
                limit: *limit,
                offset: *offset,
            };
            list(ctx, &params).await
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
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            user_context,
            limit,
            offset,
        } => {
            let params = api::event::SummarizeEventsParams {
                workspace_id: workspace.as_deref(),
                share_id: share.as_deref(),
                user_id: user_id.as_deref(),
                org_id: org_id.as_deref(),
                event: event.as_deref(),
                category: category.as_deref(),
                subcategory: subcategory.as_deref(),
                parent_event_id: parent_event_id.as_deref(),
                calling_user_id: calling_user_id.as_deref(),
                object_id: object_id.as_deref(),
                visibility: visibility.as_deref(),
                acknowledged: *acknowledged,
                created_min: created_min.as_deref(),
                created_max: created_max.as_deref(),
                user_context: user_context.as_deref(),
                limit: *limit,
                offset: *offset,
            };
            summarize(ctx, &params).await
        }
    }
}

/// List/search events.
async fn list(ctx: &CommandContext<'_>, params: &api::event::SearchEventsParams<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::search_events(&client, params)
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
async fn summarize(
    ctx: &CommandContext<'_>,
    params: &api::event::SummarizeEventsParams<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::summarize_events(&client, params)
        .await
        .context("failed to summarize events")?;
    ctx.output.render(&value)?;
    Ok(())
}
