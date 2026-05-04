/// Workspace command implementations for `fastio workspace *`.
///
/// Handles workspace listing, creation, details, update, deletion,
/// workflow management, search, and limits.
use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Workspace subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum WorkspaceCommand {
    /// List all workspaces.
    List {
        /// Filter by organization ID.
        org_id: Option<String>,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create a workspace.
    Create {
        /// Organization ID.
        org_id: String,
        /// Workspace display name.
        name: String,
        /// URL-safe folder name.
        folder_name: Option<String>,
        /// Description.
        description: Option<String>,
        /// Enable AI intelligence.
        intelligence: Option<bool>,
    },
    /// Get workspace details.
    Info {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Update workspace settings.
    Update {
        /// Workspace ID.
        workspace_id: String,
        /// New name.
        name: Option<String>,
        /// New description.
        description: Option<String>,
        /// New folder name.
        folder_name: Option<String>,
    },
    /// Delete a workspace.
    Delete {
        /// Workspace ID.
        workspace_id: String,
        /// Confirmation string.
        confirm: String,
    },
    /// Enable workflow features.
    EnableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Disable workflow features.
    DisableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// List active background jobs (poll after async metadata extract).
    JobsStatus {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Search workspace content.
    Search {
        /// Workspace ID.
        workspace_id: String,
        /// Search query.
        query: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Get workspace limits.
    Limits {
        /// Workspace ID.
        workspace_id: String,
    },
}

/// Execute a workspace subcommand.
pub async fn execute(command: &WorkspaceCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        WorkspaceCommand::List {
            org_id,
            limit,
            offset,
        } => list(ctx, org_id.as_deref(), *limit, *offset).await,
        WorkspaceCommand::Create {
            org_id,
            name,
            folder_name,
            description,
            intelligence,
        } => {
            create(
                ctx,
                org_id,
                name,
                folder_name.as_deref(),
                description.as_deref(),
                *intelligence,
            )
            .await
        }
        WorkspaceCommand::Info { workspace_id } => info(ctx, workspace_id).await,
        WorkspaceCommand::Update {
            workspace_id,
            name,
            description,
            folder_name,
        } => {
            update(
                ctx,
                workspace_id,
                name.as_deref(),
                description.as_deref(),
                folder_name.as_deref(),
            )
            .await
        }
        WorkspaceCommand::Delete {
            workspace_id,
            confirm,
        } => delete(ctx, workspace_id, confirm).await,
        WorkspaceCommand::EnableWorkflow { workspace_id } => {
            enable_workflow(ctx, workspace_id).await
        }
        WorkspaceCommand::DisableWorkflow { workspace_id } => {
            disable_workflow(ctx, workspace_id).await
        }
        WorkspaceCommand::JobsStatus { workspace_id } => jobs_status(ctx, workspace_id).await,
        WorkspaceCommand::Search {
            workspace_id,
            query,
            limit,
            offset,
        } => search(ctx, workspace_id, query, *limit, *offset).await,
        WorkspaceCommand::Limits { workspace_id } => limits(ctx, workspace_id).await,
    }
}

/// List workspaces.
async fn list(
    ctx: &CommandContext<'_>,
    org_id: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::list_workspaces(&client, org_id, limit, offset)
        .await
        .context("failed to list workspaces")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a workspace.
async fn create(
    ctx: &CommandContext<'_>,
    org_id: &str,
    name: &str,
    folder_name: Option<&str>,
    description: Option<&str>,
    intelligence: Option<bool>,
) -> Result<()> {
    // Use folder_name if provided, otherwise derive from name
    let effective_folder =
        folder_name.map_or_else(|| name.to_lowercase().replace(' ', "-"), String::from);

    let client = ctx.build_client()?;
    let value = api::workspace::create_workspace(
        &client,
        &api::workspace::CreateWorkspaceParams {
            org_id,
            folder_name: &effective_folder,
            name,
            description,
            intelligence,
        },
    )
    .await
    .context("failed to create workspace")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get workspace details.
async fn info(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::get_workspace(&client, workspace_id)
        .await
        .context("failed to get workspace details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Update workspace settings.
async fn update(
    ctx: &CommandContext<'_>,
    workspace_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    folder_name: Option<&str>,
) -> Result<()> {
    if name.is_none() && description.is_none() && folder_name.is_none() {
        anyhow::bail!(
            "at least one update field is required (--name, --description, --folder-name)"
        );
    }

    let mut fields = HashMap::new();
    if let Some(v) = name {
        fields.insert("name".to_owned(), v.to_owned());
    }
    if let Some(v) = description {
        fields.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = folder_name {
        fields.insert("folder_name".to_owned(), v.to_owned());
    }

    let client = ctx.build_client()?;
    let value = api::workspace::update_workspace(&client, workspace_id, &fields)
        .await
        .context("failed to update workspace")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a workspace.
async fn delete(ctx: &CommandContext<'_>, workspace_id: &str, confirm: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::workspace::delete_workspace(&client, workspace_id, confirm)
        .await
        .context("failed to delete workspace")?;

    let value = json!({
        "status": "deleted",
        "workspace_id": workspace_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Enable workflow features.
async fn enable_workflow(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::enable_workflow(&client, workspace_id)
        .await
        .context("failed to enable workflow features")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Disable workflow features.
async fn disable_workflow(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::disable_workflow(&client, workspace_id)
        .await
        .context("failed to disable workflow features")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List active background jobs (poll target after async metadata extract).
async fn jobs_status(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::jobs_status(&client, workspace_id)
        .await
        .context("failed to fetch workspace jobs status")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Search workspace content.
async fn search(
    ctx: &CommandContext<'_>,
    workspace_id: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::search_workspace(&client, workspace_id, query, limit, offset)
        .await
        .context("failed to search workspace")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get workspace limits.
async fn limits(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::get_workspace_limits(&client, workspace_id)
        .await
        .context("failed to get workspace limits")?;
    ctx.output.render(&value)?;
    Ok(())
}
