/// Todo command implementations for `fastio todo *`.
///
/// Handles todo listing, creation, updating, toggling, and deletion.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Todo subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TodoCommand {
    /// List todos in a workspace or share.
    List {
        /// Profile type ("workspace" or "share").
        profile_type: String,
        /// Profile ID (workspace or share ID).
        profile_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create a new todo in a workspace or share.
    Create {
        /// Profile type ("workspace" or "share").
        profile_type: String,
        /// Profile ID (workspace or share ID).
        profile_id: String,
        /// Todo title.
        title: String,
        /// Assignee profile ID.
        assignee_id: Option<String>,
    },
    /// Update a todo.
    Update {
        /// Todo ID.
        todo_id: String,
        /// New title.
        title: Option<String>,
        /// New done state.
        done: Option<bool>,
        /// New assignee.
        assignee_id: Option<String>,
    },
    /// Toggle a todo's completion state.
    Toggle {
        /// Todo ID.
        todo_id: String,
    },
    /// Delete a todo.
    Delete {
        /// Todo ID.
        todo_id: String,
    },
    /// Bulk toggle todo completion in a workspace or share.
    BulkToggle {
        /// Profile type ("workspace" or "share").
        profile_type: String,
        /// Profile ID (workspace or share ID).
        profile_id: String,
        /// Todo IDs to toggle.
        todo_ids: Vec<String>,
        /// Completion state to set.
        done: bool,
    },
}

/// Execute a todo subcommand.
pub async fn execute(command: &TodoCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        TodoCommand::List {
            profile_type,
            profile_id,
            limit,
            offset,
        } => list(ctx, profile_type, profile_id, *limit, *offset).await,
        TodoCommand::Create {
            profile_type,
            profile_id,
            title,
            assignee_id,
        } => create(ctx, profile_type, profile_id, title, assignee_id.as_deref()).await,
        TodoCommand::Update {
            todo_id,
            title,
            done,
            assignee_id,
        } => {
            update(
                ctx,
                todo_id,
                title.as_deref(),
                *done,
                assignee_id.as_deref(),
            )
            .await
        }
        TodoCommand::Toggle { todo_id } => toggle(ctx, todo_id).await,
        TodoCommand::Delete { todo_id } => delete(ctx, todo_id).await,
        TodoCommand::BulkToggle {
            profile_type,
            profile_id,
            todo_ids,
            done,
        } => bulk_toggle(ctx, profile_type, profile_id, todo_ids, *done).await,
    }
}

/// List todos in a workspace or share.
async fn list(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::list_todos_ctx(&client, profile_type, profile_id, limit, offset)
        .await
        .context("failed to list todos")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a todo in a workspace or share.
async fn create(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    title: &str,
    assignee_id: Option<&str>,
) -> Result<()> {
    if title.is_empty() {
        anyhow::bail!("todo title must not be empty");
    }

    let client = ctx.build_client()?;
    let value =
        api::workflow::create_todo_ctx(&client, profile_type, profile_id, title, assignee_id)
            .await
            .context("failed to create todo")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Update a todo.
async fn update(
    ctx: &CommandContext<'_>,
    todo_id: &str,
    title: Option<&str>,
    done: Option<bool>,
    assignee_id: Option<&str>,
) -> Result<()> {
    if title.is_none() && done.is_none() && assignee_id.is_none() {
        anyhow::bail!("at least one update field is required (--title, --done, --assignee-id)");
    }

    if let Some(t) = title
        && t.is_empty()
    {
        anyhow::bail!("todo title must not be empty");
    }

    let client = ctx.build_client()?;
    let value = api::workflow::update_todo(&client, todo_id, title, done, assignee_id)
        .await
        .context("failed to update todo")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Toggle a todo's completion state.
async fn toggle(ctx: &CommandContext<'_>, todo_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::toggle_todo(&client, todo_id)
        .await
        .context("failed to toggle todo")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a todo.
async fn delete(ctx: &CommandContext<'_>, todo_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::workflow::delete_todo(&client, todo_id)
        .await
        .context("failed to delete todo")?;
    let value = json!({
        "status": "deleted",
        "todo_id": todo_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Bulk toggle todo completion.
async fn bulk_toggle(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    todo_ids: &[String],
    done: bool,
) -> Result<()> {
    if todo_ids.is_empty() {
        anyhow::bail!("at least one todo ID is required");
    }

    let client = ctx.build_client()?;
    let value = api::workflow::bulk_toggle_todos(&client, profile_type, profile_id, todo_ids, done)
        .await
        .context("failed to bulk toggle todos")?;
    ctx.output.render(&value)?;
    Ok(())
}
