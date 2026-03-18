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
