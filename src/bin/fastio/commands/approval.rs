/// Approval command implementations for `fastio approval *`.
///
/// Handles listing, requesting, approving, and rejecting approvals.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Allowed approval status filter values.
const VALID_STATUSES: &[&str] = &["pending", "approved", "rejected"];

/// Validate that a status filter value is one of the known values.
fn validate_status(status: Option<&str>) -> Result<()> {
    if let Some(s) = status
        && !VALID_STATUSES.contains(&s)
    {
        anyhow::bail!(
            "invalid approval status '{}'. Must be one of: {}",
            s,
            VALID_STATUSES.join(", ")
        );
    }
    Ok(())
}

/// Approval subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ApprovalCommand {
    /// List approvals in a workspace.
    List {
        /// Workspace ID.
        workspace: String,
        /// Filter by status (pending, approved, rejected).
        status: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Request an approval.
    Request {
        /// Workspace ID.
        workspace: String,
        /// Entity type (task, node, `worklog_entry`).
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Description of what needs approval.
        description: String,
        /// Designated approver profile ID.
        approver_id: Option<String>,
    },
    /// Approve an approval request.
    Approve {
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        comment: Option<String>,
    },
    /// Reject an approval request.
    Reject {
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        comment: Option<String>,
    },
}

/// Execute an approval subcommand.
pub async fn execute(command: &ApprovalCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        ApprovalCommand::List {
            workspace,
            status,
            limit,
            offset,
        } => list(ctx, workspace, status.as_deref(), *limit, *offset).await,
        ApprovalCommand::Request {
            workspace,
            entity_type,
            entity_id,
            description,
            approver_id,
        } => {
            request(
                ctx,
                workspace,
                entity_type,
                entity_id,
                description,
                approver_id.as_deref(),
            )
            .await
        }
        ApprovalCommand::Approve {
            approval_id,
            comment,
        } => resolve(ctx, approval_id, "approve", comment.as_deref()).await,
        ApprovalCommand::Reject {
            approval_id,
            comment,
        } => resolve(ctx, approval_id, "reject", comment.as_deref()).await,
    }
}

/// List approvals.
async fn list(
    ctx: &CommandContext<'_>,
    workspace: &str,
    status: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_status(status)?;

    let client = ctx.build_client()?;
    let value = api::workflow::list_approvals(&client, workspace, status, limit, offset)
        .await
        .context("failed to list approvals")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create an approval request.
async fn request(
    ctx: &CommandContext<'_>,
    workspace: &str,
    entity_type: &str,
    entity_id: &str,
    description: &str,
    approver_id: Option<&str>,
) -> Result<()> {
    if description.is_empty() {
        anyhow::bail!("approval description must not be empty");
    }

    let client = ctx.build_client()?;
    let value = api::workflow::create_approval(
        &client,
        entity_type,
        entity_id,
        description,
        workspace,
        approver_id,
    )
    .await
    .context("failed to create approval request")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Resolve (approve/reject) an approval.
async fn resolve(
    ctx: &CommandContext<'_>,
    approval_id: &str,
    action: &str,
    comment: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::resolve_approval(&client, approval_id, action, comment)
        .await
        .context("failed to resolve approval")?;

    let result = json!({
        "status": action,
        "approval_id": approval_id,
        "detail": value,
    });
    ctx.output.render(&result)?;
    Ok(())
}
