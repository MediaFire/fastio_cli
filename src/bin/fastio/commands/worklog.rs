/// Worklog command implementations for `fastio worklog *`.
///
/// Handles worklog listing, appending entries, and creating interjections.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Allowed entity types for worklog operations.
const VALID_ENTITY_TYPES: &[&str] = &["profile", "task", "task_list"];

/// Validate that an entity type is one of the known values.
fn validate_entity_type(entity_type: &str) -> Result<()> {
    if !VALID_ENTITY_TYPES.contains(&entity_type) {
        anyhow::bail!(
            "invalid entity type '{}'. Must be one of: {}",
            entity_type,
            VALID_ENTITY_TYPES.join(", ")
        );
    }
    Ok(())
}

/// Worklog subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum WorklogCommand {
    /// List worklog entries.
    List {
        /// Workspace ID.
        workspace: String,
        /// Entity type (profile, task, or `task_list`).
        entity_type: Option<String>,
        /// Entity ID.
        entity_id: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Append a worklog entry.
    Append {
        /// Workspace ID.
        workspace: String,
        /// Message content.
        message: String,
        /// Entity type (defaults to "profile").
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        entity_id: Option<String>,
    },
    /// Create an interjection (urgent entry requiring acknowledgement).
    Interject {
        /// Workspace ID.
        workspace: String,
        /// Message content.
        message: String,
        /// Entity type (defaults to "profile").
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        entity_id: Option<String>,
    },
    /// Get worklog entry details.
    Details {
        /// Worklog entry ID.
        entry_id: String,
    },
    /// List unacknowledged interjections for an entity.
    ListInterjections {
        /// Entity type (profile, task, or `task_list`).
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Acknowledge a worklog interjection.
    Acknowledge {
        /// Worklog entry ID to acknowledge.
        entry_id: String,
    },
}

/// Execute a worklog subcommand.
pub async fn execute(command: &WorklogCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        WorklogCommand::List {
            workspace,
            entity_type,
            entity_id,
            limit,
            offset,
        } => {
            let etype = entity_type.as_deref().unwrap_or("profile");
            let eid = entity_id.as_deref().unwrap_or(workspace.as_str());
            list(ctx, etype, eid, *limit, *offset).await
        }
        WorklogCommand::Append {
            workspace,
            message,
            entity_type,
            entity_id,
        } => {
            let etype = entity_type.as_deref().unwrap_or("profile");
            let eid = entity_id.as_deref().unwrap_or(workspace.as_str());
            append(ctx, etype, eid, message).await
        }
        WorklogCommand::Interject {
            workspace,
            message,
            entity_type,
            entity_id,
        } => {
            let etype = entity_type.as_deref().unwrap_or("profile");
            let eid = entity_id.as_deref().unwrap_or(workspace.as_str());
            interject(ctx, etype, eid, message).await
        }
        WorklogCommand::Details { entry_id } => details(ctx, entry_id).await,
        WorklogCommand::ListInterjections {
            entity_type,
            entity_id,
            limit,
            offset,
        } => list_interjections(ctx, entity_type, entity_id, *limit, *offset).await,
        WorklogCommand::Acknowledge { entry_id } => acknowledge(ctx, entry_id).await,
    }
}

/// List worklog entries.
async fn list(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_entity_type(entity_type)?;

    let client = ctx.build_client()?;
    let value = api::workflow::list_worklogs(&client, entity_type, entity_id, limit, offset)
        .await
        .context("failed to list worklog entries")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Append a worklog entry.
async fn append(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    content: &str,
) -> Result<()> {
    validate_entity_type(entity_type)?;

    if content.is_empty() {
        anyhow::bail!("worklog message must not be empty");
    }

    let client = ctx.build_client()?;
    let value = api::workflow::append_worklog(&client, entity_type, entity_id, content)
        .await
        .context("failed to append worklog entry")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a worklog interjection.
async fn interject(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    content: &str,
) -> Result<()> {
    validate_entity_type(entity_type)?;

    if content.is_empty() {
        anyhow::bail!("interjection message must not be empty");
    }

    let client = ctx.build_client()?;
    let value = api::workflow::interject_worklog(&client, entity_type, entity_id, content)
        .await
        .context("failed to create worklog interjection")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get worklog entry details.
async fn details(ctx: &CommandContext<'_>, entry_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::worklog_details(&client, entry_id)
        .await
        .context("failed to get worklog entry details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List unacknowledged interjections for an entity.
async fn list_interjections(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_entity_type(entity_type)?;

    let client = ctx.build_client()?;
    let value =
        api::workflow::unacknowledged_worklogs(&client, entity_type, entity_id, limit, offset)
            .await
            .context("failed to list unacknowledged interjections")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Acknowledge a worklog interjection.
async fn acknowledge(ctx: &CommandContext<'_>, entry_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::acknowledge_worklog(&client, entry_id)
        .await
        .context("failed to acknowledge worklog interjection")?;
    ctx.output.render(&value)?;
    Ok(())
}
