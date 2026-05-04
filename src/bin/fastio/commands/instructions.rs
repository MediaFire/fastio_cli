/// AI instructions command implementations for `fastio instructions *`.
///
/// Manages the markdown blob of AI instructions stored against a user,
/// org, workspace, or share profile. Each non-user profile exposes both
/// a profile-wide slot (admin/owner only) and a per-user override slot
/// (`/me/`).
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Instructions subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum InstructionsCommand {
    /// Get the calling user's self-scoped AI instructions.
    GetUser,
    /// Set the calling user's self-scoped AI instructions.
    SetUser { content: String },
    /// Clear the calling user's self-scoped AI instructions.
    ClearUser,

    /// Get the org-wide AI instructions.
    GetOrg { org_id: String },
    /// Set the org-wide AI instructions.
    SetOrg { org_id: String, content: String },
    /// Clear the org-wide AI instructions.
    ClearOrg { org_id: String },
    /// Get the calling user's per-user override of an org's instructions.
    GetOrgUser { org_id: String },
    /// Set the calling user's per-user override of an org's instructions.
    SetOrgUser { org_id: String, content: String },
    /// Clear the calling user's per-user override of an org's instructions.
    ClearOrgUser { org_id: String },

    /// Get the workspace-wide AI instructions.
    GetWorkspace { workspace_id: String },
    /// Set the workspace-wide AI instructions.
    SetWorkspace {
        workspace_id: String,
        content: String,
    },
    /// Clear the workspace-wide AI instructions.
    ClearWorkspace { workspace_id: String },
    /// Get the calling user's per-user override of a workspace's instructions.
    GetWorkspaceUser { workspace_id: String },
    /// Set the calling user's per-user override of a workspace's instructions.
    SetWorkspaceUser {
        workspace_id: String,
        content: String,
    },
    /// Clear the calling user's per-user override of a workspace's instructions.
    ClearWorkspaceUser { workspace_id: String },

    /// Get the share-wide AI instructions.
    GetShare { share_id: String },
    /// Set the share-wide AI instructions.
    SetShare { share_id: String, content: String },
    /// Clear the share-wide AI instructions.
    ClearShare { share_id: String },
    /// Get the calling user's per-user override of a share's instructions.
    GetShareUser { share_id: String },
    /// Set the calling user's per-user override of a share's instructions.
    SetShareUser { share_id: String, content: String },
    /// Clear the calling user's per-user override of a share's instructions.
    ClearShareUser { share_id: String },
}

/// Execute an instructions subcommand.
#[allow(clippy::too_many_lines)]
pub async fn execute(command: &InstructionsCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = match command {
        InstructionsCommand::GetUser => api::instructions::get_user_instructions(&client)
            .await
            .context("failed to get user instructions")?,
        InstructionsCommand::SetUser { content } => {
            api::instructions::set_user_instructions(&client, content)
                .await
                .context("failed to set user instructions")?
        }
        InstructionsCommand::ClearUser => api::instructions::delete_user_instructions(&client)
            .await
            .context("failed to clear user instructions")?,

        InstructionsCommand::GetOrg { org_id } => {
            api::instructions::get_org_instructions(&client, org_id)
                .await
                .context("failed to get org instructions")?
        }
        InstructionsCommand::SetOrg { org_id, content } => {
            api::instructions::set_org_instructions(&client, org_id, content)
                .await
                .context("failed to set org instructions")?
        }
        InstructionsCommand::ClearOrg { org_id } => {
            api::instructions::delete_org_instructions(&client, org_id)
                .await
                .context("failed to clear org instructions")?
        }
        InstructionsCommand::GetOrgUser { org_id } => {
            api::instructions::get_org_user_instructions(&client, org_id)
                .await
                .context("failed to get per-user org instructions")?
        }
        InstructionsCommand::SetOrgUser { org_id, content } => {
            api::instructions::set_org_user_instructions(&client, org_id, content)
                .await
                .context("failed to set per-user org instructions")?
        }
        InstructionsCommand::ClearOrgUser { org_id } => {
            api::instructions::delete_org_user_instructions(&client, org_id)
                .await
                .context("failed to clear per-user org instructions")?
        }

        InstructionsCommand::GetWorkspace { workspace_id } => {
            api::instructions::get_workspace_instructions(&client, workspace_id)
                .await
                .context("failed to get workspace instructions")?
        }
        InstructionsCommand::SetWorkspace {
            workspace_id,
            content,
        } => api::instructions::set_workspace_instructions(&client, workspace_id, content)
            .await
            .context("failed to set workspace instructions")?,
        InstructionsCommand::ClearWorkspace { workspace_id } => {
            api::instructions::delete_workspace_instructions(&client, workspace_id)
                .await
                .context("failed to clear workspace instructions")?
        }
        InstructionsCommand::GetWorkspaceUser { workspace_id } => {
            api::instructions::get_workspace_user_instructions(&client, workspace_id)
                .await
                .context("failed to get per-user workspace instructions")?
        }
        InstructionsCommand::SetWorkspaceUser {
            workspace_id,
            content,
        } => api::instructions::set_workspace_user_instructions(&client, workspace_id, content)
            .await
            .context("failed to set per-user workspace instructions")?,
        InstructionsCommand::ClearWorkspaceUser { workspace_id } => {
            api::instructions::delete_workspace_user_instructions(&client, workspace_id)
                .await
                .context("failed to clear per-user workspace instructions")?
        }

        InstructionsCommand::GetShare { share_id } => {
            api::instructions::get_share_instructions(&client, share_id)
                .await
                .context("failed to get share instructions")?
        }
        InstructionsCommand::SetShare { share_id, content } => {
            api::instructions::set_share_instructions(&client, share_id, content)
                .await
                .context("failed to set share instructions")?
        }
        InstructionsCommand::ClearShare { share_id } => {
            api::instructions::delete_share_instructions(&client, share_id)
                .await
                .context("failed to clear share instructions")?
        }
        InstructionsCommand::GetShareUser { share_id } => {
            api::instructions::get_share_user_instructions(&client, share_id)
                .await
                .context("failed to get per-user share instructions")?
        }
        InstructionsCommand::SetShareUser { share_id, content } => {
            api::instructions::set_share_user_instructions(&client, share_id, content)
                .await
                .context("failed to set per-user share instructions")?
        }
        InstructionsCommand::ClearShareUser { share_id } => {
            api::instructions::delete_share_user_instructions(&client, share_id)
                .await
                .context("failed to clear per-user share instructions")?
        }
    };
    ctx.output.render(&value)?;
    Ok(())
}
