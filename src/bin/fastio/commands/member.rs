/// Member command implementations for `fastio member *`.
///
/// Handles member listing, adding, removing, updating, and details
/// for workspaces (and shares in the future).
use anyhow::{Context, Result, bail};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Known valid member roles.
const VALID_ROLES: &[&str] = &["admin", "member", "guest"];

/// Validate that an email address has a basic valid format.
fn validate_email(email: &str) -> Result<()> {
    if email.contains('@') && email.contains('.') && email.len() >= 5 {
        Ok(())
    } else {
        bail!("invalid email address: {email}")
    }
}

/// Validate that a role is one of the known valid roles.
fn validate_role(role: &str) -> Result<()> {
    if VALID_ROLES.contains(&role) {
        Ok(())
    } else {
        bail!(
            "invalid role '{role}'. Valid roles: {}",
            VALID_ROLES.join(", ")
        )
    }
}

/// Member subcommand variants.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum MemberCommand {
    /// List members.
    List {
        /// Workspace ID.
        workspace: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Add a member.
    Add {
        /// Workspace ID.
        workspace: String,
        /// Email or user ID to add.
        email: String,
        /// Permission role.
        role: Option<String>,
    },
    /// Remove a member.
    Remove {
        /// Workspace ID.
        workspace: String,
        /// Member ID to remove.
        member_id: String,
    },
    /// Update a member's role.
    Update {
        /// Workspace ID.
        workspace: String,
        /// Member ID to update.
        member_id: String,
        /// New role.
        role: String,
    },
    /// Get member details.
    Info {
        /// Workspace ID.
        workspace: String,
        /// Member ID.
        member_id: String,
    },
}

/// Execute a member subcommand.
pub async fn execute(command: &MemberCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        MemberCommand::List {
            workspace,
            limit,
            offset,
        } => list(ctx, workspace, *limit, *offset).await,
        MemberCommand::Add {
            workspace,
            email,
            role,
        } => add(ctx, workspace, email, role.as_deref()).await,
        MemberCommand::Remove {
            workspace,
            member_id,
        } => remove(ctx, workspace, member_id).await,
        MemberCommand::Update {
            workspace,
            member_id,
            role,
        } => update_role(ctx, workspace, member_id, role).await,
        MemberCommand::Info {
            workspace,
            member_id,
        } => info(ctx, workspace, member_id).await,
    }
}

/// List members of a workspace.
async fn list(
    ctx: &CommandContext<'_>,
    workspace: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::member::list_members(&client, "workspace", workspace, limit, offset)
        .await
        .context("failed to list members")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Add a member to a workspace.
async fn add(
    ctx: &CommandContext<'_>,
    workspace: &str,
    email: &str,
    role: Option<&str>,
) -> Result<()> {
    validate_email(email)?;
    if let Some(r) = role {
        validate_role(r)?;
    }

    let client = ctx.build_client()?;
    let value = api::member::add_member(&client, "workspace", workspace, email, role)
        .await
        .context("failed to add member")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Remove a member from a workspace.
async fn remove(ctx: &CommandContext<'_>, workspace: &str, member_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::member::remove_member(&client, "workspace", workspace, member_id)
        .await
        .context("failed to remove member")?;
    let value = json!({
        "status": "removed",
        "member_id": member_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Update a member's role.
async fn update_role(
    ctx: &CommandContext<'_>,
    workspace: &str,
    member_id: &str,
    role: &str,
) -> Result<()> {
    validate_role(role)?;

    let client = ctx.build_client()?;
    let value = api::member::update_member_role(&client, "workspace", workspace, member_id, role)
        .await
        .context("failed to update member role")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get member details.
async fn info(ctx: &CommandContext<'_>, workspace: &str, member_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::member::get_member_details(&client, "workspace", workspace, member_id)
        .await
        .context("failed to get member details")?;
    ctx.output.render(&value)?;
    Ok(())
}
