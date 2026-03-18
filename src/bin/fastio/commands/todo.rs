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
    /// List todos in a workspace.
    List {
        /// Workspace ID.
        workspace: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create a new todo.
    Create {
        /// Workspace ID.
        workspace: String,
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
}

/// Execute a todo subcommand.
pub async fn execute(command: &TodoCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        TodoCommand::List {
            workspace,
            limit,
            offset,
        } => list(ctx, workspace, *limit, *offset).await,
        TodoCommand::Create {
            workspace,
            title,
            assignee_id,
        } => create(ctx, workspace, title, assignee_id.as_deref()).await,
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
    }
}

/// List todos.
async fn list(
    ctx: &CommandContext<'_>,
    workspace: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::list_todos(&client, workspace, limit, offset)
        .await
        .context("failed to list todos")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a todo.
async fn create(
    ctx: &CommandContext<'_>,
    workspace: &str,
    title: &str,
    assignee_id: Option<&str>,
) -> Result<()> {
    if title.is_empty() {
        anyhow::bail!("todo title must not be empty");
    }

    let client = ctx.build_client()?;
    let value = api::workflow::create_todo(&client, workspace, title, assignee_id)
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
