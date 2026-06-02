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
    /// Get billing details for an org (canonical `details`; `info` alias).
    Details {
        /// Organization ID.
        org_id: String,
    },
    /// List available billing plans.
    Plans,
    /// Get credit usage and limits (canonical `usage`; `limits` alias).
    Usage {
        /// Organization ID.
        org_id: String,
    },
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
        /// Filter by workspace (mutually exclusive with `share_id`).
        workspace_id: Option<String>,
        /// Filter by share (mutually exclusive with `workspace_id`).
        share_id: Option<String>,
    },
    /// Cancel subscription (schedules cancel at period end; requires `--yes`).
    Cancel {
        /// Organization ID.
        org_id: String,
        /// Explicit confirmation.
        yes: bool,
    },
    /// Reactivate a subscription scheduled to cancel (owner-only; PUT).
    Reactivate {
        /// Organization ID.
        org_id: String,
    },
    /// Deprecated `activate` compat shim — no network; redirects to reactivate.
    ///
    /// Carries no fields: the shim never calls the server, so the parsed
    /// `org_id` is intentionally discarded by the command mapper.
    Activate,
    /// Deprecated `reset` compat shim — no network; redirects to reactivate.
    ///
    /// Carries no fields (see [`BillingCommand::Activate`]).
    Reset,
    /// List billable members.
    Members {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Subscribe to a plan (canonical `subscribe`; `create` alias).
    Subscribe {
        /// Organization ID.
        org_id: String,
        /// Plan ID (required at the CLI surface).
        plan_id: Option<String>,
    },
    /// List billing invoices (cursor-paginated via `starting_after`).
    Invoices {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Invoice-ID cursor for the next page.
        starting_after: Option<String>,
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

/// Render the redirect-hint error shared by the deprecated `activate` / `reset`
/// compat shims. These endpoints never existed on the server; the shims make NO
/// network call — they fail fast pointing the user at `reactivate` so an old
/// invocation produces a clear redirect rather than a confusing
/// unknown-subcommand error.
fn billing_shim_redirect(old: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "`org billing {old}` has been removed (the endpoint never existed). \
         Use `org billing reactivate <org>` to reverse a scheduled cancellation, \
         or `org billing subscribe <org> --plan <id>` to start a new subscription."
    )
}

/// Handle billing subcommands.
///
/// This is a flat dispatch over `BillingCommand`; the line count comes from the
/// per-variant arms (each issues one API call + domain formatting), so keeping
/// it as a single match is clearer than fragmenting it.
#[allow(clippy::too_many_lines)]
async fn billing(cmd: &BillingCommand, ctx: &CommandContext<'_>) -> Result<()> {
    // The deprecated compat shims must NOT build a client or touch the network.
    match cmd {
        BillingCommand::Activate => return Err(billing_shim_redirect("activate")),
        BillingCommand::Reset => return Err(billing_shim_redirect("reset")),
        _ => {}
    }

    let client = ctx.build_client()?;

    match cmd {
        BillingCommand::Details { org_id } => {
            validate_org_id(org_id)?;
            let mut value = api::org::get_billing_details(&client, org_id)
                .await
                .context("failed to get billing details")?;
            billing_format::format_details(&mut value);
            ctx.output.render(&value)?;
        }
        BillingCommand::Plans => {
            let mut value = api::org::list_billing_plans(&client)
                .await
                .context("failed to list billing plans")?;
            billing_format::format_plans(&mut value);
            ctx.output.render(&value)?;
        }
        BillingCommand::Usage { org_id } => {
            validate_org_id(org_id)?;
            let value = api::org::get_credit_usage(&client, org_id)
                .await
                .context("failed to get credit usage")?;
            print_credit_cost_legend(ctx);
            ctx.output.render(&value)?;
        }
        BillingCommand::Meters {
            org_id,
            meter,
            start_time,
            end_time,
            workspace_id,
            share_id,
        } => {
            validate_org_id(org_id)?;
            let value = api::org::get_billing_meters(
                &client,
                &api::org::BillingMetersParams {
                    org_id,
                    meter,
                    start_time: start_time.as_deref(),
                    end_time: end_time.as_deref(),
                    workspace_id: workspace_id.as_deref(),
                    share_id: share_id.as_deref(),
                },
            )
            .await
            .context("failed to get usage meters")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Cancel { org_id, yes } => {
            validate_org_id(org_id)?;
            anyhow::ensure!(
                *yes,
                "cancelling schedules the subscription to end at the close of the current \
                 billing period. Re-run with --yes to confirm (use `org billing reactivate` \
                 to reverse it before it executes)."
            );
            let mut value = api::org::billing_cancel(&client, org_id)
                .await
                .context("failed to cancel billing")?;
            billing_format::format_cancel(&mut value);
            ctx.output.render(&value)?;
        }
        BillingCommand::Reactivate { org_id } => {
            validate_org_id(org_id)?;
            let mut value = api::org::billing_reactivate(&client, org_id)
                .await
                .context("failed to reactivate subscription")?;
            billing_format::format_reactivate(&mut value);
            ctx.output.render(&value)?;
        }
        // Compat shims handled above (no network); unreachable here.
        BillingCommand::Activate | BillingCommand::Reset => {}
        BillingCommand::Members {
            org_id,
            limit,
            offset,
        } => {
            validate_org_id(org_id)?;
            let value = api::org::billing_members(&client, org_id, *limit, *offset)
                .await
                .context("failed to list billable members")?;
            ctx.output.render(&value)?;
        }
        BillingCommand::Subscribe { org_id, plan_id } => {
            validate_org_id(org_id)?;
            let mut value = api::org::billing_create(&client, org_id, plan_id.as_deref())
                .await
                .context("failed to subscribe to plan")?;
            // Strip the one-time `setup_intent.client_secret` and the noisy
            // `public_key` BEFORE rendering: the user completes payment via the
            // hosted onboarding URL surfaced in the follow-up, never by handling
            // the raw secret. The onboarding follow-up keys off the still-present
            // `setup_intent`/`is_active` fields, so it is computed afterward.
            billing_format::sanitize_subscribe(&mut value);
            ctx.output.render(&value)?;
            print_subscribe_followup(ctx, &value);
        }
        BillingCommand::Invoices {
            org_id,
            limit,
            starting_after,
        } => {
            validate_org_id(org_id)?;
            let mut value =
                api::org::billing_invoices(&client, org_id, *limit, starting_after.as_deref())
                    .await
                    .context("failed to list billing invoices")?;
            billing_format::format_invoices(&mut value);
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Print the credit-cost legend that accompanies `org billing usage`.
///
/// Written to stderr so it never pollutes machine-readable stdout (JSON/CSV)
/// and is suppressed under `--quiet`.
fn print_credit_cost_legend(ctx: &CommandContext<'_>) {
    if ctx.output.quiet {
        return;
    }
    eprintln!(
        "credit costs: storage 100/GB · bandwidth 212/GB · AI tokens 1/100 · \
         doc ingestion 10/page · video 5/sec · image 5/image · conversions 25/each"
    );
}

/// After a successful subscribe, surface the hosted onboarding URL so the user
/// can complete payment. `setup_intent.client_secret` / `public_key` are
/// sensitive and are NOT printed here (only the public onboarding URL is).
fn print_subscribe_followup(ctx: &CommandContext<'_>, value: &serde_json::Value) {
    if ctx.output.quiet {
        return;
    }
    // Only emit the follow-up when the server returned a setup_intent (a NEW,
    // not-yet-active subscription that still needs a payment method).
    let needs_payment = value.get("setup_intent").is_some_and(|si| !si.is_null())
        && value.get("is_active").and_then(serde_json::Value::as_bool) != Some(true);
    if needs_payment {
        eprintln!(
            "Subscription created — complete payment to activate it: \
             https://go.fast.io/onboarding"
        );
    }
}

/// Billing-only domain formatting.
///
/// Transforms KNOWN billing fields (cents → `$x.xx`, Unix timestamp → date)
/// on the response `Value` BEFORE it reaches the generic renderer. Scoped here
/// so the generic table/markdown renderers stay generic — they must NEVER
/// guess that "an integer is cents" or "a number is a timestamp". Every helper
/// touches only explicitly-named billing fields.
mod billing_format {
    use serde_json::Value;

    /// Cents → `"$x.xx"`. Negative amounts (credits/refunds) keep their sign.
    fn cents_to_dollars(cents: i64) -> String {
        let sign = if cents < 0 { "-" } else { "" };
        let abs = cents.unsigned_abs();
        format!("{sign}${}.{:02}", abs / 100, abs % 100)
    }

    /// Format a named integer-cents field in place into a `$x.xx` string.
    fn format_cents_field(obj: &mut serde_json::Map<String, Value>, key: &str) {
        if let Some(cents) = obj.get(key).and_then(Value::as_i64) {
            obj.insert(key.to_owned(), Value::String(cents_to_dollars(cents)));
        }
    }

    /// Unix seconds → `"YYYY-MM-DD HH:MM:SS UTC"`. Leaves the field untouched
    /// (and thus rendered as-is) if the timestamp is out of range.
    fn unix_to_date(ts: i64) -> Option<String> {
        chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
    }

    /// Format a named Unix-timestamp field in place into a date string. Only
    /// rewrites integer values (string dates from the server pass through
    /// unchanged).
    fn format_timestamp_field(obj: &mut serde_json::Map<String, Value>, key: &str) {
        if let Some(ts) = obj.get(key).and_then(Value::as_i64)
            && let Some(date) = unix_to_date(ts)
        {
            obj.insert(key.to_owned(), Value::String(date));
        }
    }

    /// Format the `billing details` response — the
    /// `current_period_end` / `cancel_at` timestamps the server reflects here.
    ///
    /// The server may surface these timestamps at the TOP LEVEL of the response
    /// or NESTED under a `subscription` object (orgs.txt:1827). Format wherever
    /// found: the top-level handling is kept for compatibility and the nested
    /// `subscription.{current_period_start,current_period_end,cancel_at}` fields
    /// are formatted in addition.
    pub(super) fn format_details(value: &mut Value) {
        const TS_KEYS: [&str; 3] = ["current_period_start", "current_period_end", "cancel_at"];
        if let Some(obj) = value.as_object_mut() {
            for key in TS_KEYS {
                format_timestamp_field(obj, key);
            }
            if let Some(sub) = obj.get_mut("subscription").and_then(Value::as_object_mut) {
                for key in TS_KEYS {
                    format_timestamp_field(sub, key);
                }
            }
        }
    }

    /// Format the `plans` list — each plan's `amount` (cents) → `$x.xx`.
    pub(super) fn format_plans(value: &mut Value) {
        if let Some(plans) = value.get_mut("plans").and_then(Value::as_array_mut) {
            for plan in plans {
                if let Some(obj) = plan.as_object_mut() {
                    format_cents_field(obj, "amount");
                }
            }
        }
    }

    /// Format the schedule-cancel response (`cancel_at` Unix timestamp → date).
    pub(super) fn format_cancel(value: &mut Value) {
        if let Some(obj) = value.as_object_mut() {
            format_timestamp_field(obj, "cancel_at");
        }
    }

    /// Format the reactivate response (`current_period_end` → date).
    pub(super) fn format_reactivate(value: &mut Value) {
        if let Some(obj) = value.as_object_mut() {
            format_timestamp_field(obj, "current_period_end");
        }
    }

    /// Format the invoices list — money fields (cents → `$x.xx`) per invoice.
    /// The `period_*` / `created` fields are already server-formatted date
    /// strings, so they pass through untouched.
    pub(super) fn format_invoices(value: &mut Value) {
        if let Some(invoices) = value.get_mut("invoices").and_then(Value::as_array_mut) {
            for inv in invoices {
                if let Some(obj) = inv.as_object_mut() {
                    for key in ["amount_due", "amount_paid", "subtotal", "total"] {
                        format_cents_field(obj, key);
                    }
                }
            }
        }
    }

    /// Sanitize a `billing subscribe` (POST) response BEFORE it is rendered.
    ///
    /// The 201 create response (orgs.txt:1671) carries `setup_intent.client_secret`
    /// (a real one-time secret) and a top-level `public_key`. Neither must be
    /// echoed to the terminal: the user completes payment via the hosted
    /// onboarding URL, not by handling the raw secret, and `public_key` without
    /// the secret is useless noise. This removes BOTH while keeping the rest of
    /// the response (including `setup_intent.id` / `setup_intent.status`) so the
    /// follow-up can still detect whether payment is needed.
    pub(super) fn sanitize_subscribe(value: &mut Value) {
        if let Some(obj) = value.as_object_mut() {
            obj.remove("public_key");
            if let Some(si) = obj.get_mut("setup_intent").and_then(Value::as_object_mut) {
                si.remove("client_secret");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn cents_to_dollars_formats_correctly() {
            assert_eq!(cents_to_dollars(2900), "$29.00");
            assert_eq!(cents_to_dollars(9900), "$99.00");
            assert_eq!(cents_to_dollars(5), "$0.05");
            assert_eq!(cents_to_dollars(0), "$0.00");
            assert_eq!(cents_to_dollars(100), "$1.00");
            assert_eq!(cents_to_dollars(-2900), "-$29.00");
        }

        #[test]
        fn unix_to_date_formats_known_timestamp() {
            // 1735689600 == 2025-01-01 00:00:00 UTC
            assert_eq!(
                unix_to_date(1_735_689_600).as_deref(),
                Some("2025-01-01 00:00:00 UTC")
            );
        }

        #[test]
        fn format_invoices_converts_money_only() {
            let mut v = json!({
                "invoices": [{
                    "id": "in_1",
                    "amount_due": 2900,
                    "amount_paid": 2900,
                    "subtotal": 2900,
                    "total": 2900,
                    "period_start": "2026-03-01 00:00:00 UTC",
                    "created": "2026-03-01 00:00:00 UTC"
                }],
                "has_more": false
            });
            format_invoices(&mut v);
            let inv = &v["invoices"][0];
            assert_eq!(inv["amount_due"], "$29.00");
            assert_eq!(inv["total"], "$29.00");
            // Server-formatted date strings are left untouched.
            assert_eq!(inv["period_start"], "2026-03-01 00:00:00 UTC");
            // The id and has_more are untouched.
            assert_eq!(inv["id"], "in_1");
            assert_eq!(v["has_more"], false);
        }

        #[test]
        fn format_plans_converts_amount() {
            let mut v = json!({
                "plans": [
                    {"id": "solo_monthly", "name": "Solo", "amount": 2900},
                    {"id": "business_v2_monthly", "name": "Business", "amount": 9900}
                ]
            });
            format_plans(&mut v);
            assert_eq!(v["plans"][0]["amount"], "$29.00");
            assert_eq!(v["plans"][1]["amount"], "$99.00");
        }

        #[test]
        fn format_cancel_converts_cancel_at_timestamp() {
            let mut v = json!({
                "status": "scheduled_cancellation",
                "cancel_at": 1_735_689_600i64,
                "cancel_at_period_end": true
            });
            format_cancel(&mut v);
            assert_eq!(v["cancel_at"], "2025-01-01 00:00:00 UTC");
            // Non-timestamp fields untouched.
            assert_eq!(v["status"], "scheduled_cancellation");
        }

        #[test]
        fn format_reactivate_converts_period_end() {
            let mut v = json!({
                "status": "reactivated",
                "current_period_end": 1_735_689_600i64,
                "cancel_at_period_end": false
            });
            format_reactivate(&mut v);
            assert_eq!(v["current_period_end"], "2025-01-01 00:00:00 UTC");
        }

        #[test]
        fn format_details_leaves_null_cancel_at() {
            // A null cancel_at must not be coerced to a date.
            let mut v = json!({"current_period_end": null, "cancel_at": null});
            format_details(&mut v);
            assert!(v["cancel_at"].is_null());
            assert!(v["current_period_end"].is_null());
        }

        #[test]
        fn format_details_handles_nested_subscription_timestamps() {
            // Timestamps may be nested under a `subscription` object
            // (orgs.txt:1827); they must be formatted there too, while
            // top-level handling is preserved.
            let mut v = json!({
                "current_period_end": 1_735_689_600i64,
                "subscription": {
                    "id": "sub_1",
                    "current_period_start": 1_735_689_600i64,
                    "current_period_end": 1_735_689_600i64,
                    "cancel_at": 1_735_689_600i64
                }
            });
            format_details(&mut v);
            assert_eq!(v["current_period_end"], "2025-01-01 00:00:00 UTC");
            assert_eq!(
                v["subscription"]["current_period_start"],
                "2025-01-01 00:00:00 UTC"
            );
            assert_eq!(
                v["subscription"]["current_period_end"],
                "2025-01-01 00:00:00 UTC"
            );
            assert_eq!(v["subscription"]["cancel_at"], "2025-01-01 00:00:00 UTC");
            // Non-timestamp nested fields untouched.
            assert_eq!(v["subscription"]["id"], "sub_1");
        }

        #[test]
        fn sanitize_subscribe_removes_client_secret_and_public_key() {
            // The 201 subscribe response (orgs.txt:1671) carries a real
            // client_secret + public_key that must NEVER reach the rendered
            // output, while setup_intent.id/status are retained.
            let mut v = json!({
                "result": true,
                "setup_intent": {
                    "id": "seti_1",
                    "client_secret": "seti_1_secret_LIVE",
                    "status": "requires_payment_method"
                },
                "is_active": false,
                "public_key": "pk_live_should_never_log"
            });
            sanitize_subscribe(&mut v);
            let rendered = v.to_string();
            assert!(
                !rendered.contains("seti_1_secret_LIVE"),
                "client_secret leaked: {rendered}"
            );
            assert!(
                !rendered.contains("pk_live_should_never_log"),
                "public_key leaked: {rendered}"
            );
            assert!(v["setup_intent"].get("client_secret").is_none());
            assert!(v.get("public_key").is_none());
            // The non-sensitive fields are retained.
            assert_eq!(v["setup_intent"]["id"], "seti_1");
            assert_eq!(v["setup_intent"]["status"], "requires_payment_method");
            assert_eq!(v["result"], true);
        }
    }
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
