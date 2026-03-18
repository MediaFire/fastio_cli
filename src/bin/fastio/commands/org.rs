/// Org command implementations for `fastio org *`.
///
/// Handles organization CRUD, billing, member management, transfer,
/// and discovery operations.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Org subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OrgCommand {
    /// List user's organizations.
    List {
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create an organization.
    Create {
        /// Organization display name.
        name: String,
        /// URL-safe domain/subdomain.
        domain: String,
        /// Description.
        description: Option<String>,
        /// Industry type.
        industry: Option<String>,
        /// Billing email.
        billing_email: Option<String>,
    },
    /// Get organization details.
    Info {
        /// Organization ID.
        org_id: String,
    },
    /// Update organization settings.
    Update {
        /// Organization ID.
        org_id: String,
        /// New name.
        name: Option<String>,
        /// New domain.
        domain: Option<String>,
        /// New description.
        description: Option<String>,
        /// New industry.
        industry: Option<String>,
        /// Billing email.
        billing_email: Option<String>,
        /// Homepage URL.
        homepage_url: Option<String>,
    },
    /// Delete (close) an organization.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Confirmation string (must match org domain or ID).
        confirm: String,
    },
    /// Billing subcommands.
    Billing(BillingCommand),
    /// Members subcommands.
    Members(OrgMembersCommand),
    /// Transfer ownership.
    Transfer {
        /// Organization ID.
        org_id: String,
        /// New owner user ID.
        new_owner_id: String,
    },
    /// Discover subcommands.
    Discover(DiscoverCommand),
    /// Get public org info.
    PublicDetails {
        /// Organization ID.
        org_id: String,
    },
    /// Get plan limits.
    Limits {
        /// Organization ID.
        org_id: String,
    },
    /// Invitations subcommands.
    Invitations(OrgInvitationsCommand),
    /// Transfer token subcommands.
    TransferToken(OrgTransferTokenCommand),
    /// Claim org ownership via transfer token.
    TransferClaim {
        /// Transfer token string.
        token: String,
    },
    /// List org workspaces.
    Workspaces {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// List org shares.
    Shares {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Org asset subcommands.
    Asset(OrgAssetCommand),
    /// Create workspace in org.
    CreateWorkspace {
        /// Organization ID.
        org_id: String,
        /// Workspace name.
        name: String,
        /// Folder name.
        folder_name: Option<String>,
        /// Description.
        description: Option<String>,
    },
}

/// Discovery subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DiscoverCommand {
    /// Discover available organizations.
    List {
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Discover all orgs.
    All {
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Discover available orgs.
    Available {
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Check domain availability.
    CheckDomain {
        /// Domain to check.
        domain: String,
    },
    /// List external orgs.
    External {
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
}

/// Billing subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BillingCommand {
    /// Get billing details for an org.
    Info {
        /// Organization ID.
        org_id: String,
    },
    /// List available billing plans.
    Plans,
    /// Get usage meters.
    Meters {
        /// Organization ID.
        org_id: String,
        /// Meter type.
        meter: String,
        /// Start time.
        start_time: Option<String>,
        /// End time.
        end_time: Option<String>,
    },
    /// Cancel subscription.
    Cancel {
        /// Organization ID.
        org_id: String,
    },
    /// Activate subscription.
    Activate {
        /// Organization ID.
        org_id: String,
    },
    /// Reset billing.
    Reset {
        /// Organization ID.
        org_id: String,
    },
    /// List billable members.
    Members {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create subscription.
    Create {
        /// Organization ID.
        org_id: String,
        /// Plan ID.
        plan_id: Option<String>,
    },
}

/// Org members subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OrgMembersCommand {
    /// List org members.
    List {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Invite a member.
    Invite {
        /// Organization ID.
        org_id: String,
        /// Email address to invite.
        email: String,
        /// Role (admin, member, guest).
        role: Option<String>,
    },
    /// Remove a member.
    Remove {
        /// Organization ID.
        org_id: String,
        /// Member ID to remove.
        member_id: String,
    },
    /// Update member role.
    UpdateRole {
        /// Organization ID.
        org_id: String,
        /// Member ID to update.
        member_id: String,
        /// New role.
        role: String,
    },
    /// Get member details.
    Details {
        /// Organization ID.
        org_id: String,
        /// Member user ID.
        member_id: String,
    },
    /// Leave organization.
    Leave {
        /// Organization ID.
        org_id: String,
    },
    /// Join organization.
    Join {
        /// Organization ID.
        org_id: String,
    },
}

/// Org invitations subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OrgInvitationsCommand {
    /// List org invitations.
    List {
        /// Organization ID.
        org_id: String,
        /// Filter by state.
        state: Option<String>,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Update an invitation.
    Update {
        /// Organization ID.
        org_id: String,
        /// Invitation ID.
        invitation_id: String,
        /// New state.
        state: Option<String>,
        /// New role.
        role: Option<String>,
    },
    /// Delete an invitation.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Invitation ID.
        invitation_id: String,
    },
}

/// Org transfer token subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OrgTransferTokenCommand {
    /// Create a transfer token.
    Create {
        /// Organization ID.
        org_id: String,
    },
    /// List transfer tokens.
    List {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Delete a transfer token.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Token ID.
        token_id: String,
    },
}

/// Org asset subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OrgAssetCommand {
    /// List asset types.
    Types,
    /// List org assets.
    List {
        /// Organization ID.
        org_id: String,
    },
    /// Delete an org asset.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Asset type name.
        asset_type: String,
    },
}

/// Valid roles for org membership.
const VALID_ORG_ROLES: &[&str] = &["admin", "member", "guest"];

/// Validate that an org ID is not empty or whitespace-only.
fn validate_org_id(org_id: &str) -> Result<()> {
    anyhow::ensure!(!org_id.trim().is_empty(), "org ID must not be empty");
    Ok(())
}

/// Validate that an email address has a basic valid format.
fn validate_email(email: &str) -> Result<()> {
    anyhow::ensure!(!email.trim().is_empty(), "email must not be empty");
    anyhow::ensure!(
        email.contains('@') && email.contains('.'),
        "invalid email address: {email}"
    );
    Ok(())
}

/// Validate that a role is one of the accepted values.
fn validate_role(role: &str) -> Result<()> {
    anyhow::ensure!(
        VALID_ORG_ROLES.contains(&role),
        "invalid role '{role}'. Must be one of: admin, member, guest"
    );
    Ok(())
}

/// Execute an org subcommand.
pub async fn execute(command: &OrgCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        OrgCommand::List { limit, offset } => list(ctx, *limit, *offset).await,
        OrgCommand::Create {
            name,
            domain,
            description,
            industry,
            billing_email,
        } => {
            create(
                ctx,
                domain,
                name,
                description.as_deref(),
                industry.as_deref(),
                billing_email.as_deref(),
            )
            .await
        }
        OrgCommand::Info { org_id } => info(ctx, org_id).await,
        OrgCommand::Update {
            org_id,
            name,
            domain,
            description,
            industry,
            billing_email,
            homepage_url,
        } => {
            update(
                ctx,
                org_id,
                name.as_deref(),
                domain.as_deref(),
                description.as_deref(),
                industry.as_deref(),
                billing_email.as_deref(),
                homepage_url.as_deref(),
            )
            .await
        }
        OrgCommand::Delete { org_id, confirm } => delete(ctx, org_id, confirm).await,
        OrgCommand::Billing(cmd) => billing(cmd, ctx).await,
        OrgCommand::Members(cmd) => members(cmd, ctx).await,
        OrgCommand::Transfer {
            org_id,
            new_owner_id,
        } => transfer(ctx, org_id, new_owner_id).await,
        OrgCommand::Discover(cmd) => execute_discover(cmd, ctx).await,
        OrgCommand::PublicDetails { org_id } => public_details(ctx, org_id).await,
        OrgCommand::Limits { org_id } => limits(ctx, org_id).await,
        OrgCommand::Invitations(cmd) => org_invitations(cmd, ctx).await,
        OrgCommand::TransferToken(cmd) => org_transfer_token(cmd, ctx).await,
        OrgCommand::TransferClaim { token } => transfer_claim(ctx, token).await,
        OrgCommand::Workspaces {
            org_id,
            limit,
            offset,
        } => list_workspaces(ctx, org_id, *limit, *offset).await,
        OrgCommand::Shares {
            org_id,
            limit,
            offset,
        } => list_shares(ctx, org_id, *limit, *offset).await,
        OrgCommand::Asset(cmd) => org_asset(cmd, ctx).await,
        OrgCommand::CreateWorkspace {
            org_id,
            name,
            folder_name,
            description,
        } => {
            create_workspace(
                ctx,
                org_id,
                name,
                folder_name.as_deref(),
                description.as_deref(),
            )
            .await
        }
    }
}

/// List organizations.
async fn list(ctx: &CommandContext<'_>, limit: Option<u32>, offset: Option<u32>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::list_orgs(&client, limit, offset)
        .await
        .context("failed to list organizations")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create an organization.
async fn create(
    ctx: &CommandContext<'_>,
    domain: &str,
    name: &str,
    description: Option<&str>,
    industry: Option<&str>,
    billing_email: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::create_org(&client, domain, name, description, industry, billing_email)
        .await
        .context("failed to create organization")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get organization details.
async fn info(ctx: &CommandContext<'_>, org_id: &str) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    let value = api::org::get_org(&client, org_id)
        .await
        .context("failed to get organization details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Update organization settings.
#[allow(clippy::too_many_arguments)]
async fn update(
    ctx: &CommandContext<'_>,
    org_id: &str,
    name: Option<&str>,
    domain: Option<&str>,
    description: Option<&str>,
    industry: Option<&str>,
    billing_email: Option<&str>,
    homepage_url: Option<&str>,
) -> Result<()> {
    validate_org_id(org_id)?;
    if name.is_none()
        && domain.is_none()
        && description.is_none()
        && industry.is_none()
        && billing_email.is_none()
        && homepage_url.is_none()
    {
        anyhow::bail!(
            "at least one update field is required (--name, --domain, --description, --industry, --billing-email, --homepage-url)"
        );
    }

    let client = ctx.build_client()?;
    let value = api::org::update_org(
        &client,
        &api::org::UpdateOrgParams {
            org_id,
            name,
            domain,
            description,
            industry,
            billing_email,
            homepage_url,
        },
    )
    .await
    .context("failed to update organization")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete (close) an organization.
async fn delete(ctx: &CommandContext<'_>, org_id: &str, confirm: &str) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    api::org::close_org(&client, org_id, confirm)
        .await
        .context("failed to close organization")?;

    let value = json!({
        "status": "closed",
        "org_id": org_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Handle billing subcommands.
async fn billing(cmd: &BillingCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        BillingCommand::Info { org_id } => {
            let value = api::org::get_billing_details(&client, org_id)
                .await
                .context("failed to get billing details")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Plans => {
            let value = api::org::list_billing_plans(&client)
                .await
                .context("failed to list billing plans")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Meters {
            org_id,
            meter,
            start_time,
            end_time,
        } => {
            let value = api::org::get_billing_meters(
                &client,
                org_id,
                meter,
                start_time.as_deref(),
                end_time.as_deref(),
            )
            .await
            .context("failed to get usage meters")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Cancel { org_id } => {
            let value = api::org::billing_cancel(&client, org_id)
                .await
                .context("failed to cancel billing")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Activate { org_id } => {
            let value = api::org::billing_activate(&client, org_id)
                .await
                .context("failed to activate billing")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Reset { org_id } => {
            let value = api::org::billing_reset(&client, org_id)
                .await
                .context("failed to reset billing")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Members {
            org_id,
            limit,
            offset,
        } => {
            let value = api::org::billing_members(&client, org_id, *limit, *offset)
                .await
                .context("failed to list billable members")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Create { org_id, plan_id } => {
            let value = api::org::billing_create(&client, org_id, plan_id.as_deref())
                .await
                .context("failed to create billing subscription")?;
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Handle members subcommands.
async fn members(cmd: &OrgMembersCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        OrgMembersCommand::List {
            org_id,
            limit,
            offset,
        } => {
            validate_org_id(org_id)?;
            let value = api::org::list_org_members(&client, org_id, *limit, *offset)
                .await
                .context("failed to list organization members")?;
            ctx.output.render(&value)?;
        }
        OrgMembersCommand::Invite {
            org_id,
            email,
            role,
        } => {
            validate_org_id(org_id)?;
            validate_email(email)?;
            if let Some(r) = role {
                validate_role(r)?;
            }
            let value = api::org::invite_org_member(&client, org_id, email, role.as_deref())
                .await
                .context("failed to invite member")?;
            ctx.output.render(&value)?;
        }
        OrgMembersCommand::Remove { org_id, member_id } => {
            validate_org_id(org_id)?;
            api::org::remove_org_member(&client, org_id, member_id)
                .await
                .context("failed to remove member")?;
            let value = json!({
                "status": "removed",
                "member_id": member_id,
            });
            ctx.output.render(&value)?;
        }
        OrgMembersCommand::UpdateRole {
            org_id,
            member_id,
            role,
        } => {
            validate_org_id(org_id)?;
            validate_role(role)?;
            let value = api::org::update_org_member_role(&client, org_id, member_id, role)
                .await
                .context("failed to update member role")?;
            ctx.output.render(&value)?;
        }
        OrgMembersCommand::Details { org_id, member_id } => {
            validate_org_id(org_id)?;
            let value = api::org::get_member_details(&client, org_id, member_id)
                .await
                .context("failed to get member details")?;
            ctx.output.render(&value)?;
        }
        OrgMembersCommand::Leave { org_id } => {
            validate_org_id(org_id)?;
            let value = api::org::leave_org(&client, org_id)
                .await
                .context("failed to leave organization")?;
            ctx.output.render(&value)?;
        }
        OrgMembersCommand::Join { org_id } => {
            validate_org_id(org_id)?;
            let value = api::org::join_org(&client, org_id)
                .await
                .context("failed to join organization")?;
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Transfer organization ownership.
async fn transfer(ctx: &CommandContext<'_>, org_id: &str, new_owner_id: &str) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    let value = api::org::transfer_org_ownership(&client, org_id, new_owner_id)
        .await
        .context("failed to transfer organization ownership")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Dispatch discover subcommands.
async fn execute_discover(command: &DiscoverCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        DiscoverCommand::List { limit, offset } => discover(ctx, *limit, *offset).await,
        DiscoverCommand::All { limit, offset } => discover_all(ctx, *limit, *offset).await,
        DiscoverCommand::Available { limit, offset } => {
            discover_available(ctx, *limit, *offset).await
        }
        DiscoverCommand::CheckDomain { domain } => discover_check_domain(ctx, domain).await,
        DiscoverCommand::External { limit, offset } => {
            discover_external(ctx, *limit, *offset).await
        }
    }
}

/// Discover available organizations.
async fn discover(ctx: &CommandContext<'_>, limit: Option<u32>, offset: Option<u32>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::discover_orgs(&client, limit, offset)
        .await
        .context("failed to discover organizations")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Handle org invitation subcommands.
async fn org_invitations(cmd: &OrgInvitationsCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match cmd {
        OrgInvitationsCommand::List {
            org_id,
            state,
            limit,
            offset,
        } => {
            let value =
                api::org::list_invitations(&client, org_id, state.as_deref(), *limit, *offset)
                    .await
                    .context("failed to list invitations")?;
            ctx.output.render(&value)?;
        }
        OrgInvitationsCommand::Update {
            org_id,
            invitation_id,
            state,
            role,
        } => {
            let value = api::org::update_invitation(
                &client,
                org_id,
                invitation_id,
                state.as_deref(),
                role.as_deref(),
            )
            .await
            .context("failed to update invitation")?;
            ctx.output.render(&value)?;
        }
        OrgInvitationsCommand::Delete {
            org_id,
            invitation_id,
        } => {
            api::org::delete_invitation(&client, org_id, invitation_id)
                .await
                .context("failed to delete invitation")?;
            let value = json!({
                "status": "deleted",
                "invitation_id": invitation_id,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Handle org transfer token subcommands.
async fn org_transfer_token(cmd: &OrgTransferTokenCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match cmd {
        OrgTransferTokenCommand::Create { org_id } => {
            let value = api::org::transfer_token_create(&client, org_id)
                .await
                .context("failed to create transfer token")?;
            ctx.output.render(&value)?;
        }
        OrgTransferTokenCommand::List {
            org_id,
            limit,
            offset,
        } => {
            let value = api::org::transfer_token_list(&client, org_id, *limit, *offset)
                .await
                .context("failed to list transfer tokens")?;
            ctx.output.render(&value)?;
        }
        OrgTransferTokenCommand::Delete { org_id, token_id } => {
            api::org::transfer_token_delete(&client, org_id, token_id)
                .await
                .context("failed to delete transfer token")?;
            let value = json!({
                "status": "deleted",
                "token_id": token_id,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Handle org asset subcommands.
async fn org_asset(cmd: &OrgAssetCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match cmd {
        OrgAssetCommand::Types => {
            let value = api::org::org_asset_types(&client)
                .await
                .context("failed to get asset types")?;
            ctx.output.render(&value)?;
        }
        OrgAssetCommand::List { org_id } => {
            let value = api::org::list_org_assets(&client, org_id)
                .await
                .context("failed to list org assets")?;
            ctx.output.render(&value)?;
        }
        OrgAssetCommand::Delete { org_id, asset_type } => {
            api::org::delete_org_asset(&client, org_id, asset_type)
                .await
                .context("failed to delete org asset")?;
            let value = json!({
                "status": "deleted",
                "asset_type": asset_type,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Get public org details.
async fn public_details(ctx: &CommandContext<'_>, org_id: &str) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    let value = api::org::get_public_details(&client, org_id)
        .await
        .context("failed to get public details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get plan limits.
async fn limits(ctx: &CommandContext<'_>, org_id: &str) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    let value = api::org::get_limits(&client, org_id)
        .await
        .context("failed to get limits")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Claim org ownership via transfer token.
async fn transfer_claim(ctx: &CommandContext<'_>, token_str: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::transfer_claim(&client, token_str)
        .await
        .context("failed to claim transfer")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Discover all orgs.
async fn discover_all(
    ctx: &CommandContext<'_>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::discover_all(&client, limit, offset)
        .await
        .context("failed to discover all orgs")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Discover available orgs.
async fn discover_available(
    ctx: &CommandContext<'_>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::discover_orgs(&client, limit, offset)
        .await
        .context("failed to discover available orgs")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Check domain availability.
async fn discover_check_domain(ctx: &CommandContext<'_>, domain: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::discover_check_domain(&client, domain)
        .await
        .context("failed to check domain")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List external orgs.
async fn discover_external(
    ctx: &CommandContext<'_>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::org::discover_external(&client, limit, offset)
        .await
        .context("failed to list external orgs")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List org workspaces.
async fn list_workspaces(
    ctx: &CommandContext<'_>,
    org_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    let value = api::org::list_workspaces(&client, org_id, limit, offset)
        .await
        .context("failed to list workspaces")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List org shares.
async fn list_shares(
    ctx: &CommandContext<'_>,
    org_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    let value = api::org::list_org_shares(&client, org_id, limit, offset)
        .await
        .context("failed to list shares")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create workspace in org.
async fn create_workspace(
    ctx: &CommandContext<'_>,
    org_id: &str,
    name: &str,
    folder_name: Option<&str>,
    description: Option<&str>,
) -> Result<()> {
    validate_org_id(org_id)?;
    let client = ctx.build_client()?;
    let fname = folder_name.unwrap_or(name);
    let value = api::org::create_workspace(&client, org_id, fname, name, description)
        .await
        .context("failed to create workspace")?;
    ctx.output.render(&value)?;
    Ok(())
}
