/// Worklog command implementations for `fastio worklog *`.
///
/// Handles worklog listing, appending entries, and creating interjections.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Allowed entity types for worklog operations.
const VALID_ENTITY_TYPES: &[&str] = &["profile", "task", "task_list", "node"];

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

/// Validate the `--entry-type` filter against the worklog filter in use.
///
/// Per the contract (workflow.txt) only the `authored` filter honors
/// `?entry_type=`; other filters (e.g. `interjections`) ignore it server-side,
/// so passing it elsewhere is rejected rather than silently dropped.
fn validate_entry_type_filter(filter: &str, entry_type: Option<&str>) -> Result<()> {
    if entry_type.is_some() && filter != "authored" {
        anyhow::bail!("--entry-type is only valid with the `authored` filter");
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
    /// List all worklog entries in a workspace or share.
    ListAll {
        /// Profile type: workspace or share.
        profile_type: String,
        /// Workspace or share ID.
        profile_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Filtered worklog list (personal/group view).
    Filter {
        /// Profile type: workspace or share.
        profile_type: String,
        /// Workspace or share ID.
        profile_id: String,
        /// Filter: authored, interjections.
        filter: String,
        /// Entry type filter (authored only).
        entry_type: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Worklog entry summary for a workspace or share.
    Summary {
        /// Profile type: workspace or share.
        profile_type: String,
        /// Workspace or share ID.
        profile_id: String,
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
        WorklogCommand::ListAll {
            profile_type,
            profile_id,
            limit,
            offset,
        } => list_all(ctx, profile_type, profile_id, *limit, *offset).await,
        WorklogCommand::Filter {
            profile_type,
            profile_id,
            filter,
            entry_type,
            limit,
            offset,
        } => {
            filter_worklogs(
                ctx,
                profile_type,
                profile_id,
                filter,
                entry_type.as_deref(),
                *limit,
                *offset,
            )
            .await
        }
        WorklogCommand::Summary {
            profile_type,
            profile_id,
        } => summary(ctx, profile_type, profile_id).await,
    }
}

/// List all worklog entries in a workspace or share.
async fn list_all(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let query = api::workflow::FilterQuery {
        limit,
        offset,
        status: None,
        entry_type: None,
    };
    let value = api::workflow::list_worklogs_ctx(&client, profile_type, profile_id, &query)
        .await
        .context("failed to list worklog entries")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Filtered worklog list.
async fn filter_worklogs(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    filter: &str,
    entry_type: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_entry_type_filter(filter, entry_type)?;

    let client = ctx.build_client()?;
    let query = api::workflow::FilterQuery {
        limit,
        offset,
        status: None,
        entry_type,
    };
    let value =
        api::workflow::list_worklogs_filtered(&client, profile_type, profile_id, filter, &query)
            .await
            .context("failed to list filtered worklog entries")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Worklog entry summary.
async fn summary(ctx: &CommandContext<'_>, profile_type: &str, profile_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::worklogs_summary(&client, profile_type, profile_id)
        .await
        .context("failed to get worklog summary")?;
    ctx.output.render(&value)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{validate_entity_type, validate_entry_type_filter};

    #[test]
    fn entity_type_accepts_node() {
        // workflow.txt lists `node` as a valid worklog entity type.
        assert!(validate_entity_type("node").is_ok());
        assert!(validate_entity_type("profile").is_ok());
        assert!(validate_entity_type("task").is_ok());
        assert!(validate_entity_type("task_list").is_ok());
    }

    #[test]
    fn entity_type_rejects_unknown() {
        assert!(validate_entity_type("workspace").is_err());
    }

    #[test]
    fn entry_type_filter_allowed_only_with_authored() {
        // `authored` honors `?entry_type=`; the guard permits it there.
        assert!(validate_entry_type_filter("authored", Some("info")).is_ok());
        // No entry_type → always fine regardless of filter.
        assert!(validate_entry_type_filter("interjections", None).is_ok());
        // entry_type with a non-authored filter is rejected (not silently dropped).
        assert!(validate_entry_type_filter("interjections", Some("info")).is_err());
    }
}
