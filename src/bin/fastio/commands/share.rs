/// Share command implementations for `fastio share *`.
///
/// Handles share CRUD, share file operations, and share member management.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Arguments for creating a share (boxed into [`ShareCommand::Create`] because
/// the full documented create surface is large).
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct ShareCreateArgs {
    /// Workspace ID to create the share in.
    pub workspace_id: String,
    /// Share display title.
    pub name: String,
    /// Share direction type: `send`, `receive`, `exchange`.
    pub share_type: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Access options.
    pub access_options: Option<String>,
    /// Who can manage invitations: `owners`, `guests`.
    pub invite: Option<String>,
    /// Storage mode: `independent`, `workspace_folder`.
    pub storage_mode: Option<String>,
    /// Backing workspace folder opaque ID.
    pub folder_node_id: Option<String>,
    /// Create a new backing folder.
    pub create_folder: Option<bool>,
    /// Name for the new backing folder.
    pub folder_name: Option<String>,
    /// URL-friendly custom name.
    pub custom_name: Option<String>,
    /// Password for share access.
    pub password: Option<String>,
    /// Expiration datetime.
    pub expires: Option<String>,
    /// Notification preference.
    pub notify: Option<String>,
    /// Enable comments.
    pub comments_enabled: Option<bool>,
    /// Enable guest AI chat.
    pub guest_chat_enabled: Option<bool>,
    /// Visual display mode.
    pub display_type: Option<String>,
    /// Workspace visual style.
    pub workspace_style: Option<String>,
    /// Enable anonymous uploads.
    pub anonymous_uploads: Option<bool>,
    /// Enable AI intelligence features (default false).
    pub intelligence: Option<bool>,
    /// Download security level ("high", "medium", or "off").
    pub download_security: Option<String>,
    /// Accent color (JSON color object).
    pub accent_color: Option<String>,
    /// Primary background color (JSON color object).
    pub background_color1: Option<String>,
    /// Secondary background color (JSON color object).
    pub background_color2: Option<String>,
    /// Background image selection (numeric).
    pub background_image: Option<i64>,
    /// Custom link #1 (JSON link object).
    pub link_1: Option<String>,
    /// Custom link #2 (JSON link object).
    pub link_2: Option<String>,
    /// Custom link #3 (JSON link object).
    pub link_3: Option<String>,
    /// Custom owner-defined properties (JSON or "null").
    pub owner_defined: Option<String>,
}

/// Arguments for updating a share (boxed into [`ShareCommand::Update`]).
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct ShareUpdateArgs {
    /// Share ID.
    pub share_id: String,
    /// New share display name.
    pub name: Option<String>,
    /// New display title.
    pub title: Option<String>,
    /// New URL-friendly custom name.
    pub custom_name: Option<String>,
    /// New description.
    pub description: Option<String>,
    /// Share direction type.
    pub share_type: Option<String>,
    /// New access options.
    pub access_options: Option<String>,
    /// Who can manage invitations.
    pub invite: Option<String>,
    /// Password; "null"/"" to clear.
    pub password: Option<String>,
    /// Expiration datetime; "null" to clear.
    pub expires: Option<String>,
    /// Notification preference.
    pub notify: Option<String>,
    /// Enable/disable downloads (legacy — prefer `download_security`).
    pub download_enabled: Option<bool>,
    /// Enable/disable comments.
    pub comments_enabled: Option<bool>,
    /// Download security level ("high", "medium", or "off").
    pub download_security: Option<String>,
    /// Visual display mode.
    pub display_type: Option<String>,
    /// Workspace visual style.
    pub workspace_style: Option<String>,
    /// Enable/disable guest AI chat.
    pub guest_chat_enabled: Option<bool>,
    /// Toggle AI indexing (intelligence).
    pub intelligence: Option<bool>,
    /// Enable/disable anonymous uploads.
    pub anonymous_uploads: Option<bool>,
    /// Accent color (JSON color object), or "null".
    pub accent_color: Option<String>,
    /// Primary background color (JSON color object), or "null".
    pub background_color1: Option<String>,
    /// Secondary background color (JSON color object), or "null".
    pub background_color2: Option<String>,
    /// Background image selection (numeric).
    pub background_image: Option<i64>,
    /// Custom link #1 (JSON link object), or "null".
    pub link_1: Option<String>,
    /// Custom link #2 (JSON link object), or "null".
    pub link_2: Option<String>,
    /// Custom link #3 (JSON link object), or "null".
    pub link_3: Option<String>,
    /// Custom owner-defined properties (JSON or "null").
    pub owner_defined: Option<String>,
    /// Remove the workspace share-link node (pass "null" — the only accepted
    /// value).
    pub share_link_node_id: Option<String>,
}

/// Share subcommand variants.
#[derive(Clone)]
#[non_exhaustive]
pub enum ShareCommand {
    /// List all shares.
    List {
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create a share.
    Create(Box<ShareCreateArgs>),
    /// Get share details.
    Info {
        /// Share ID.
        share_id: String,
    },
    /// Update share settings.
    Update(Box<ShareUpdateArgs>),
    /// Delete a share.
    Delete {
        /// Share ID.
        share_id: String,
        /// Confirmation string.
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
    /// Share file operations.
    Files(ShareFilesCommand),
    /// Share member operations.
    Members(ShareMembersCommand),
    /// Share invitation operations.
    Invitation(ShareInvitationCommand),
}

/// Share files subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ShareFilesCommand {
    /// List files in a share.
    List {
        /// Share ID.
        share_id: String,
        /// Parent folder node ID.
        folder: Option<String>,
        /// Sort column.
        sort_by: Option<String>,
        /// Sort direction.
        sort_dir: Option<String>,
        /// Page size.
        page_size: Option<u32>,
        /// Cursor for pagination.
        cursor: Option<String>,
    },
}

/// Share members subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ShareMembersCommand {
    /// List share members.
    List {
        /// Share ID.
        share_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Add a member (user ID) or invite a member (email) to a share.
    Add {
        /// Share ID.
        share_id: String,
        /// Email address (invite) or 19-digit user ID (add existing user).
        email: String,
        /// Permission role.
        role: Option<String>,
        /// Notification preference (existing-user add).
        notify_options: Option<String>,
        /// Membership expiration; "null"/"" to clear.
        expires: Option<String>,
        /// Resend notification email.
        force_notification: Option<bool>,
        /// Custom message for the invitation email.
        message: Option<String>,
        /// Invitation expiration datetime.
        invitation_expires: Option<String>,
    },
    /// Update a member's permissions, notification preference, or expiration.
    Update {
        /// Share ID.
        share_id: String,
        /// Member user ID.
        member_id: String,
        /// New permission role.
        role: Option<String>,
        /// Notification preference.
        notify_options: Option<String>,
        /// Membership expiration; "null"/"" to clear.
        expires: Option<String>,
    },
    /// Get member details.
    Info {
        /// Share ID.
        share_id: String,
        /// Member user ID.
        member_id: String,
    },
    /// Transfer share ownership to another member.
    Transfer {
        /// Share ID.
        share_id: String,
        /// Member user ID to transfer ownership to.
        member_id: String,
    },
    /// Leave a share (self-removal).
    Leave {
        /// Share ID.
        share_id: String,
    },
    /// Self-join a share.
    Join {
        /// Share ID.
        share_id: String,
    },
    /// Remove a member from a share.
    Remove {
        /// Share ID.
        share_id: String,
        /// Member ID to remove.
        member_id: String,
    },
}

/// Share invitation subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ShareInvitationCommand {
    /// List a share's invitations (optionally filtered by state).
    List {
        /// Share ID.
        share_id: String,
        /// Filter by state.
        state: Option<String>,
    },
    /// Update a share invitation.
    Update {
        /// Share ID.
        share_id: String,
        /// Invitation ID (numeric) or email address.
        invitation_id: String,
        /// New state.
        state: Option<String>,
        /// New permission role.
        role: Option<String>,
        /// Notification preference.
        notify_options: Option<String>,
        /// Membership expiration datetime.
        expires: Option<String>,
    },
    /// Revoke (delete) a share invitation.
    Delete {
        /// Share ID.
        share_id: String,
        /// Invitation ID (numeric) or email address.
        invitation_id: String,
    },
}

/// Allowed page sizes for storage list endpoints.
const VALID_PAGE_SIZES: &[u32] = &[100, 250, 500];

/// Valid roles for share membership (member-add / member-update /
/// invitation-update). `owner` is excluded — use transfer ownership.
const VALID_SHARE_ROLES: &[&str] = &["admin", "member", "guest", "view"];

/// Validate that a share ID is not empty or whitespace-only.
fn validate_share_id(share_id: &str) -> Result<()> {
    anyhow::ensure!(!share_id.trim().is_empty(), "share ID must not be empty");
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

/// Validate a member-add target, which may be an email (invite) OR a 19-digit
/// user ID (add an existing user). Anything containing `@` is checked as an
/// email; otherwise any non-empty token (e.g. a numeric user ID) is accepted.
fn validate_member_target(target: &str) -> Result<()> {
    anyhow::ensure!(!target.trim().is_empty(), "member target must not be empty");
    if target.contains('@') {
        validate_email(target)?;
    }
    Ok(())
}

/// Validate that a role is one of the accepted values.
fn validate_role(role: &str) -> Result<()> {
    anyhow::ensure!(
        VALID_SHARE_ROLES.contains(&role),
        "invalid role '{role}'. Must be one of: {}",
        VALID_SHARE_ROLES.join(", ")
    );
    Ok(())
}

/// Validate that a page size, if provided, is one of the accepted values.
fn validate_page_size(page_size: Option<u32>) -> Result<()> {
    if let Some(ps) = page_size {
        anyhow::ensure!(
            VALID_PAGE_SIZES.contains(&ps),
            "invalid page size {ps}. Must be one of: 100, 250, 500"
        );
    }
    Ok(())
}

/// Execute a share subcommand.
pub async fn execute(command: &ShareCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        ShareCommand::List { limit, offset } => list(ctx, *limit, *offset).await,
        ShareCommand::Create(args) => create(ctx, args).await,
        ShareCommand::Info { share_id } => {
            validate_share_id(share_id)?;
            info(ctx, share_id).await
        }
        ShareCommand::Update(args) => {
            validate_share_id(&args.share_id)?;
            update(ctx, args).await
        }
        ShareCommand::Delete { share_id, confirm } => {
            validate_share_id(share_id)?;
            delete(ctx, share_id, confirm).await
        }
        ShareCommand::Archive { share_id } => {
            validate_share_id(share_id)?;
            archive(ctx, share_id).await
        }
        ShareCommand::Unarchive { share_id } => {
            validate_share_id(share_id)?;
            unarchive(ctx, share_id).await
        }
        ShareCommand::PasswordAuth { share_id, password } => {
            validate_share_id(share_id)?;
            password_auth(ctx, share_id, password).await
        }
        ShareCommand::GuestAuth { share_id } => {
            validate_share_id(share_id)?;
            guest_auth(ctx, share_id).await
        }
        ShareCommand::PublicInfo { share_id } => {
            validate_share_id(share_id)?;
            public_info(ctx, share_id).await
        }
        ShareCommand::Available => available(ctx).await,
        ShareCommand::CheckName { name } => check_name(ctx, name).await,
        ShareCommand::Files(cmd) => files(cmd, ctx).await,
        ShareCommand::Members(cmd) => members(cmd, ctx).await,
        ShareCommand::Invitation(cmd) => invitations(cmd, ctx).await,
    }
}

/// List shares.
async fn list(ctx: &CommandContext<'_>, limit: Option<u32>, offset: Option<u32>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::list_shares(&client, limit, offset)
        .await
        .context("failed to list shares")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a share.
async fn create(ctx: &CommandContext<'_>, args: &ShareCreateArgs) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::create_share(
        &client,
        &api::share::CreateShareParams {
            workspace_id: &args.workspace_id,
            title: &args.name,
            share_type: args.share_type.as_deref(),
            description: args.description.as_deref(),
            access_options: args.access_options.as_deref(),
            invite: args.invite.as_deref(),
            storage_mode: args.storage_mode.as_deref(),
            folder_node_id: args.folder_node_id.as_deref(),
            create_folder: args.create_folder,
            folder_name: args.folder_name.as_deref(),
            custom_name: args.custom_name.as_deref(),
            password: args.password.as_deref(),
            expires: args.expires.as_deref(),
            notify: args.notify.as_deref(),
            comments_enabled: args.comments_enabled,
            download_security: args.download_security.as_deref(),
            guest_chat_enabled: args.guest_chat_enabled,
            display_type: args.display_type.as_deref(),
            workspace_style: args.workspace_style.as_deref(),
            anonymous_uploads_enabled: args.anonymous_uploads,
            // `intelligence` is required server-side; default to false (AI off).
            intelligence: args.intelligence.unwrap_or(false),
            accent_color: args.accent_color.as_deref(),
            background_color1: args.background_color1.as_deref(),
            background_color2: args.background_color2.as_deref(),
            background_image: args.background_image,
            link_1: args.link_1.as_deref(),
            link_2: args.link_2.as_deref(),
            link_3: args.link_3.as_deref(),
            owner_defined: args.owner_defined.as_deref(),
        },
    )
    .await
    .context("failed to create share")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get share details.
async fn info(ctx: &CommandContext<'_>, share_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::get_share_details(&client, share_id)
        .await
        .context("failed to get share details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Whether at least one updatable field is present on an update request.
fn update_has_any_field(a: &ShareUpdateArgs) -> bool {
    a.name.is_some()
        || a.title.is_some()
        || a.custom_name.is_some()
        || a.description.is_some()
        || a.share_type.is_some()
        || a.access_options.is_some()
        || a.invite.is_some()
        || a.password.is_some()
        || a.expires.is_some()
        || a.notify.is_some()
        || a.download_enabled.is_some()
        || a.comments_enabled.is_some()
        || a.download_security.is_some()
        || a.display_type.is_some()
        || a.workspace_style.is_some()
        || a.guest_chat_enabled.is_some()
        || a.intelligence.is_some()
        || a.anonymous_uploads.is_some()
        || a.accent_color.is_some()
        || a.background_color1.is_some()
        || a.background_color2.is_some()
        || a.background_image.is_some()
        || a.link_1.is_some()
        || a.link_2.is_some()
        || a.link_3.is_some()
        || a.owner_defined.is_some()
        || a.share_link_node_id.is_some()
}

/// Update share settings.
async fn update(ctx: &CommandContext<'_>, args: &ShareUpdateArgs) -> Result<()> {
    anyhow::ensure!(
        update_has_any_field(args),
        "at least one update field is required (e.g. --name, --title, --description, \
         --share-type, --access-options, --download-security, --comments-enabled, …)"
    );
    let client = ctx.build_client()?;
    let value = api::share::update_share(
        &client,
        &api::share::UpdateShareParams {
            share_id: &args.share_id,
            name: args.name.as_deref(),
            title: args.title.as_deref(),
            custom_name: args.custom_name.as_deref(),
            description: args.description.as_deref(),
            share_type: args.share_type.as_deref(),
            access_options: args.access_options.as_deref(),
            invite: args.invite.as_deref(),
            password: args.password.as_deref(),
            expires: args.expires.as_deref(),
            notify: args.notify.as_deref(),
            download_enabled: args.download_enabled,
            comments_enabled: args.comments_enabled,
            download_security: args.download_security.as_deref(),
            display_type: args.display_type.as_deref(),
            workspace_style: args.workspace_style.as_deref(),
            guest_chat_enabled: args.guest_chat_enabled,
            intelligence: args.intelligence,
            anonymous_uploads_enabled: args.anonymous_uploads,
            accent_color: args.accent_color.as_deref(),
            background_color1: args.background_color1.as_deref(),
            background_color2: args.background_color2.as_deref(),
            background_image: args.background_image,
            link_1: args.link_1.as_deref(),
            link_2: args.link_2.as_deref(),
            link_3: args.link_3.as_deref(),
            owner_defined: args.owner_defined.as_deref(),
            share_link_node_id: args.share_link_node_id.as_deref(),
        },
    )
    .await
    .context("failed to update share")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a share.
async fn delete(ctx: &CommandContext<'_>, share_id: &str, confirm: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::share::delete_share(&client, share_id, confirm)
        .await
        .context("failed to delete share")?;
    let value = json!({
        "status": "deleted",
        "share_id": share_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Handle share files subcommands.
async fn files(cmd: &ShareFilesCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        ShareFilesCommand::List {
            share_id,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => {
            validate_share_id(share_id)?;
            validate_page_size(*page_size)?;
            let value = api::share::list_share_files(
                &client,
                &api::share::ListShareFilesParams {
                    share_id,
                    parent_id: folder.as_deref().unwrap_or("root"),
                    sort_by: sort_by.as_deref(),
                    sort_dir: sort_dir.as_deref(),
                    page_size: *page_size,
                    cursor: cursor.as_deref(),
                },
            )
            .await
            .context("failed to list share files")?;
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Handle share members subcommands.
#[allow(clippy::too_many_lines)]
async fn members(cmd: &ShareMembersCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        ShareMembersCommand::List {
            share_id,
            limit,
            offset,
        } => {
            validate_share_id(share_id)?;
            let value = api::share::list_share_members(&client, share_id, *limit, *offset)
                .await
                .context("failed to list share members")?;
            ctx.output.render(&value)?;
        }
        ShareMembersCommand::Add {
            share_id,
            email,
            role,
            notify_options,
            expires,
            force_notification,
            message,
            invitation_expires,
        } => {
            validate_share_id(share_id)?;
            validate_member_target(email)?;
            if let Some(r) = role {
                validate_role(r)?;
            }
            let value = api::share::add_share_member(
                &client,
                &api::share::AddShareMemberParams {
                    share_id,
                    email_or_user_id: email,
                    permissions: role.as_deref(),
                    notify_options: notify_options.as_deref(),
                    expires: expires.as_deref(),
                    force_notification: *force_notification,
                    message: message.as_deref(),
                    invitation_expires: invitation_expires.as_deref(),
                },
            )
            .await
            .context("failed to add share member")?;
            ctx.output.render(&value)?;
        }
        ShareMembersCommand::Update {
            share_id,
            member_id,
            role,
            notify_options,
            expires,
        } => {
            validate_share_id(share_id)?;
            if let Some(r) = role {
                validate_role(r)?;
            }
            anyhow::ensure!(
                role.is_some() || notify_options.is_some() || expires.is_some(),
                "at least one of --role, --notify-options, or --expires is required"
            );
            let value = api::member::update_member(
                &client,
                "share",
                share_id,
                member_id,
                &api::member::UpdateMemberParams {
                    permissions: role.as_deref(),
                    notify_options: notify_options.as_deref(),
                    expires: expires.as_deref(),
                },
            )
            .await
            .context("failed to update share member")?;
            ctx.output.render(&value)?;
        }
        ShareMembersCommand::Info {
            share_id,
            member_id,
        } => {
            validate_share_id(share_id)?;
            let value = api::member::get_member_details(&client, "share", share_id, member_id)
                .await
                .context("failed to get share member details")?;
            ctx.output.render(&value)?;
        }
        ShareMembersCommand::Transfer {
            share_id,
            member_id,
        } => {
            validate_share_id(share_id)?;
            let value = api::member::transfer_ownership(&client, "share", share_id, member_id)
                .await
                .context("failed to transfer share ownership")?;
            ctx.output.render(&value)?;
        }
        ShareMembersCommand::Leave { share_id } => {
            validate_share_id(share_id)?;
            let value = api::member::leave(&client, "share", share_id)
                .await
                .context("failed to leave share")?;
            ctx.output.render(&value)?;
        }
        ShareMembersCommand::Join { share_id } => {
            validate_share_id(share_id)?;
            let value = api::member::join(&client, "share", share_id)
                .await
                .context("failed to join share")?;
            ctx.output.render(&value)?;
        }
        ShareMembersCommand::Remove {
            share_id,
            member_id,
        } => {
            validate_share_id(share_id)?;
            api::member::remove_member(&client, "share", share_id, member_id)
                .await
                .context("failed to remove share member")?;
            let value = json!({
                "status": "removed",
                "share_id": share_id,
                "member_id": member_id,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Handle share invitation subcommands.
async fn invitations(cmd: &ShareInvitationCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        ShareInvitationCommand::List { share_id, state } => {
            validate_share_id(share_id)?;
            let value =
                api::invitation::list_invitations(&client, "share", share_id, state.as_deref())
                    .await
                    .context("failed to list share invitations")?;
            ctx.output.render(&value)?;
        }
        ShareInvitationCommand::Update {
            share_id,
            invitation_id,
            state,
            role,
            notify_options,
            expires,
        } => {
            validate_share_id(share_id)?;
            if let Some(r) = role {
                validate_role(r)?;
            }
            anyhow::ensure!(
                state.is_some() || role.is_some() || notify_options.is_some() || expires.is_some(),
                "at least one of --state, --role, --notify-options, or --expires is required"
            );
            let value = api::invitation::update_invitation(
                &client,
                "share",
                share_id,
                invitation_id,
                &api::invitation::UpdateInvitationParams {
                    new_state: state.as_deref(),
                    permissions: role.as_deref(),
                    notify_options: notify_options.as_deref(),
                    expires: expires.as_deref(),
                },
            )
            .await
            .context("failed to update share invitation")?;
            ctx.output.render(&value)?;
        }
        ShareInvitationCommand::Delete {
            share_id,
            invitation_id,
        } => {
            validate_share_id(share_id)?;
            api::invitation::delete_invitation(&client, "share", share_id, invitation_id)
                .await
                .context("failed to delete share invitation")?;
            let value = json!({
                "status": "deleted",
                "share_id": share_id,
                "invitation_id": invitation_id,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Archive a share.
async fn archive(ctx: &CommandContext<'_>, share_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::archive_share(&client, share_id)
        .await
        .context("failed to archive share")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Unarchive a share.
async fn unarchive(ctx: &CommandContext<'_>, share_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::unarchive_share(&client, share_id)
        .await
        .context("failed to unarchive share")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Authenticate to a password-protected share.
async fn password_auth(ctx: &CommandContext<'_>, share_id: &str, password: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::password_auth_share(&client, share_id, password)
        .await
        .context("failed to authenticate to share")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Authenticate as a guest to a share.
async fn guest_auth(ctx: &CommandContext<'_>, share_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::guest_auth(&client, share_id)
        .await
        .context("failed to authenticate as guest to share")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get public details for a share.
async fn public_info(ctx: &CommandContext<'_>, share_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::get_share_public_details(&client, share_id)
        .await
        .context("failed to get share public details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List available shares for the current user.
async fn available(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::available_shares(&client)
        .await
        .context("failed to list available shares")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Check if a share name is available.
async fn check_name(ctx: &CommandContext<'_>, name: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::check_share_name(&client, name)
        .await
        .context("failed to check share name")?;
    ctx.output.render(&value)?;
    Ok(())
}
