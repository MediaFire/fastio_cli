/// Approval command implementations for `fastio approval *`.
///
/// Handles listing, requesting, approving, rejecting, updating, deleting,
/// filtered listing, summaries, and the per-user approvals dashboard.
///
/// Part of the `[legacy]` workflow primitives, superseded by the `fastio
/// workflow` orchestration group; remains functional for now.
///
/// Approval **creation** is always routed through the **scoped**
/// `/{profile_type}/{profile_id}/approvals/create/` endpoint (never the
/// unscoped alias) so the request body survives the server-side redirect from
/// the legacy unscoped create route. The per-approval **action** routes
/// (details/approve/reject/update/delete) use the scoped form when a
/// workspace/share scope is supplied, and fall back to the legacy unscoped
/// `/approvals/{approval_id}/{action}/` route when it is omitted — preserving
/// the historical `approval approve <id>` syntax (backward compatibility).
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::api::workflow::{CreateApprovalParams, FilterQuery, UpdateApprovalParams};

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

/// Scope (profile type + id) an approval mutation is addressed to.
#[derive(Debug, Clone)]
pub struct ApprovalScope {
    /// Profile type: `workspace` or `share`.
    pub profile_type: String,
    /// Workspace or share profile ID.
    pub profile_id: String,
}

/// Convert an optional [`ApprovalScope`] into the `(profile_type, profile_id)`
/// tuple the API layer expects. `None` selects the legacy unscoped route.
fn scope_pair(scope: Option<&ApprovalScope>) -> Option<(&str, &str)> {
    scope.map(|s| (s.profile_type.as_str(), s.profile_id.as_str()))
}

/// Parse an optional `--properties` JSON-object string into a value to send in
/// the request body. Returns a clear error if the string is not valid JSON or
/// is not a JSON object (the contract types `properties` as an object).
fn parse_properties(properties: Option<&str>) -> Result<Option<serde_json::Value>> {
    match properties {
        None => Ok(None),
        Some(raw) => {
            let value: serde_json::Value =
                serde_json::from_str(raw).context("--properties must be a valid JSON object")?;
            if !value.is_object() {
                anyhow::bail!("--properties must be a JSON object (e.g. '{{\"key\":\"value\"}}')");
            }
            Ok(Some(value))
        }
    }
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
        /// Scope (workspace or share) the approval is created in.
        scope: ApprovalScope,
        /// Entity type (task, node, `worklog_entry`, share).
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Description of what needs approval.
        description: String,
        /// Designated approver profile ID.
        approver_id: Option<String>,
        /// Informational deadline.
        deadline: Option<String>,
        /// Associated artifact node ID.
        node_id: Option<String>,
        /// Metadata properties as a JSON object string.
        properties: Option<String>,
    },
    /// Get approval details.
    Info {
        /// Scope (workspace or share) the approval belongs to. `None` uses the
        /// legacy unscoped route for backward compatibility.
        scope: Option<ApprovalScope>,
        /// Approval ID.
        approval_id: String,
    },
    /// Approve an approval request.
    Approve {
        /// Scope (workspace or share) the approval belongs to. `None` uses the
        /// legacy unscoped route for backward compatibility.
        scope: Option<ApprovalScope>,
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        comment: Option<String>,
    },
    /// Reject an approval request.
    Reject {
        /// Scope (workspace or share) the approval belongs to. `None` uses the
        /// legacy unscoped route for backward compatibility.
        scope: Option<ApprovalScope>,
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        comment: Option<String>,
    },
    /// Update a pending approval.
    Update {
        /// Scope (workspace or share) the approval belongs to. `None` uses the
        /// legacy unscoped route for backward compatibility.
        scope: Option<ApprovalScope>,
        /// Approval ID.
        approval_id: String,
        /// Updated description.
        description: Option<String>,
        /// Updated designated approver profile ID.
        approver_id: Option<String>,
        /// Updated deadline.
        deadline: Option<String>,
        /// Updated associated node ID.
        node_id: Option<String>,
        /// Updated metadata properties as a JSON object string.
        properties: Option<String>,
    },
    /// Delete an approval.
    Delete {
        /// Scope (workspace or share) the approval belongs to. `None` uses the
        /// legacy unscoped route for backward compatibility.
        scope: Option<ApprovalScope>,
        /// Approval ID.
        approval_id: String,
    },
    /// Filtered approval list (personal/group view).
    Filter {
        /// Scope (workspace or share).
        scope: ApprovalScope,
        /// Filter: pending, created, assigned, resolved.
        filter: String,
        /// Status filter (created/assigned only).
        status: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Approval count summary for a workspace or share.
    Summary {
        /// Scope (workspace or share).
        scope: ApprovalScope,
    },
    /// The authenticated user's approvals across all profiles.
    Mine {
        /// Filter: pending, created, resolved.
        filter: String,
        /// Status filter (created only).
        status: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
}

/// Execute an approval subcommand.
#[allow(clippy::too_many_lines)]
pub async fn execute(command: &ApprovalCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        ApprovalCommand::List {
            workspace,
            status,
            limit,
            offset,
        } => list(ctx, workspace, status.as_deref(), *limit, *offset).await,
        ApprovalCommand::Request {
            scope,
            entity_type,
            entity_id,
            description,
            approver_id,
            deadline,
            node_id,
            properties,
        } => {
            request(
                ctx,
                scope,
                entity_type,
                entity_id,
                description,
                approver_id.as_deref(),
                deadline.as_deref(),
                node_id.as_deref(),
                properties.as_deref(),
            )
            .await
        }
        ApprovalCommand::Info { scope, approval_id } => {
            info(ctx, scope.as_ref(), approval_id).await
        }
        ApprovalCommand::Approve {
            scope,
            approval_id,
            comment,
        } => {
            resolve(
                ctx,
                scope.as_ref(),
                approval_id,
                "approve",
                comment.as_deref(),
            )
            .await
        }
        ApprovalCommand::Reject {
            scope,
            approval_id,
            comment,
        } => {
            resolve(
                ctx,
                scope.as_ref(),
                approval_id,
                "reject",
                comment.as_deref(),
            )
            .await
        }
        ApprovalCommand::Update {
            scope,
            approval_id,
            description,
            approver_id,
            deadline,
            node_id,
            properties,
        } => {
            update(
                ctx,
                scope.as_ref(),
                approval_id,
                description.as_deref(),
                approver_id.as_deref(),
                deadline.as_deref(),
                node_id.as_deref(),
                properties.as_deref(),
            )
            .await
        }
        ApprovalCommand::Delete { scope, approval_id } => {
            delete(ctx, scope.as_ref(), approval_id).await
        }
        ApprovalCommand::Filter {
            scope,
            filter,
            status,
            limit,
            offset,
        } => filter_list(ctx, scope, filter, status.as_deref(), *limit, *offset).await,
        ApprovalCommand::Summary { scope } => summary(ctx, scope).await,
        ApprovalCommand::Mine {
            filter,
            status,
            limit,
            offset,
        } => mine(ctx, filter, status.as_deref(), *limit, *offset).await,
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
#[allow(clippy::too_many_arguments)]
async fn request(
    ctx: &CommandContext<'_>,
    scope: &ApprovalScope,
    entity_type: &str,
    entity_id: &str,
    description: &str,
    approver_id: Option<&str>,
    deadline: Option<&str>,
    node_id: Option<&str>,
    properties: Option<&str>,
) -> Result<()> {
    if description.is_empty() {
        anyhow::bail!("approval description must not be empty");
    }

    let properties = parse_properties(properties)?;
    let client = ctx.build_client()?;
    let params = CreateApprovalParams {
        profile_type: &scope.profile_type,
        profile_id: &scope.profile_id,
        entity_type,
        entity_id,
        description,
        approver_id,
        deadline,
        node_id,
        properties: properties.as_ref(),
    };
    let value = api::workflow::create_approval(&client, &params)
        .await
        .context("failed to create approval request")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get approval details.
async fn info(
    ctx: &CommandContext<'_>,
    scope: Option<&ApprovalScope>,
    approval_id: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::get_approval(&client, scope_pair(scope), approval_id)
        .await
        .context("failed to get approval details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Resolve (approve/reject) an approval.
async fn resolve(
    ctx: &CommandContext<'_>,
    scope: Option<&ApprovalScope>,
    approval_id: &str,
    action: &str,
    comment: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::workflow::resolve_approval(&client, scope_pair(scope), approval_id, action, comment)
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

/// Update a pending approval.
#[allow(clippy::too_many_arguments)]
async fn update(
    ctx: &CommandContext<'_>,
    scope: Option<&ApprovalScope>,
    approval_id: &str,
    description: Option<&str>,
    approver_id: Option<&str>,
    deadline: Option<&str>,
    node_id: Option<&str>,
    properties: Option<&str>,
) -> Result<()> {
    if description.is_none()
        && approver_id.is_none()
        && deadline.is_none()
        && node_id.is_none()
        && properties.is_none()
    {
        anyhow::bail!(
            "approval update requires at least one of: --description, --approver-id, \
             --deadline, --node-id, --properties"
        );
    }

    let properties = parse_properties(properties)?;
    let client = ctx.build_client()?;
    let params = UpdateApprovalParams {
        scope: scope_pair(scope),
        approval_id,
        description,
        approver_id,
        deadline,
        node_id,
        properties: properties.as_ref(),
    };
    let value = api::workflow::update_approval(&client, &params)
        .await
        .context("failed to update approval")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete an approval.
async fn delete(
    ctx: &CommandContext<'_>,
    scope: Option<&ApprovalScope>,
    approval_id: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::delete_approval(&client, scope_pair(scope), approval_id)
        .await
        .context("failed to delete approval")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Filtered approval list (personal/group view).
async fn filter_list(
    ctx: &CommandContext<'_>,
    scope: &ApprovalScope,
    filter: &str,
    status: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_status(status)?;

    let client = ctx.build_client()?;
    let query = FilterQuery {
        limit,
        offset,
        status,
        entry_type: None,
    };
    let value = api::workflow::list_approvals_filtered(
        &client,
        &scope.profile_type,
        &scope.profile_id,
        filter,
        &query,
    )
    .await
    .context("failed to list filtered approvals")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Approval count summary.
async fn summary(ctx: &CommandContext<'_>, scope: &ApprovalScope) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workflow::approvals_summary(&client, &scope.profile_type, &scope.profile_id)
        .await
        .context("failed to get approval summary")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// The authenticated user's approvals across all profiles.
async fn mine(
    ctx: &CommandContext<'_>,
    filter: &str,
    status: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_status(status)?;

    let client = ctx.build_client()?;
    let query = FilterQuery {
        limit,
        offset,
        status,
        entry_type: None,
    };
    let value = api::workflow::user_approvals(&client, filter, &query)
        .await
        .context("failed to list user approvals")?;
    ctx.output.render(&value)?;
    Ok(())
}
