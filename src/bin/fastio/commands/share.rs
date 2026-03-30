/// Share command implementations for `fastio share *`.
///
/// Handles share CRUD, share file operations, and share member management.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

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
    Create {
        /// Workspace ID to create the share in.
        workspace_id: String,
        /// Share name/title.
        name: String,
        /// Description.
        description: Option<String>,
        /// Access options.
        access_options: Option<String>,
        /// Password for share access.
        password: Option<String>,
        /// Enable anonymous uploads.
        anonymous_uploads: Option<bool>,
        /// Enable AI intelligence features.
        intelligence: Option<bool>,
        /// Download security level: "high", "medium", or "off".
        download_security: Option<String>,
    },
    /// Get share details.
    Info {
        /// Share ID.
        share_id: String,
    },
    /// Update share settings.
    Update {
        /// Share ID.
        share_id: String,
        /// New name.
        name: Option<String>,
        /// New description.
        description: Option<String>,
        /// New access options.
        access_options: Option<String>,
        /// Enable/disable downloads (legacy — prefer `download_security`).
        download_enabled: Option<bool>,
        /// Enable/disable comments.
        comments_enabled: Option<bool>,
        /// Enable/disable anonymous uploads.
        anonymous_uploads: Option<bool>,
        /// Download security level: "high", "medium", or "off".
        download_security: Option<String>,
    },
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
    Files(ShareFilesCommand),
    /// Share member operations.
    Members(ShareMembersCommand),
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
    /// Add a member to a share.
    Add {
        /// Share ID.
        share_id: String,
        /// Email address.
        email: String,
        /// Permission role.
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

/// Allowed page sizes for storage list endpoints.
const VALID_PAGE_SIZES: &[u32] = &[100, 250, 500];

/// Valid roles for share membership.
const VALID_SHARE_ROLES: &[&str] = &["admin", "member", "guest"];

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

/// Validate that a role is one of the accepted values.
fn validate_role(role: &str) -> Result<()> {
    anyhow::ensure!(
        VALID_SHARE_ROLES.contains(&role),
        "invalid role '{role}'. Must be one of: admin, member, guest"
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
        ShareCommand::Create {
            workspace_id,
            name,
            description,
            access_options,
            password,
            anonymous_uploads,
            intelligence,
            download_security,
        } => {
            create(
                ctx,
                workspace_id,
                name,
                description.as_deref(),
                access_options.as_deref(),
                password.as_deref(),
                *anonymous_uploads,
                *intelligence,
                download_security.as_deref(),
            )
            .await
        }
        ShareCommand::Info { share_id } => {
            validate_share_id(share_id)?;
            info(ctx, share_id).await
        }
        ShareCommand::Update {
            share_id,
            name,
            description,
            access_options,
            download_enabled,
            comments_enabled,
            anonymous_uploads,
            download_security,
        } => {
            validate_share_id(share_id)?;
            update(
                ctx,
                share_id,
                name.as_deref(),
                description.as_deref(),
                access_options.as_deref(),
                *download_enabled,
                *comments_enabled,
                *anonymous_uploads,
                download_security.as_deref(),
            )
            .await
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
        ShareCommand::WorkflowEnable { share_id } => {
            validate_share_id(share_id)?;
            workflow_enable(ctx, share_id).await
        }
        ShareCommand::WorkflowDisable { share_id } => {
            validate_share_id(share_id)?;
            workflow_disable(ctx, share_id).await
        }
        ShareCommand::Files(cmd) => files(cmd, ctx).await,
        ShareCommand::Members(cmd) => members(cmd, ctx).await,
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
#[allow(clippy::too_many_arguments)]
async fn create(
    ctx: &CommandContext<'_>,
    workspace_id: &str,
    name: &str,
    description: Option<&str>,
    access_options: Option<&str>,
    password: Option<&str>,
    anonymous_uploads: Option<bool>,
    intelligence: Option<bool>,
    download_security: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::create_share(
        &client,
        &api::share::CreateShareParams {
            workspace_id,
            title: name,
            description,
            access_options,
            password,
            anonymous_uploads_enabled: anonymous_uploads,
            intelligence,
            download_security,
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

/// Update share settings.
#[allow(clippy::too_many_arguments)]
async fn update(
    ctx: &CommandContext<'_>,
    share_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    access_options: Option<&str>,
    download_enabled: Option<bool>,
    comments_enabled: Option<bool>,
    anonymous_uploads: Option<bool>,
    download_security: Option<&str>,
) -> Result<()> {
    if name.is_none()
        && description.is_none()
        && access_options.is_none()
        && download_enabled.is_none()
        && comments_enabled.is_none()
        && anonymous_uploads.is_none()
        && download_security.is_none()
    {
        anyhow::bail!(
            "at least one update field is required (--name, --description, --access-options, --download-enabled, --download-security, --comments-enabled, --anonymous-uploads)"
        );
    }
    let client = ctx.build_client()?;
    let value = api::share::update_share(
        &client,
        &api::share::UpdateShareParams {
            share_id,
            name,
            description,
            access_options,
            download_enabled,
            comments_enabled,
            anonymous_uploads_enabled: anonymous_uploads,
            download_security,
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
        } => {
            validate_share_id(share_id)?;
            validate_email(email)?;
            if let Some(r) = role {
                validate_role(r)?;
            }
            let value = api::share::add_share_member(&client, share_id, email, role.as_deref())
                .await
                .context("failed to add share member")?;
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

/// Enable workflow on a share.
async fn workflow_enable(ctx: &CommandContext<'_>, share_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::enable_share_workflow(&client, share_id)
        .await
        .context("failed to enable share workflow")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Disable workflow on a share.
async fn workflow_disable(ctx: &CommandContext<'_>, share_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::share::disable_share_workflow(&client, share_id)
        .await
        .context("failed to disable share workflow")?;
    ctx.output.render(&value)?;
    Ok(())
}
