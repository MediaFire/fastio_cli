/// User command implementations for `fastio user *`.
///
/// Handles user profile info, update, avatar management, and settings.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// User subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum UserCommand {
    /// Get current user profile.
    Info,
    /// Update user profile.
    Update {
        /// First name.
        first_name: Option<String>,
        /// Last name.
        last_name: Option<String>,
        /// Display name (alias for first name).
        display_name: Option<String>,
    },
    /// Avatar subcommands.
    Avatar(AvatarCommand),
    /// Settings subcommands.
    Settings(SettingsCommand),
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
    OrgLimits,
    /// List the user's shares.
    Shares,
    /// User invitations subcommands.
    Invitations(UserInvitationsCommand),
    /// User asset subcommands.
    Asset(UserAssetCommand),
    /// Enable or disable photo auto-sync.
    Autosync {
        /// State: "enable" or "disable".
        state: String,
    },
    /// Get support PIN and identity hash.
    Pin,
    /// Validate a phone number.
    Phone {
        /// Country code (e.g. "1").
        country_code: String,
        /// Phone number (e.g. "5551234567").
        phone_number: String,
    },
}

/// User invitations subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum UserInvitationsCommand {
    /// List pending invitations.
    List,
    /// Get invitation details.
    Details {
        /// Invitation ID.
        invitation_id: String,
    },
    /// Accept all pending invitations.
    AcceptAll,
}

/// User asset subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum UserAssetCommand {
    /// List asset types.
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
    /// Upload a user asset.
    Upload {
        /// Asset type name (e.g. `profile_pic`).
        asset_type: String,
        /// Path to the file to upload.
        file: String,
    },
    /// Read/download a user asset binary.
    Read {
        /// User ID.
        user_id: String,
        /// Asset type name (e.g. `profile_pic`).
        asset_type: String,
        /// Output file path.
        output: String,
    },
}

/// Avatar subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AvatarCommand {
    /// Upload an avatar image.
    Upload {
        /// Path to the image file.
        file: String,
    },
    /// Remove the avatar.
    Remove,
}

/// Settings subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SettingsCommand {
    /// Get user settings.
    Get,
    /// Update user settings.
    Update {
        /// First name.
        first_name: Option<String>,
        /// Last name.
        last_name: Option<String>,
    },
}

/// Execute a user subcommand.
pub async fn execute(command: &UserCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        UserCommand::Info => info(ctx).await,
        UserCommand::Update {
            first_name,
            last_name,
            display_name,
        } => {
            update(
                ctx,
                first_name.as_deref(),
                last_name.as_deref(),
                display_name.as_deref(),
            )
            .await
        }
        UserCommand::Avatar(cmd) => avatar(cmd, ctx).await,
        UserCommand::Settings(cmd) => settings(cmd, ctx).await,
        UserCommand::Search { query } => {
            let client = ctx.build_client()?;
            let value = api::user::search_users(&client, query)
                .await
                .context("failed to search users")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        UserCommand::Close { confirmation } => {
            let client = ctx.build_client()?;
            let value = api::user::close_account(&client, confirmation)
                .await
                .context("failed to close account")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        UserCommand::Details { user_id } => {
            let client = ctx.build_client()?;
            let value = api::user::get_user_by_id(&client, user_id)
                .await
                .context("failed to get user details")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        UserCommand::Profiles => {
            let client = ctx.build_client()?;
            let value = api::user::get_profiles(&client)
                .await
                .context("failed to get profiles")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        UserCommand::Allowed => {
            let client = ctx.build_client()?;
            let value = api::user::user_allowed(&client)
                .await
                .context("failed to check country authorization")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        UserCommand::OrgLimits => {
            let client = ctx.build_client()?;
            let value = api::user::user_org_limits(&client)
                .await
                .context("failed to check org limits")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        UserCommand::Shares => {
            let client = ctx.build_client()?;
            let value = api::user::list_user_shares(&client)
                .await
                .context("failed to list user shares")?;
            ctx.output.render(&value)?;
            Ok(())
        }
        UserCommand::Invitations(cmd) => user_invitations(cmd, ctx).await,
        UserCommand::Asset(cmd) => user_asset(cmd, ctx).await,
        UserCommand::Autosync { state } => autosync(ctx, state).await,
        UserCommand::Pin => pin(ctx).await,
        UserCommand::Phone {
            country_code,
            phone_number,
        } => phone(ctx, country_code, phone_number).await,
    }
}

/// Handle user invitation subcommands.
async fn user_invitations(cmd: &UserInvitationsCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match cmd {
        UserInvitationsCommand::List => {
            let value = api::user::list_invitations(&client, None)
                .await
                .context("failed to list invitations")?;
            ctx.output.render(&value)?;
        }
        UserInvitationsCommand::Details { invitation_id } => {
            let value = api::user::get_invitation_details(&client, invitation_id)
                .await
                .context("failed to get invitation details")?;
            ctx.output.render(&value)?;
        }
        UserInvitationsCommand::AcceptAll => {
            let value = api::user::accept_all_invitations(&client, None)
                .await
                .context("failed to accept all invitations")?;
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Handle user asset subcommands.
async fn user_asset(cmd: &UserAssetCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match cmd {
        UserAssetCommand::Types => {
            let value = api::user::get_asset_types(&client)
                .await
                .context("failed to get asset types")?;
            ctx.output.render(&value)?;
        }
        UserAssetCommand::List { user_id } => {
            let value = api::user::list_user_assets(&client, user_id)
                .await
                .context("failed to list user assets")?;
            ctx.output.render(&value)?;
        }
        UserAssetCommand::Delete { asset_type } => {
            let me = api::user::get_me(&client)
                .await
                .context("failed to get current user")?;
            let user_id = me
                .get("id")
                .or_else(|| me.get("profile_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("could not determine user ID"))?;
            api::user::delete_user_asset(&client, user_id, asset_type)
                .await
                .context("failed to delete user asset")?;
            let value = json!({
                "status": "deleted",
                "asset_type": asset_type,
            });
            ctx.output.render(&value)?;
        }
        UserAssetCommand::Upload { asset_type, file } => {
            let me = api::user::get_me(&client)
                .await
                .context("failed to get current user")?;
            let user_id = me
                .get("id")
                .or_else(|| me.get("profile_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("could not determine user ID"))?;
            let value = api::user::upload_user_asset(&client, user_id, asset_type, file)
                .await
                .context("failed to upload user asset")?;
            ctx.output.render(&value)?;
        }
        UserAssetCommand::Read {
            user_id,
            asset_type,
            output,
        } => {
            let bytes = api::user::read_user_asset(
                &client,
                user_id,
                asset_type,
                std::path::Path::new(output.as_str()),
            )
            .await
            .context("failed to read user asset")?;
            let value = json!({
                "status": "downloaded",
                "asset_type": asset_type,
                "output": output,
                "bytes": bytes,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Get current user profile.
async fn info(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::user::get_me(&client)
        .await
        .context("failed to get user info")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Update user profile fields.
async fn update(
    ctx: &CommandContext<'_>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    display_name: Option<&str>,
) -> Result<()> {
    let effective_first = first_name.or(display_name);

    if effective_first.is_none() && last_name.is_none() {
        anyhow::bail!("at least one of --first-name, --last-name, or --display-name is required");
    }

    let client = ctx.build_client()?;
    let value = api::user::update_user(&client, effective_first, last_name, None)
        .await
        .context("failed to update user profile")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Handle avatar subcommands.
async fn avatar(cmd: &AvatarCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        AvatarCommand::Upload { file } => {
            // First get the user ID
            let me = api::user::get_me(&client)
                .await
                .context("failed to get current user")?;
            let user_id = me
                .get("id")
                .or_else(|| me.get("profile_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("could not determine user ID"))?;

            let value = api::user::upload_user_asset(&client, user_id, "profile_pic", file)
                .await
                .context("failed to upload avatar")?;
            ctx.output.render(&value)?;
        }
        AvatarCommand::Remove => {
            let me = api::user::get_me(&client)
                .await
                .context("failed to get current user")?;
            let user_id = me
                .get("id")
                .or_else(|| me.get("profile_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("could not determine user ID"))?;

            api::user::delete_user_asset(&client, user_id, "profile_pic")
                .await
                .context("failed to remove avatar")?;

            let value = json!({
                "status": "removed",
                "asset": "profile_pic",
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Handle settings subcommands.
async fn settings(cmd: &SettingsCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;

    match cmd {
        SettingsCommand::Get => {
            let value = api::user::get_me(&client)
                .await
                .context("failed to get user settings")?;
            ctx.output.render(&value)?;
        }
        SettingsCommand::Update {
            first_name,
            last_name,
        } => {
            if first_name.is_none() && last_name.is_none() {
                anyhow::bail!("at least one of --first-name or --last-name is required");
            }
            let value =
                api::user::update_user(&client, first_name.as_deref(), last_name.as_deref(), None)
                    .await
                    .context("failed to update user settings")?;
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Enable or disable photo auto-sync.
async fn autosync(ctx: &CommandContext<'_>, state: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::user::autosync(&client, state)
        .await
        .context("failed to set autosync state")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get support PIN and identity hash.
async fn pin(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::user::get_pin(&client)
        .await
        .context("failed to get support PIN")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Validate a phone number.
async fn phone(ctx: &CommandContext<'_>, country_code: &str, phone_number: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::user::validate_phone(&client, country_code, phone_number)
        .await
        .context("failed to validate phone number")?;
    ctx.output.render(&value)?;
    Ok(())
}
