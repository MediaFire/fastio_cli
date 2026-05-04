/// CLI argument parsing for the Fast.io CLI.
///
/// Defines the root `Cli` struct and all subcommands using clap's derive API.
use clap::{Parser, Subcommand, ValueEnum};
use std::fmt;

/// Fast.io cloud storage CLI.
#[derive(Parser)]
#[command(
    name = "fastio",
    version,
    about = "Command-line interface for the Fast.io cloud storage platform",
    long_about = None,
)]
pub struct Cli {
    /// Output format (json, table, csv, markdown). Auto-detects if omitted.
    #[arg(long, global = true, value_parser = ["json", "table", "csv", "markdown", "md"])]
    pub format: Option<String>,

    /// Comma-separated list of fields to include in output.
    #[arg(long, global = true)]
    pub fields: Option<String>,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Suppress all output.
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// Increase verbosity (-v info, -vv debug, -vvv trace API calls).
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Named profile to use.
    #[arg(long, global = true)]
    pub profile: Option<String>,

    /// Bearer token for authentication (overrides stored credentials).
    #[arg(long, global = true, env = "FASTIO_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Override the API base URL.
    #[arg(long, global = true)]
    pub api_base: Option<String>,

    /// The subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level command groups.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum Commands {
    /// Authentication and credential management.
    #[command(subcommand)]
    Auth(AuthCommands),

    /// User profile management.
    #[command(subcommand)]
    User(UserCommands),
    /// Organization management.
    #[command(subcommand)]
    Org(OrgCommands),
    /// Workspace management.
    #[command(subcommand)]
    Workspace(WorkspaceCommands),
    /// Workspace member management.
    #[command(subcommand)]
    Member(MemberCommands),
    /// Invitations.
    #[command(subcommand)]
    Invitation(InvitationCommands),

    /// File and folder operations.
    #[command(subcommand)]
    Files(FilesCommands),
    /// File uploads.
    #[command(subcommand)]
    Upload(UploadCommands),
    /// File downloads.
    #[command(subcommand)]
    Download(DownloadCommands),
    /// Share management (data rooms).
    #[command(subcommand)]
    Share(ShareCommands),
    /// AI chat and search.
    #[command(subcommand)]
    Ai(AiCommands),
    /// File comments.
    #[command(subcommand)]
    Comment(CommentCommands),
    /// Activity events.
    #[command(subcommand)]
    Event(EventCommands),
    /// File previews.
    #[command(subcommand)]
    Preview(PreviewCommands),
    /// Organization and workspace assets.
    #[command(subcommand)]
    Asset(AssetCommands),
    /// Task management.
    #[command(subcommand)]
    Task(TaskCommands),
    /// Worklog management.
    #[command(subcommand)]
    Worklog(WorklogCommands),
    /// Approval workflows.
    #[command(subcommand)]
    Approval(ApprovalCommands),
    /// Todo items.
    #[command(subcommand)]
    Todo(TodoCommands),

    /// Connected apps and integrations.
    #[command(subcommand)]
    Apps(AppsCommands),
    /// Cloud import and sync.
    #[command(subcommand)]
    Import(ImportCommands),
    /// File locking.
    #[command(subcommand)]
    Lock(LockCommands),

    /// Metadata extraction and template management.
    #[command(subcommand)]
    Metadata(MetadataCommands),

    /// System health and status checks (no auth required).
    #[command(subcommand)]
    System(SystemCommands),

    /// Start the MCP (Model Context Protocol) server over stdio.
    Mcp {
        /// Optional comma-separated list of tools to enable (default: all).
        #[arg(long)]
        tools: Option<String>,
    },

    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: ShellType,
    },

    /// Manage CLI configuration and profiles.
    #[command(subcommand)]
    Configure(ConfigureCommands),

    /// Print the agent skill guide (usage patterns for AI agents and automation).
    Skill,
}

// ─── Auth ────────────────────────────────────────────────────────────────────

/// Auth subcommands.
#[derive(Subcommand)]
#[non_exhaustive]
pub enum AuthCommands {
    /// Log in to Fast.io. Uses browser PKCE flow by default.
    /// Provide --email and --password for direct authentication.
    Login {
        /// Email address for basic auth login.
        #[arg(long)]
        email: Option<String>,
        /// Password for basic auth login.
        #[arg(long)]
        password: Option<String>,
    },
    /// Clear stored credentials for the current profile.
    Logout,
    /// Show current authentication status.
    Status,
    /// Create a new Fast.io account.
    Signup {
        /// Email address.
        #[arg(long)]
        email: String,
        /// Password.
        #[arg(long)]
        password: String,
        /// First name.
        #[arg(long)]
        first_name: Option<String>,
        /// Last name.
        #[arg(long)]
        last_name: Option<String>,
    },
    /// Send or confirm email verification.
    Verify {
        /// Email address to verify.
        #[arg(long)]
        email: String,
        /// Verification code (omit to send a new code).
        #[arg(long)]
        code: Option<String>,
    },
    /// Two-factor authentication management.
    #[command(subcommand, name = "2fa")]
    TwoFa(TwoFaCommands),
    /// API key management.
    #[command(subcommand, name = "api-key")]
    ApiKey(ApiKeyCommands),
    /// Verify token validity.
    Check,
    /// Show session info from stored credentials.
    Session,
    /// Check email availability.
    #[command(name = "email-check")]
    EmailCheck {
        /// Email to check.
        email: String,
    },
    /// Request a password reset email.
    #[command(name = "password-reset-request")]
    PasswordResetRequest {
        /// Email address.
        email: String,
    },
    /// Complete a password reset.
    #[command(name = "password-reset")]
    PasswordReset {
        /// Reset code.
        code: String,
        /// New password.
        #[arg(long = "new-password")]
        password1: String,
        /// Confirm new password.
        #[arg(long = "confirm-password")]
        password2: String,
    },
    /// OAuth session management.
    #[command(subcommand)]
    Oauth(OauthCommands),
    /// Check the scopes and capabilities of the current token.
    Scopes,
    /// Check whether a password reset code is valid.
    #[command(name = "password-reset-check")]
    PasswordResetCheck {
        /// The reset code to check.
        code: String,
    },
}

/// 2FA subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TwoFaCommands {
    /// Enable 2FA on a channel (sms, totp, whatsapp).
    Setup {
        /// 2FA channel to enable.
        #[arg(long)]
        channel: String,
    },
    /// Verify a 2FA code after login.
    Verify {
        /// The 2FA verification code.
        #[arg(long)]
        code: String,
    },
    /// Disable 2FA.
    Disable {
        /// 2FA verification token.
        #[arg(long)]
        token: String,
    },
    /// Check 2FA status.
    Status,
    /// Send a 2FA code on a channel.
    Send {
        /// Channel: sms, totp, or whatsapp.
        #[arg(long)]
        channel: String,
    },
    /// Verify TOTP setup.
    #[command(name = "verify-setup")]
    VerifySetup {
        /// The TOTP verification token.
        #[arg(long)]
        token: String,
    },
}

/// API key subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ApiKeyCommands {
    /// Create a new API key.
    Create {
        /// Key label / memo.
        #[arg(long)]
        name: Option<String>,
        /// Scopes as a JSON array string.
        #[arg(long)]
        scopes: Option<String>,
    },
    /// List all API keys.
    List,
    /// Delete an API key.
    Delete {
        /// The API key ID to delete.
        #[arg(long)]
        key_id: String,
    },
    /// Get API key details.
    Get {
        /// The API key ID.
        key_id: String,
    },
    /// Update an API key.
    Update {
        /// The API key ID.
        key_id: String,
        /// New label / memo.
        #[arg(long)]
        name: Option<String>,
        /// New scopes.
        #[arg(long)]
        scopes: Option<String>,
    },
}

/// OAuth session subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OauthCommands {
    /// List OAuth sessions.
    List,
    /// Get OAuth session details.
    Details {
        /// Session ID.
        session_id: String,
    },
    /// Revoke a single session.
    Revoke {
        /// Session ID.
        session_id: String,
    },
    /// Revoke all sessions.
    #[command(name = "revoke-all")]
    RevokeAll,
}

// ─── User ────────────────────────────────────────────────────────────────────

/// User subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserCommands {
    /// Get current user profile.
    Info,
    /// Update user profile.
    Update {
        /// First name.
        #[arg(long)]
        first_name: Option<String>,
        /// Last name.
        #[arg(long)]
        last_name: Option<String>,
        /// Display name.
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Manage user avatar.
    #[command(subcommand)]
    Avatar(UserAvatarCommands),
    /// Manage user settings.
    #[command(subcommand)]
    Settings(UserSettingsCommands),
    /// Search for users.
    Search {
        /// Search query.
        query: String,
    },
    /// Close/delete the current account.
    Close {
        /// Confirmation string.
        confirmation: String,
    },
    /// Get user details by ID.
    Details {
        /// User ID.
        user_id: String,
    },
    /// List accessible profile types.
    Profiles,
    /// Check country authorization.
    Allowed,
    /// Check org creation eligibility.
    #[command(name = "org-limits")]
    OrgLimits,
    /// List the user's shares.
    Shares,
    /// User invitations management.
    #[command(subcommand)]
    Invitations(UserInvitationsCommands),
    /// User asset management.
    #[command(subcommand)]
    Asset(UserAssetCommands),
    /// Enable or disable photo auto-sync from SSO providers.
    Autosync {
        /// State: "enable" or "disable".
        #[arg(value_parser = ["enable", "disable"])]
        state: String,
    },
    /// Get support PIN and identity verification hash.
    Pin,
    /// Validate a phone number.
    Phone {
        /// Country code (e.g. "1" for US).
        #[arg(long)]
        country_code: String,
        /// Phone number (e.g. "5551234567").
        #[arg(long)]
        phone_number: String,
    },
}

/// User invitations subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserInvitationsCommands {
    /// List pending invitations.
    List,
    /// Get invitation details.
    Details {
        /// Invitation ID.
        invitation_id: String,
    },
    /// Accept all pending invitations.
    #[command(name = "accept-all")]
    AcceptAll,
}

/// User asset subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserAssetCommands {
    /// List available asset types.
    Types,
    /// List user assets.
    List {
        /// User ID.
        user_id: String,
    },
    /// Delete a user asset.
    Delete {
        /// Asset type name.
        asset_type: String,
    },
    /// Upload a user asset (e.g. `profile_pic`).
    Upload {
        /// Asset type name (e.g. `profile_pic`).
        #[arg(long)]
        asset_type: String,
        /// Path to the file to upload.
        #[arg(long)]
        file: String,
    },
    /// Read/download a user asset binary.
    Read {
        /// User ID.
        #[arg(long)]
        user_id: String,
        /// Asset type name (e.g. `profile_pic`).
        #[arg(long)]
        asset_type: String,
        /// Output file path.
        #[arg(long)]
        output: String,
    },
}

/// User avatar subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserAvatarCommands {
    /// Upload an avatar image.
    Upload {
        /// Path to the image file.
        file: String,
    },
    /// Remove the current avatar.
    Remove,
}

/// User settings subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserSettingsCommands {
    /// Get user settings.
    Get,
    /// Update user settings.
    Update {
        /// First name.
        #[arg(long)]
        first_name: Option<String>,
        /// Last name.
        #[arg(long)]
        last_name: Option<String>,
    },
}

// ─── Org ─────────────────────────────────────────────────────────────────────

/// Organization subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgCommands {
    /// List your organizations.
    List {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new organization.
    Create {
        /// Organization display name.
        name: String,
        /// URL-safe subdomain for the organization.
        #[arg(long)]
        domain: String,
        /// Organization description.
        #[arg(long)]
        description: Option<String>,
        /// Industry type (e.g. technology, healthcare).
        #[arg(long)]
        industry: Option<String>,
        /// Billing contact email.
        #[arg(long)]
        billing_email: Option<String>,
    },
    /// Get organization details.
    Info {
        /// Organization ID or domain.
        org_id: String,
    },
    /// Update organization settings.
    Update {
        /// Organization ID.
        org_id: String,
        /// New display name.
        #[arg(long)]
        name: Option<String>,
        /// New domain.
        #[arg(long)]
        domain: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New industry.
        #[arg(long)]
        industry: Option<String>,
        /// Billing email.
        #[arg(long)]
        billing_email: Option<String>,
        /// Homepage URL.
        #[arg(long)]
        homepage_url: Option<String>,
    },
    /// Delete (close) an organization. Permanent and irreversible.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Confirmation string (must match org domain or ID).
        #[arg(long)]
        confirm: String,
    },
    /// Billing information and management.
    #[command(subcommand)]
    Billing(OrgBillingCommands),
    /// Organization member management.
    #[command(subcommand)]
    Members(OrgMembersCommands),
    /// Transfer organization ownership.
    Transfer {
        /// Organization ID.
        org_id: String,
        /// User ID of the new owner.
        new_owner_id: String,
    },
    /// Discover organizations you can join.
    Discover {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get public organization info.
    #[command(name = "public-details")]
    PublicDetails {
        /// Organization ID.
        org_id: String,
    },
    /// Get plan limits.
    Limits {
        /// Organization ID.
        org_id: String,
    },
    /// Org invitation management.
    #[command(subcommand)]
    Invitations(OrgInvitationsCommands),
    /// Transfer token management.
    #[command(subcommand, name = "transfer-token")]
    TransferToken(OrgTransferTokenCommands),
    /// Claim org ownership via transfer token.
    #[command(name = "transfer-claim")]
    TransferClaim {
        /// Transfer token string.
        token: String,
    },
    /// Discover all organizations.
    #[command(name = "discover-all")]
    DiscoverAll {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Discover available organizations.
    #[command(name = "discover-available")]
    DiscoverAvailable {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Check domain availability.
    #[command(name = "discover-check-domain")]
    DiscoverCheckDomain {
        /// Domain to check.
        domain: String,
    },
    /// List external organizations.
    #[command(name = "discover-external")]
    DiscoverExternal {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// List org workspaces.
    Workspaces {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// List org shares.
    Shares {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Org asset management.
    #[command(subcommand, name = "asset")]
    OrgAsset(OrgAssetCommands),
    /// Create workspace in org.
    #[command(name = "create-workspace")]
    CreateWorkspace {
        /// Organization ID.
        org_id: String,
        /// Workspace name.
        name: String,
        /// Folder name.
        #[arg(long)]
        folder_name: Option<String>,
        /// Description.
        #[arg(long)]
        description: Option<String>,
    },
}

/// Org billing subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgBillingCommands {
    /// Get billing details for an organization.
    Info {
        /// Organization ID.
        org_id: String,
    },
    /// List available billing plans.
    Plans,
    /// Get usage meters/metrics for an organization.
    Meters {
        /// Organization ID.
        org_id: String,
        /// Meter type (e.g. `storage_bytes`, `transfer_bytes`, `ai_tokens`).
        #[arg(long)]
        meter: String,
        /// Start time for the meter range.
        #[arg(long)]
        start_time: Option<String>,
        /// End time for the meter range.
        #[arg(long)]
        end_time: Option<String>,
    },
    /// Cancel a billing subscription.
    Cancel {
        /// Organization ID.
        org_id: String,
    },
    /// Activate a billing subscription.
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
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a billing subscription.
    Create {
        /// Organization ID.
        org_id: String,
        /// Plan ID.
        #[arg(long)]
        plan_id: Option<String>,
    },
    /// List billing invoices.
    Invoices {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
}

/// Org members subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgMembersCommands {
    /// List organization members.
    List {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Invite a member to the organization.
    Invite {
        /// Organization ID.
        org_id: String,
        /// Email address to invite.
        email: String,
        /// Role: admin, member, or guest.
        #[arg(long)]
        role: Option<String>,
    },
    /// Remove a member from the organization.
    Remove {
        /// Organization ID.
        org_id: String,
        /// Member user ID or email to remove.
        member_id: String,
    },
    /// Update a member's role.
    #[command(name = "update-role")]
    UpdateRole {
        /// Organization ID.
        org_id: String,
        /// Member user ID to update.
        member_id: String,
        /// New role: admin, member, or guest.
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

/// Org invitations subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgInvitationsCommands {
    /// List org invitations.
    List {
        /// Organization ID.
        org_id: String,
        /// Filter by state.
        #[arg(long)]
        state: Option<String>,
        /// Max results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Update an invitation.
    Update {
        /// Organization ID.
        org_id: String,
        /// Invitation ID.
        invitation_id: String,
        /// New state.
        #[arg(long)]
        state: Option<String>,
        /// New role.
        #[arg(long)]
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

/// Org transfer token subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgTransferTokenCommands {
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
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
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

/// Org asset subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgAssetCommands {
    /// List available asset types.
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

// ─── Workspace ───────────────────────────────────────────────────────────────

/// Workspace subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkspaceCommands {
    /// List all workspaces.
    List {
        /// Filter by organization ID.
        #[arg(long)]
        org: Option<String>,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new workspace.
    Create {
        /// Workspace display name.
        name: String,
        /// Organization ID to create the workspace in.
        #[arg(long)]
        org: String,
        /// URL-safe folder name (derived from name if omitted).
        #[arg(long)]
        folder_name: Option<String>,
        /// Workspace description.
        #[arg(long)]
        description: Option<String>,
        /// Enable AI intelligence features.
        #[arg(long)]
        intelligence: Option<bool>,
    },
    /// Get workspace details.
    Info {
        /// Workspace ID or folder name.
        workspace_id: String,
    },
    /// Update workspace settings.
    Update {
        /// Workspace ID.
        workspace_id: String,
        /// New display name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New folder name.
        #[arg(long)]
        folder_name: Option<String>,
    },
    /// Delete a workspace. Permanent and irreversible.
    Delete {
        /// Workspace ID.
        workspace_id: String,
        /// Confirmation string (must match workspace folder name or ID).
        #[arg(long)]
        confirm: String,
    },
    /// Enable workflow features on a workspace.
    #[command(name = "enable-workflow")]
    EnableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Disable workflow features on a workspace.
    #[command(name = "disable-workflow")]
    DisableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// List active background jobs (poll after async metadata extract).
    #[command(name = "jobs-status")]
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
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get workspace limits and quotas.
    Limits {
        /// Workspace ID.
        workspace_id: String,
    },
}

// ─── Member ──────────────────────────────────────────────────────────────────

/// Member subcommands (workspace members).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum MemberCommands {
    /// List workspace members.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add a member to a workspace.
    Add {
        /// Email address or user ID to add.
        email: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Permission role: admin, member, or guest.
        #[arg(long)]
        role: Option<String>,
    },
    /// Remove a member from a workspace.
    Remove {
        /// Member ID to remove.
        member_id: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
    },
    /// Update a member's role.
    Update {
        /// Member ID to update.
        member_id: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// New role: admin, member, or guest.
        #[arg(long)]
        role: String,
    },
    /// Get member details.
    Info {
        /// Member ID.
        member_id: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
    },
}

// ─── Invitation ──────────────────────────────────────────────────────────────

/// Invitation subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum InvitationCommands {
    /// List pending invitations for the current user.
    List {
        /// Max results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Accept an invitation (or all pending invitations).
    Accept {
        /// Invitation ID (omit to accept all).
        invitation_id: Option<String>,
    },
    /// Decline an invitation.
    Decline {
        /// Invitation ID.
        invitation_id: String,
        /// Entity type: workspace or share.
        #[arg(long)]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// Delete an invitation.
    Delete {
        /// Invitation ID.
        invitation_id: String,
        /// Entity type: workspace or share.
        #[arg(long)]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
}

// ─── Files ──────────────────────────────────────────────────────────────────

/// File and folder subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum FilesCommands {
    /// List files and folders in a workspace directory.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Parent folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Sort column: name, updated, created, type.
        #[arg(long, value_parser = ["name", "updated", "created", "type"])]
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        #[arg(long, value_parser = ["asc", "desc"])]
        sort_dir: Option<String>,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Get details for a file or folder.
    Info {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Storage node ID.
        node_id: String,
    },
    /// Create a new folder.
    #[command(name = "create-folder")]
    CreateFolder {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Folder name.
        name: String,
        /// Parent folder node ID (defaults to root).
        #[arg(long)]
        parent: Option<String>,
    },
    /// Move a file or folder to another location.
    Move {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to move.
        node_id: String,
        /// Destination folder node ID.
        #[arg(long)]
        to: String,
    },
    /// Copy a file or folder.
    Copy {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to copy.
        node_id: String,
        /// Destination folder node ID.
        #[arg(long)]
        to: String,
    },
    /// Rename a file or folder.
    Rename {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to rename.
        node_id: String,
        /// New name.
        new_name: String,
    },
    /// Delete a file or folder (move to trash).
    Delete {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to delete.
        node_id: String,
    },
    /// Restore a file or folder from trash.
    Restore {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to restore.
        node_id: String,
    },
    /// Permanently delete a trashed file or folder.
    Purge {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to permanently delete.
        node_id: String,
    },
    /// List items in the trash.
    Trash {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Sort column: name, updated, created, type.
        #[arg(long)]
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        #[arg(long)]
        sort_dir: Option<String>,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// List versions of a file.
    Versions {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Search for files in a workspace.
    Search {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Search query.
        query: String,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// List recently accessed files.
    Recent {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Add a share link to a folder.
    #[command(name = "add-link")]
    AddLink {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Parent folder node ID.
        parent: String,
        /// Share ID to link.
        share_id: String,
    },
    /// Transfer a node to another workspace.
    Transfer {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to transfer.
        node_id: String,
        /// Target workspace ID.
        #[arg(long)]
        to_workspace: String,
    },
    /// Restore a specific version of a file.
    #[command(name = "version-restore")]
    VersionRestore {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
        /// Version ID.
        version_id: String,
    },
    /// File lock operations.
    #[command(subcommand)]
    Lock(FileLockCommands),
    /// Read file content (text).
    Read {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Create or get a quickshare link.
    Quickshare {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
}

/// File lock subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum FileLockCommands {
    /// Acquire a file lock.
    Acquire {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Check lock status.
    Status {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Release a file lock.
    Release {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        #[arg(long)]
        lock_token: String,
    },
}

// ─── Upload ─────────────────────────────────────────────────────────────────

/// Upload subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UploadCommands {
    /// Upload a local file with progress bar.
    File {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Path to the local file.
        file_path: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
    },
    /// Upload text content as a file.
    Text {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filename for the uploaded file.
        #[arg(long)]
        name: String,
        /// Text content.
        content: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
    },
    /// Import a file from a URL.
    Url {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Source URL.
        url: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Override filename (derived from URL if omitted).
        #[arg(long)]
        name: Option<String>,
    },
    /// Create an upload session manually.
    #[command(name = "create-session")]
    CreateSession {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filename.
        filename: String,
        /// File size in bytes.
        filesize: u64,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
    },
    /// Upload a single chunk.
    Chunk {
        /// Upload key/ID.
        upload_key: String,
        /// Chunk number (1-based).
        chunk_num: u32,
        /// Path to chunk data file.
        file: String,
    },
    /// Trigger assembly after all chunks are uploaded.
    Finalize {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Check upload status.
    Status {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Cancel an upload.
    Cancel {
        /// Upload key/ID.
        upload_key: String,
    },
    /// List active upload sessions.
    #[command(name = "list-sessions")]
    ListSessions,
    /// Cancel all uploads.
    #[command(name = "cancel-all")]
    CancelAll,
    /// Check chunk status.
    #[command(name = "chunk-status")]
    ChunkStatus {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Delete a chunk.
    #[command(name = "chunk-delete")]
    ChunkDelete {
        /// Upload key/ID.
        upload_key: String,
        /// Chunk number.
        chunk_num: u32,
    },
    /// List web imports.
    #[command(name = "web-list")]
    WebList,
    /// Cancel a web import.
    #[command(name = "web-cancel")]
    WebCancel {
        /// Upload ID.
        upload_id: String,
    },
    /// Check web import status.
    #[command(name = "web-status")]
    WebStatus {
        /// Upload ID.
        upload_id: String,
    },
    /// Get upload limits.
    Limits,
    /// Get restricted file extensions.
    Extensions,
    /// Upload a file via streaming (no exact size required upfront).
    Stream {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Path to the local file (use - for stdin).
        file_path: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Maximum upload size in bytes (defaults to plan limit).
        #[arg(long)]
        max_size: Option<u64>,
        /// Override filename (required for stdin, derived from path otherwise).
        #[arg(long)]
        name: Option<String>,
        /// Pre-computed hash of the file content for integrity verification.
        #[arg(long, requires = "hash_algo")]
        hash: Option<String>,
        /// Hash algorithm used (e.g. sha256). Requires --hash.
        #[arg(long, requires = "hash")]
        hash_algo: Option<String>,
    },
    /// Create a streaming upload session manually.
    #[command(name = "create-stream-session")]
    CreateStreamSession {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filename.
        filename: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Maximum upload size in bytes (defaults to plan limit).
        #[arg(long)]
        max_size: Option<u64>,
    },
    /// Send data to a streaming upload session (auto-finalizes).
    #[command(name = "stream-send")]
    StreamSend {
        /// Upload key/ID from create-stream-session.
        upload_key: String,
        /// Path to data file.
        file: String,
        /// Maximum file size in bytes (rejects before reading if exceeded).
        #[arg(long)]
        max_size: Option<u64>,
        /// Pre-computed hash of the file content.
        #[arg(long, requires = "hash_algo")]
        hash: Option<String>,
        /// Hash algorithm used (e.g. sha256). Requires --hash.
        #[arg(long, requires = "hash")]
        hash_algo: Option<String>,
    },
}

// ─── Download ───────────────────────────────────────────────────────────────

/// Download subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum DownloadCommands {
    /// Download a single file.
    File {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID of the file to download.
        node_id: String,
        /// Output file path (auto-determined if omitted).
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Download a folder as a ZIP archive.
    Folder {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID of the folder to download.
        node_id: String,
        /// Output file path (auto-determined if omitted).
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Download multiple files.
    Batch {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node IDs to download.
        node_ids: Vec<String>,
        /// Output directory (defaults to current directory).
        #[arg(long, short)]
        output_dir: Option<String>,
    },
}

// ─── Share ──────────────────────────────────────────────────────────────────

/// Share subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ShareCommands {
    /// List all shares.
    List {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new share.
    Create {
        /// Share name/title.
        name: String,
        /// Workspace ID to create the share in.
        #[arg(long)]
        workspace: String,
        /// Share description.
        #[arg(long)]
        description: Option<String>,
        /// Access options.
        #[arg(long)]
        access_options: Option<String>,
        /// Password for share access.
        #[arg(long)]
        password: Option<String>,
        /// Enable anonymous uploads.
        #[arg(long)]
        anonymous_uploads: Option<bool>,
        /// Enable AI intelligence features.
        #[arg(long)]
        intelligence: Option<bool>,
        /// Download security level (high, medium, or off).
        #[arg(long, value_parser = ["high", "medium", "off"])]
        download_security: Option<String>,
    },
    /// Get share details.
    Info {
        /// Share ID or custom name.
        share_id: String,
    },
    /// Update share settings.
    Update {
        /// Share ID.
        share_id: String,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New access options.
        #[arg(long)]
        access_options: Option<String>,
        /// Enable or disable downloads (legacy — prefer --download-security).
        #[arg(long)]
        download_enabled: Option<bool>,
        /// Enable or disable comments.
        #[arg(long)]
        comments_enabled: Option<bool>,
        /// Enable or disable anonymous uploads.
        #[arg(long)]
        anonymous_uploads: Option<bool>,
        /// Download security level (high, medium, or off).
        #[arg(long, value_parser = ["high", "medium", "off"])]
        download_security: Option<String>,
    },
    /// Delete a share. Permanent and irreversible.
    Delete {
        /// Share ID.
        share_id: String,
        /// Confirmation string (must match share ID or custom name).
        #[arg(long)]
        confirm: String,
    },
    /// Archive a share.
    Archive {
        /// Share ID.
        share_id: String,
    },
    /// Unarchive a share.
    Unarchive {
        /// Share ID.
        share_id: String,
    },
    /// Authenticate to a password-protected share.
    PasswordAuth {
        /// Share ID.
        share_id: String,
        /// Password for the share.
        password: String,
    },
    /// Authenticate as a guest to a share.
    GuestAuth {
        /// Share ID.
        share_id: String,
    },
    /// Get public details for a share.
    PublicInfo {
        /// Share ID.
        share_id: String,
    },
    /// List available shares for the current user.
    Available,
    /// Check if a share name is available.
    CheckName {
        /// Share name to check.
        name: String,
    },
    /// Enable workflow on a share.
    WorkflowEnable {
        /// Share ID.
        share_id: String,
    },
    /// Disable workflow on a share.
    WorkflowDisable {
        /// Share ID.
        share_id: String,
    },
    /// Share file operations.
    #[command(subcommand)]
    Files(ShareFilesCommands),
    /// Share member operations.
    #[command(subcommand)]
    Members(ShareMembersCommands),
}

/// Share file subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ShareFilesCommands {
    /// List files and folders in a share.
    List {
        /// Share ID.
        share_id: String,
        /// Parent folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Sort column: name, updated, created, type.
        #[arg(long, value_parser = ["name", "updated", "created", "type"])]
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        #[arg(long, value_parser = ["asc", "desc"])]
        sort_dir: Option<String>,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
}

/// Share member subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ShareMembersCommands {
    /// List share members.
    List {
        /// Share ID.
        share_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add a member to a share.
    Add {
        /// Share ID.
        share_id: String,
        /// Email address to add.
        email: String,
        /// Permission role: admin, member, or guest.
        #[arg(long)]
        role: Option<String>,
    },
    /// Remove a member from a share.
    Remove {
        /// Share ID.
        share_id: String,
        /// Member ID to remove.
        member_id: String,
    },
}

// ─── Comment ────────────────────────────────────────────────────────────────

/// Comment subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum CommentCommands {
    /// List comments on a file.
    List {
        /// Storage node ID.
        node_id: String,
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
        /// Sort order: created or -created.
        #[arg(long, value_parser = ["created", "-created"])]
        sort: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add a comment to a file.
    Create {
        /// Storage node ID.
        node_id: String,
        /// Comment text.
        text: String,
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
    },
    /// Reply to an existing comment.
    Reply {
        /// Comment ID to reply to.
        comment_id: String,
        /// Reply text.
        text: String,
        /// Storage node ID.
        #[arg(long)]
        node_id: String,
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
    },
    /// Delete a comment.
    Delete {
        /// Comment ID.
        comment_id: String,
    },
    /// List all comments across a workspace or share.
    ListAll {
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
        /// Sort order: created or -created.
        #[arg(long, value_parser = ["created", "-created"])]
        sort: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get comment details.
    Info {
        /// Comment ID.
        comment_id: String,
    },
    /// Add an emoji reaction to a comment.
    React {
        /// Comment ID.
        comment_id: String,
        /// Emoji to react with (e.g. thumbsup, heart).
        emoji: String,
    },
    /// Remove your emoji reaction from a comment.
    Unreact {
        /// Comment ID.
        comment_id: String,
    },
}

// ─── Event ──────────────────────────────────────────────────────────────────

/// Event subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum EventCommands {
    /// List/search activity events.
    List {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get event details.
    Info {
        /// Event ID.
        event_id: String,
    },
    /// Long-poll for activity updates.
    Poll {
        /// Workspace or share ID to monitor.
        entity_id: String,
        /// Last activity timestamp for incremental polling.
        #[arg(long)]
        lastactivity: Option<String>,
        /// Max seconds the server will hold the connection (1-95).
        #[arg(long)]
        wait: Option<u32>,
    },
    /// Acknowledge an event.
    Ack {
        /// Event ID to acknowledge.
        event_id: String,
    },
    /// Get an AI-powered summary of events.
    Summarize {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Filter by subcategory.
        #[arg(long)]
        subcategory: Option<String>,
        /// Free-text context for the AI summarizer.
        #[arg(long)]
        user_context: Option<String>,
        /// Maximum number of events to include.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
}

// ─── Preview ────────────────────────────────────────────────────────────────

/// Preview subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum PreviewCommands {
    /// Get a preauthorized preview URL.
    Get {
        /// Storage node ID.
        node_id: String,
        /// Preview type.
        #[arg(long, value_parser = ["binary", "thumbnail", "image", "pdf", "hlsstream", "audio", "spreadsheet"])]
        preview_type: String,
        /// Context type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
    },
    /// Get a thumbnail preview URL (shorthand for --preview-type thumbnail).
    Thumbnail {
        /// Storage node ID.
        node_id: String,
        /// Context type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
    },
    /// Request a file transformation URL (resize, crop, format conversion).
    Transform {
        /// Storage node ID.
        node_id: String,
        /// Transform name (e.g. "image").
        #[arg(long)]
        transform_name: String,
        /// Context type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// Target width in pixels.
        #[arg(long)]
        width: Option<u32>,
        /// Target height in pixels.
        #[arg(long)]
        height: Option<u32>,
        /// Output format: png, jpg, webp.
        #[arg(long)]
        output_format: Option<String>,
        /// Size preset: `IconSmall`, `IconMedium`, Preview.
        #[arg(long)]
        size: Option<String>,
    },
}

// ─── Asset ──────────────────────────────────────────────────────────────────

/// Asset subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum AssetCommands {
    /// Upload an asset (logo, banner, etc.).
    Upload {
        /// Asset type name (e.g. logo, banner, photo).
        asset_type: String,
        /// Path to the file to upload.
        file: String,
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// Remove an asset.
    Remove {
        /// Asset type name.
        asset_type: String,
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// List assets on an entity.
    List {
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// List available asset types.
    Types {
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
    },
}

// ─── AI ─────────────────────────────────────────────────────────────────────

/// AI subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum AiCommands {
    /// Send a chat message and get the AI response.
    Chat {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// User message text.
        message: String,
        /// Existing chat ID (creates new if omitted).
        #[arg(long)]
        chat_id: Option<String>,
        /// Scope query to specific file/folder node IDs (comma-separated).
        #[arg(long, value_delimiter = ',')]
        node_ids: Option<Vec<String>>,
        /// Folder ID to scope the AI query to.
        #[arg(long)]
        folder_id: Option<String>,
        /// Enable enhanced intelligence for this query.
        #[arg(long)]
        intelligence: Option<bool>,
    },
    /// Semantic search over indexed workspace files.
    Search {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Search query.
        query: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get chat message history.
    History {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Chat ID (lists all chats if omitted).
        #[arg(long)]
        chat_id: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Generate a shareable AI summary from specific workspace files.
    Summary {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// File node IDs to include in the summary (at least one required).
        node_ids: Vec<String>,
    },
}

// ─── Task ───────────────────────────────────────────────────────────────────

/// Task subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TaskCommands {
    /// List tasks in a workspace.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filter by task list ID.
        #[arg(long)]
        list_id: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new task.
    Create {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task title.
        title: String,
        /// Task description.
        #[arg(long)]
        description: Option<String>,
        /// Status: pending, `in_progress`, complete, blocked.
        #[arg(long, value_parser = ["pending", "in_progress", "complete", "blocked"])]
        status: Option<String>,
        /// Priority: 0=none, 1=low, 2=medium, 3=high, 4=critical.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: Option<u8>,
        /// Assignee profile ID.
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Get task details.
    Info {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Update a task.
    Update {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
        /// New title.
        #[arg(long)]
        title: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New status: pending, `in_progress`, complete, blocked.
        #[arg(long, value_parser = ["pending", "in_progress", "complete", "blocked"])]
        status: Option<String>,
        /// New priority: 0=none, 1=low, 2=medium, 3=high, 4=critical.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: Option<u8>,
        /// New assignee profile ID.
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Delete a task.
    Delete {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Assign a task to a user.
    Assign {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
        /// Assignee profile ID (omit to unassign).
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Mark a task as complete.
    Complete {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Move a task to a different list.
    Move {
        /// Source task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
        /// Target task list ID.
        #[arg(long)]
        target_list_id: String,
        /// Sort order in the target list.
        #[arg(long)]
        sort_order: Option<u32>,
    },
    /// Bulk change status for multiple tasks in a list.
    #[command(name = "bulk-status")]
    BulkStatus {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Comma-separated task IDs.
        #[arg(long)]
        task_ids: String,
        /// New status: pending, `in_progress`, complete, blocked.
        #[arg(long, value_parser = ["pending", "in_progress", "complete", "blocked"])]
        status: String,
    },
    /// Reorder tasks within a list.
    Reorder {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Comma-separated task IDs in desired order.
        #[arg(long)]
        task_ids: String,
    },
    /// Reorder task lists in a workspace or share.
    #[command(name = "reorder-lists")]
    ReorderLists {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace")]
        profile_type: String,
        /// Workspace or share ID.
        #[arg(long)]
        profile_id: String,
        /// Comma-separated list IDs in desired order.
        #[arg(long)]
        list_ids: String,
    },
    /// Manage task lists.
    #[command(subcommand)]
    Lists(TaskListCommands),
}

/// Task list subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TaskListCommands {
    /// List all task lists in a workspace or share.
    List {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new task list in a workspace or share.
    Create {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace")]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Task list name.
        name: String,
        /// Task list description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Update a task list.
    Update {
        /// Task list ID.
        list_id: String,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Delete a task list.
    Delete {
        /// Task list ID.
        list_id: String,
    },
}

// ─── Worklog ────────────────────────────────────────────────────────────────

/// Worklog subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorklogCommands {
    /// List worklog entries.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Entity type (profile, task, `task_list`). Defaults to "profile".
        #[arg(long, value_parser = ["profile", "task", "task_list"])]
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        #[arg(long)]
        entity_id: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Append a worklog entry.
    Append {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Message content.
        message: String,
        /// Entity type (profile, task, `task_list`). Defaults to "profile".
        #[arg(long, value_parser = ["profile", "task", "task_list"])]
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        #[arg(long)]
        entity_id: Option<String>,
    },
    /// Create an interjection (urgent entry requiring acknowledgement).
    Interject {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Message content.
        message: String,
        /// Entity type (profile, task, `task_list`). Defaults to "profile".
        #[arg(long, value_parser = ["profile", "task", "task_list"])]
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        #[arg(long)]
        entity_id: Option<String>,
    },
    /// Get worklog entry details.
    Details {
        /// Worklog entry ID.
        entry_id: String,
    },
    /// List unacknowledged interjections for an entity.
    #[command(name = "list-interjections")]
    ListInterjections {
        /// Entity type (profile, task, `task_list`).
        #[arg(long, value_parser = ["profile", "task", "task_list"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Acknowledge a worklog interjection.
    Acknowledge {
        /// Worklog entry ID to acknowledge.
        entry_id: String,
    },
}

// ─── Approval ───────────────────────────────────────────────────────────────

/// Approval subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ApprovalCommands {
    /// List approvals in a workspace.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filter by status: pending, approved, rejected.
        #[arg(long, value_parser = ["pending", "approved", "rejected"])]
        status: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Request an approval.
    Request {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Entity type: task, node, or `worklog_entry`.
        #[arg(long, value_parser = ["task", "node", "worklog_entry"])]
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Description of what needs approval.
        #[arg(long)]
        description: String,
        /// Designated approver profile ID.
        #[arg(long)]
        approver_id: Option<String>,
    },
    /// Approve an approval request.
    Approve {
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        #[arg(long)]
        comment: Option<String>,
    },
    /// Reject an approval request.
    Reject {
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        #[arg(long)]
        comment: Option<String>,
    },
}

// ─── Todo ───────────────────────────────────────────────────────────────────

/// Todo subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TodoCommands {
    /// List todos in a workspace or share.
    List {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace")]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new todo in a workspace or share.
    Create {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Todo title.
        title: String,
        /// Assignee profile ID.
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Update a todo.
    Update {
        /// Todo ID.
        todo_id: String,
        /// New title.
        #[arg(long)]
        title: Option<String>,
        /// Mark as done or not done.
        #[arg(long)]
        done: Option<bool>,
        /// New assignee profile ID.
        #[arg(long)]
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
    #[command(name = "bulk-toggle")]
    BulkToggle {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Comma-separated todo IDs.
        #[arg(long)]
        todo_ids: String,
        /// Set completion state (true = done, false = not done).
        #[arg(long, default_value = "true")]
        done: bool,
    },
}

// ─── Completions ─────────────────────────────────────────────────────────────

/// Supported shells for completion script generation.
#[derive(Clone, Copy, Debug, ValueEnum)]
#[non_exhaustive]
pub enum ShellType {
    /// Bash shell.
    Bash,
    /// Zsh shell.
    Zsh,
    /// Fish shell.
    Fish,
    /// `PowerShell`.
    Powershell,
}

impl std::fmt::Display for ShellType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bash => write!(f, "bash"),
            Self::Zsh => write!(f, "zsh"),
            Self::Fish => write!(f, "fish"),
            Self::Powershell => write!(f, "powershell"),
        }
    }
}

// ─── Configure ───────────────────────────────────────────────────────────────

/// Configuration management subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ConfigureCommands {
    /// Interactive profile setup.
    Init {
        /// Profile name to create or update.
        #[arg(long, default_value = "default")]
        name: String,
        /// API base URL.
        #[arg(long)]
        api_base: Option<String>,
        /// Authentication method: pkce, basic, or `api_key`.
        #[arg(long, value_parser = ["pkce", "basic", "api_key"])]
        auth_method: Option<String>,
    },
    /// List all configured profiles.
    List,
    /// Set the default profile.
    SetDefault {
        /// Profile name to set as default.
        name: String,
    },
    /// Show current configuration.
    Show,
    /// Delete a named profile.
    Delete {
        /// Profile name to delete.
        name: String,
    },
}

// ─── Apps ────────────────────────────────────────────────────────────────────

/// Apps subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum AppsCommands {
    /// List all available apps.
    List,
    /// Get details for a specific app.
    Details {
        /// App identifier.
        app_id: String,
    },
    /// Launch an app in a context.
    Launch {
        /// App identifier.
        app_id: String,
        /// Context type: workspace or share.
        #[arg(long)]
        context_type: String,
        /// Context ID.
        #[arg(long)]
        context_id: String,
    },
    /// List apps available for a specific tool.
    #[command(name = "tool-apps")]
    GetToolApps {
        /// Tool name.
        tool_name: String,
    },
}

// ─── Import ──────────────────────────────────────────────────────────────────

/// Cloud import subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ImportCommands {
    /// List available cloud import providers.
    #[command(name = "list-providers")]
    ListProviders {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
    },
    /// List provider identities.
    #[command(name = "list-identities")]
    ListIdentities {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Provision a new provider identity.
    #[command(name = "provision-identity")]
    ProvisionIdentity {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Cloud provider: `google_drive`, box, `onedrive_business`, dropbox.
        #[arg(long)]
        provider: String,
    },
    /// Get identity details.
    #[command(name = "identity-details")]
    IdentityDetails {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
    },
    /// Revoke a provider identity.
    #[command(name = "revoke-identity")]
    RevokeIdentity {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
    },
    /// List import sources.
    #[command(name = "list-sources")]
    ListSources {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filter by status.
        #[arg(long)]
        status: Option<String>,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Discover shared folders from a provider.
    Discover {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
    },
    /// Create an import source.
    #[command(name = "create-source")]
    CreateSource {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
        /// Remote folder path.
        #[arg(long)]
        remote_path: String,
        /// Display name.
        #[arg(long)]
        remote_name: Option<String>,
        /// Sync interval in seconds (300-86400).
        #[arg(long)]
        sync_interval: Option<u32>,
        /// Access mode: `read_only` or `read_write`.
        #[arg(long)]
        access_mode: Option<String>,
    },
    /// Get source details.
    #[command(name = "source-details")]
    SourceDetails {
        /// Source ID.
        source_id: String,
    },
    /// Update source settings.
    #[command(name = "update-source")]
    UpdateSource {
        /// Source ID.
        source_id: String,
        /// Sync interval in seconds.
        #[arg(long)]
        sync_interval: Option<u32>,
        /// Status action: paused or synced.
        #[arg(long)]
        status: Option<String>,
        /// Display name.
        #[arg(long)]
        remote_name: Option<String>,
        /// Access mode: `read_only` or `read_write`.
        #[arg(long)]
        access_mode: Option<String>,
    },
    /// Delete a source.
    #[command(name = "delete-source")]
    DeleteSource {
        /// Source ID.
        source_id: String,
    },
    /// Disconnect source with keep/delete.
    Disconnect {
        /// Source ID.
        source_id: String,
        /// Action: keep or delete.
        #[arg(long, value_parser = ["keep", "delete"])]
        action: String,
    },
    /// Trigger immediate refresh sync.
    Refresh {
        /// Source ID.
        source_id: String,
    },
    /// List jobs for a source.
    #[command(name = "list-jobs")]
    ListJobs {
        /// Source ID.
        source_id: String,
        /// Max results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get job details.
    #[command(name = "job-details")]
    JobDetails {
        /// Source ID.
        source_id: String,
        /// Job ID.
        #[arg(long)]
        job_id: String,
    },
    /// Cancel a running job.
    #[command(name = "cancel-job")]
    CancelJob {
        /// Source ID.
        source_id: String,
        /// Job ID.
        #[arg(long)]
        job_id: String,
    },
    /// List write-back jobs.
    #[command(name = "list-writebacks")]
    ListWritebacks {
        /// Source ID.
        source_id: String,
        /// Filter by status.
        #[arg(long)]
        status: Option<String>,
        /// Max results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get write-back details.
    #[command(name = "writeback-details")]
    WritebackDetails {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
    },
    /// Push a file to remote storage.
    #[command(name = "push-writeback")]
    PushWriteback {
        /// Source ID.
        source_id: String,
        /// Node ID.
        #[arg(long)]
        node_id: String,
    },
    /// Retry a failed write-back.
    #[command(name = "retry-writeback")]
    RetryWriteback {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
    },
    /// Resolve a write-back conflict.
    #[command(name = "resolve-conflict")]
    ResolveConflict {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
        /// Resolution: `keep_local` or `keep_remote`.
        #[arg(long, value_parser = ["keep_local", "keep_remote"])]
        resolution: String,
    },
    /// Cancel a pending write-back.
    #[command(name = "cancel-writeback")]
    CancelWriteback {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
    },
}

// ─── Lock ────────────────────────────────────────────────────────────────────

/// File locking subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum LockCommands {
    /// Acquire an exclusive lock on a file.
    Acquire {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
    },
    /// Check lock status for a file.
    Status {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
    },
    /// Release a lock on a file.
    Release {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        #[arg(long)]
        lock_token: String,
    },
    /// Renew (heartbeat) an existing lock on a file.
    Heartbeat {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        #[arg(long)]
        lock_token: String,
    },
}

// ─── Metadata ─────────────────────────────────────────────────────────────────

/// Metadata extraction subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum MetadataCommands {
    /// List files eligible for metadata extraction.
    Eligible {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add files to a metadata template.
    #[command(name = "add-nodes")]
    AddNodes {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// JSON-encoded array of node IDs.
        #[arg(long)]
        node_ids: String,
    },
    /// Remove files from a metadata template.
    #[command(name = "remove-nodes")]
    RemoveNodes {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// JSON-encoded array of node IDs.
        #[arg(long)]
        node_ids: String,
    },
    /// List files mapped to a metadata template.
    #[command(name = "list-nodes")]
    ListNodes {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
        /// Template field name to sort by (optional).
        #[arg(long)]
        sort_field: Option<String>,
        /// Sort direction when --sort-field is set (asc or desc).
        #[arg(long, value_parser = ["asc", "desc"], requires = "sort_field")]
        sort_dir: Option<String>,
    },
    /// AI-based file matching for a template.
    #[command(name = "auto-match")]
    AutoMatch {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
    },
    /// Batch extract metadata for all files in a template.
    #[command(name = "extract-all")]
    ExtractAll {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
    },
    /// Enqueue an async metadata extraction for a single file. Usually
    /// returns a `job_id`; poll `workspace jobs-status` until status is
    /// "completed", then read values from the metadata details endpoint.
    /// A full-row call whose effective scope is empty (every template
    /// field has `autoextract: false`) responds successfully without
    /// enqueueing a job — do not assume a `job_id` is always present.
    Extract {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// File node ID.
        #[arg(long)]
        node_id: String,
        /// Template ID to extract against.
        #[arg(long)]
        template_id: String,
        /// JSON-encoded array of field names for partial extraction
        /// (omit for full-row extraction).
        #[arg(long)]
        fields: Option<String>,
    },
    /// Preview files that would match a proposed template name + description.
    #[command(name = "preview-match")]
    PreviewMatch {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Proposed template name (1-255 chars).
        #[arg(long)]
        name: String,
        /// Natural-language description of the view/template.
        #[arg(long)]
        description: String,
    },
    /// Suggest custom columns for a proposed template (AI-assisted).
    #[command(name = "suggest-fields")]
    SuggestFields {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// JSON-encoded array of 1-25 sample node IDs from preview-match.
        #[arg(long)]
        node_ids: String,
        /// View description (also passed to preview-match).
        #[arg(long)]
        description: String,
        /// Optional short hint ("photo collection", max 64 chars, letters/numbers/spaces).
        #[arg(long)]
        user_context: Option<String>,
    },
    /// Create a metadata template (a.k.a. "view").
    #[command(name = "create-template")]
    CreateTemplate {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template name (shown as "view name" in the UI).
        #[arg(long)]
        name: String,
        /// Template description.
        #[arg(long)]
        description: String,
        /// Template category.
        #[arg(long)]
        category: String,
        /// JSON-encoded array of column definitions (compatible with suggest-fields output).
        #[arg(long)]
        fields: String,
    },
}

// ─── System ───────────────────────────────────────────────────────────────────

/// System health subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SystemCommands {
    /// Health check (no authentication required).
    Ping,
    /// System status (no authentication required).
    Status,
}

// ─── Manual Debug impls (redact sensitive fields) ────────────────────────────

impl fmt::Debug for Cli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cli")
            .field("format", &self.format)
            .field("fields", &self.fields)
            .field("no_color", &self.no_color)
            .field("quiet", &self.quiet)
            .field("verbose", &self.verbose)
            .field("profile", &self.profile)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("api_base", &self.api_base)
            .field("command", &self.command)
            .finish()
    }
}

impl fmt::Debug for AuthCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Login { email, password: _ } => f
                .debug_struct("Login")
                .field("email", email)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::Signup {
                email,
                password: _,
                first_name,
                last_name,
            } => f
                .debug_struct("Signup")
                .field("email", email)
                .field("password", &"[REDACTED]")
                .field("first_name", first_name)
                .field("last_name", last_name)
                .finish(),
            Self::PasswordReset {
                code,
                password1: _,
                password2: _,
            } => f
                .debug_struct("PasswordReset")
                .field("code", code)
                .field("password1", &"[REDACTED]")
                .field("password2", &"[REDACTED]")
                .finish(),
            Self::Logout => write!(f, "Logout"),
            Self::Status => write!(f, "Status"),
            Self::Verify { email, code } => f
                .debug_struct("Verify")
                .field("email", email)
                .field("code", code)
                .finish(),
            Self::TwoFa(cmds) => f.debug_tuple("TwoFa").field(cmds).finish(),
            Self::ApiKey(cmds) => f.debug_tuple("ApiKey").field(cmds).finish(),
            Self::Check => write!(f, "Check"),
            Self::Session => write!(f, "Session"),
            Self::EmailCheck { email } => {
                f.debug_struct("EmailCheck").field("email", email).finish()
            }
            Self::PasswordResetRequest { email } => f
                .debug_struct("PasswordResetRequest")
                .field("email", email)
                .finish(),
            Self::Oauth(cmds) => f.debug_tuple("Oauth").field(cmds).finish(),
            Self::Scopes => write!(f, "Scopes"),
            Self::PasswordResetCheck { code } => f
                .debug_struct("PasswordResetCheck")
                .field("code", code)
                .finish(),
            #[allow(unreachable_patterns)]
            _ => write!(f, "AuthCommands(<unknown variant>)"),
        }
    }
}
