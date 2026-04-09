/// Task command implementations for `fastio task *`.
///
/// Handles task CRUD, assignment, completion, and task list management.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Maximum allowed task priority value (inclusive).
const MAX_PRIORITY: u8 = 4;

/// Validate that a task priority is within the allowed range (0-4).
fn validate_priority(priority: Option<u8>) -> Result<()> {
    if let Some(p) = priority
        && p > MAX_PRIORITY
    {
        anyhow::bail!("priority must be between 0 and {MAX_PRIORITY}, got {p}");
    }
    Ok(())
}

/// Task subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TaskCommand {
    /// List all tasks in a workspace (across all task lists).
    List {
        /// Workspace ID.
        workspace: String,
        /// Task list ID to filter by.
        list_id: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create a new task.
    Create {
        /// Workspace ID.
        workspace: String,
        /// Task list ID.
        list_id: String,
        /// Task title.
        title: String,
        /// Task description.
        description: Option<String>,
        /// Task status.
        status: Option<String>,
        /// Priority (0-4).
        priority: Option<u8>,
        /// Assignee profile ID.
        assignee_id: Option<String>,
    },
    /// Get task details.
    Info {
        /// Task list ID.
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Update a task.
    Update {
        /// Task list ID.
        list_id: String,
        /// Task ID.
        task_id: String,
        /// New title.
        title: Option<String>,
        /// New description.
        description: Option<String>,
        /// New status.
        status: Option<String>,
        /// New priority.
        priority: Option<u8>,
        /// New assignee.
        assignee_id: Option<String>,
    },
    /// Delete a task.
    Delete {
        /// Task list ID.
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Assign a task to a user.
    Assign {
        /// Task list ID.
        list_id: String,
        /// Task ID.
        task_id: String,
        /// Assignee profile ID (omit to unassign).
        assignee_id: Option<String>,
    },
    /// Mark a task as complete.
    Complete {
        /// Task list ID.
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Move a task to a different list.
    Move {
        /// Source task list ID.
        list_id: String,
        /// Task ID.
        task_id: String,
        /// Target task list ID.
        target_list_id: String,
        /// Sort order in the target list.
        sort_order: Option<u32>,
    },
    /// Bulk change status for multiple tasks in a list.
    BulkStatus {
        /// Task list ID.
        list_id: String,
        /// Comma-separated task IDs.
        task_ids: String,
        /// New status.
        status: String,
    },
    /// Reorder tasks within a list.
    Reorder {
        /// Task list ID.
        list_id: String,
        /// Comma-separated task IDs in desired order.
        task_ids: String,
    },
    /// Reorder task lists in a workspace or share.
    ReorderLists {
        /// Profile type: workspace or share.
        profile_type: String,
        /// Workspace or share ID.
        profile_id: String,
        /// Comma-separated list IDs in desired order.
        list_ids: String,
    },
    /// Manage task lists.
    Lists(TaskListCommand),
}

/// Task list subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TaskListCommand {
    /// List all task lists in a workspace or share.
    List {
        /// Profile type: workspace or share.
        profile_type: String,
        /// Workspace or share ID.
        profile_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create a task list in a workspace or share.
    Create {
        /// Profile type: workspace or share.
        profile_type: String,
        /// Workspace or share ID.
        profile_id: String,
        /// List name.
        name: String,
        /// List description.
        description: Option<String>,
    },
    /// Update a task list.
    Update {
        /// Task list ID.
        list_id: String,
        /// New name.
        name: Option<String>,
        /// New description.
        description: Option<String>,
    },
    /// Delete a task list.
    Delete {
        /// Task list ID.
        list_id: String,
    },
}

/// Execute a task subcommand.
pub async fn execute(command: &TaskCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        TaskCommand::List {
            workspace,
            list_id,
            limit,
            offset,
        } => list(ctx, workspace, list_id.as_deref(), *limit, *offset).await,
        TaskCommand::Create {
            workspace,
            list_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        } => {
            create(
                ctx,
                workspace,
                list_id,
                title,
                description.as_deref(),
                status.as_deref(),
                *priority,
                assignee_id.as_deref(),
            )
            .await
        }
        TaskCommand::Info { list_id, task_id } => info(ctx, list_id, task_id).await,
        TaskCommand::Update {
            list_id,
            task_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        } => {
            update(
                ctx,
                list_id,
                task_id,
                title.as_deref(),
                description.as_deref(),
                status.as_deref(),
                *priority,
                assignee_id.as_deref(),
            )
            .await
        }
        TaskCommand::Delete { list_id, task_id } => delete(ctx, list_id, task_id).await,
        TaskCommand::Assign {
            list_id,
            task_id,
            assignee_id,
        } => assign(ctx, list_id, task_id, assignee_id.as_deref()).await,
        TaskCommand::Complete { list_id, task_id } => complete(ctx, list_id, task_id).await,
        TaskCommand::Move {
            list_id,
            task_id,
            target_list_id,
            sort_order,
        } => move_task(ctx, list_id, task_id, target_list_id, *sort_order).await,
        TaskCommand::BulkStatus {
            list_id,
            task_ids,
            status,
        } => bulk_status(ctx, list_id, task_ids, status).await,
        TaskCommand::Reorder { list_id, task_ids } => reorder(ctx, list_id, task_ids).await,
        TaskCommand::ReorderLists {
            profile_type,
            profile_id,
            list_ids,
        } => reorder_lists(ctx, profile_type, profile_id, list_ids).await,
        TaskCommand::Lists(cmd) => lists(cmd, ctx).await,
    }
}

/// List tasks.
async fn list(
    ctx: &CommandContext<'_>,
    workspace: &str,
    list_id: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;

    if let Some(lid) = list_id {
        let value = api::workflow::list_tasks(&client, lid, limit, offset)
            .await
            .context("failed to list tasks")?;
        ctx.output.render(&value)?;
    } else {
        // List all task lists first (no direct "all tasks" endpoint)
        let value = api::workflow::list_task_lists(&client, workspace, limit, offset)
            .await
            .context("failed to list task lists")?;
        ctx.output.render(&value)?;
    }
    Ok(())
}

/// Create a task.
#[allow(clippy::too_many_arguments)]
async fn create(
    ctx: &CommandContext<'_>,
    _workspace: &str,
    list_id: &str,
    title: &str,
    description: Option<&str>,
    status: Option<&str>,
    priority: Option<u8>,
    assignee_id: Option<&str>,
) -> Result<()> {
    validate_priority(priority)?;

    let client = ctx.build_client()?;
    let value = api::workflow::create_task(
        &client,
        &api::workflow::CreateTaskParams {
            list_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        },
    )
    .await
    .context("failed to create task")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get task details.
async fn info(ctx: &CommandContext<'_>, list_id: &str, task_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::get_task(&client, list_id, task_id)
        .await
        .context("failed to get task details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Update a task.
#[allow(clippy::too_many_arguments)]
async fn update(
    ctx: &CommandContext<'_>,
    list_id: &str,
    task_id: &str,
    title: Option<&str>,
    description: Option<&str>,
    status: Option<&str>,
    priority: Option<u8>,
    assignee_id: Option<&str>,
) -> Result<()> {
    if title.is_none()
        && description.is_none()
        && status.is_none()
        && priority.is_none()
        && assignee_id.is_none()
    {
        anyhow::bail!(
            "at least one update field is required (--title, --description, --status, --priority, --assignee-id)"
        );
    }

    validate_priority(priority)?;

    let client = ctx.build_client()?;
    let value = api::workflow::update_task(
        &client,
        &api::workflow::UpdateTaskParams {
            list_id,
            task_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        },
    )
    .await
    .context("failed to update task")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a task.
async fn delete(ctx: &CommandContext<'_>, list_id: &str, task_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::workflow::delete_task(&client, list_id, task_id)
        .await
        .context("failed to delete task")?;
    let value = json!({
        "status": "deleted",
        "task_id": task_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Assign a task.
async fn assign(
    ctx: &CommandContext<'_>,
    list_id: &str,
    task_id: &str,
    assignee_id: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::assign_task(&client, list_id, task_id, assignee_id)
        .await
        .context("failed to assign task")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Mark a task as complete.
async fn complete(ctx: &CommandContext<'_>, list_id: &str, task_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::change_task_status(&client, list_id, task_id, "complete")
        .await
        .context("failed to mark task complete")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Move a task to a different list.
async fn move_task(
    ctx: &CommandContext<'_>,
    list_id: &str,
    task_id: &str,
    target_list_id: &str,
    sort_order: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::move_task(&client, list_id, task_id, target_list_id, sort_order)
        .await
        .context("failed to move task")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Bulk change status for multiple tasks.
async fn bulk_status(
    ctx: &CommandContext<'_>,
    list_id: &str,
    task_ids_csv: &str,
    status: &str,
) -> Result<()> {
    let ids: Vec<String> = task_ids_csv
        .split(',')
        .map(|s| s.trim().to_owned())
        .collect();
    anyhow::ensure!(!ids.is_empty(), "task_ids must not be empty");

    let client = ctx.build_client()?;
    let value = api::workflow::bulk_status_tasks(&client, list_id, &ids, status)
        .await
        .context("failed to bulk-update task statuses")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Reorder tasks within a list.
async fn reorder(ctx: &CommandContext<'_>, list_id: &str, task_ids_csv: &str) -> Result<()> {
    let ids: Vec<String> = task_ids_csv
        .split(',')
        .map(|s| s.trim().to_owned())
        .collect();
    anyhow::ensure!(!ids.is_empty(), "task_ids must not be empty");

    let client = ctx.build_client()?;
    let value = api::workflow::reorder_tasks(&client, list_id, &ids)
        .await
        .context("failed to reorder tasks")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Reorder task lists in a workspace or share.
async fn reorder_lists(
    ctx: &CommandContext<'_>,
    profile_type: &str,
    profile_id: &str,
    list_ids_csv: &str,
) -> Result<()> {
    let ids: Vec<String> = list_ids_csv
        .split(',')
        .map(|s| s.trim().to_owned())
        .collect();
    anyhow::ensure!(!ids.is_empty(), "list_ids must not be empty");

    let client = ctx.build_client()?;
    let value = api::workflow::reorder_task_lists(&client, profile_type, profile_id, &ids)
        .await
        .context("failed to reorder task lists")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Handle task list subcommands.
async fn lists(cmd: &TaskListCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        TaskListCommand::List {
            profile_type,
            profile_id,
            limit,
            offset,
        } => {
            let value = api::workflow::list_task_lists_ctx(
                &client,
                profile_type,
                profile_id,
                *limit,
                *offset,
            )
            .await
            .context("failed to list task lists")?;
            ctx.output.render(&value)?;
        }
        TaskListCommand::Create {
            profile_type,
            profile_id,
            name,
            description,
        } => {
            let value = api::workflow::create_task_list_ctx(
                &client,
                profile_type,
                profile_id,
                name,
                description.as_deref(),
            )
            .await
            .context("failed to create task list")?;
            ctx.output.render(&value)?;
        }
        TaskListCommand::Update {
            list_id,
            name,
            description,
        } => {
            if name.is_none() && description.is_none() {
                anyhow::bail!("at least one update field is required (--name, --description)");
            }
            let value = api::workflow::update_task_list(
                &client,
                list_id,
                name.as_deref(),
                description.as_deref(),
            )
            .await
            .context("failed to update task list")?;
            ctx.output.render(&value)?;
        }
        TaskListCommand::Delete { list_id } => {
            api::workflow::delete_task_list(&client, list_id)
                .await
                .context("failed to delete task list")?;
            let value = json!({
                "status": "deleted",
                "list_id": list_id,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}
