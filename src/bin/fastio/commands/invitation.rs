/// Invitation command implementations for `fastio invitation *`.
///
/// Handles listing, accepting, declining, and deleting invitations.
use anyhow::{Context, Result, bail};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Known valid entity types for invitations.
const VALID_ENTITY_TYPES: &[&str] = &["workspace", "share"];

/// Validate that an entity type is one of the known valid types.
fn validate_entity_type(entity_type: &str) -> Result<()> {
    if VALID_ENTITY_TYPES.contains(&entity_type) {
        Ok(())
    } else {
        bail!(
            "invalid entity type '{entity_type}'. Valid types: {}",
            VALID_ENTITY_TYPES.join(", ")
        )
    }
}

/// Invitation subcommand variants.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum InvitationCommand {
    /// List pending invitations for the current user.
    List {
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Accept an invitation (or all invitations).
    Accept {
        /// Invitation ID (not used for accept-all).
        invitation_id: Option<String>,
    },
    /// Decline an invitation.
    Decline {
        /// Invitation ID.
        invitation_id: String,
        /// Entity type (workspace or share).
        entity_type: String,
        /// Entity ID.
        entity_id: String,
    },
    /// Delete an invitation.
    Delete {
        /// Invitation ID.
        invitation_id: String,
        /// Entity type (workspace or share).
        entity_type: String,
        /// Entity ID.
        entity_id: String,
    },
}

/// Execute an invitation subcommand.
pub async fn execute(command: &InvitationCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        InvitationCommand::List { limit, offset } => list(ctx, *limit, *offset).await,
        InvitationCommand::Accept { invitation_id } => accept(ctx, invitation_id.as_deref()).await,
        InvitationCommand::Decline {
            invitation_id,
            entity_type,
            entity_id,
        } => decline(ctx, invitation_id, entity_type, entity_id).await,
        InvitationCommand::Delete {
            invitation_id,
            entity_type,
            entity_id,
        } => delete(ctx, invitation_id, entity_type, entity_id).await,
    }
}

/// List pending invitations for the current user.
async fn list(ctx: &CommandContext<'_>, limit: Option<u32>, offset: Option<u32>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::invitation::list_user_invitations(&client, limit, offset)
        .await
        .context("failed to list invitations")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Accept invitations (all pending).
async fn accept(ctx: &CommandContext<'_>, invitation_id: Option<&str>) -> Result<()> {
    if invitation_id.is_some() {
        eprintln!(
            "warning: individual invitation accept is not supported by the API; \
             accepting all pending invitations instead"
        );
    }

    let client = ctx.build_client()?;
    let value = api::invitation::accept_all_user_invitations(&client)
        .await
        .context("failed to accept invitations")?;

    let result = json!({
        "status": "accepted",
        "details": value,
    });
    ctx.output.render(&result)?;
    Ok(())
}

/// Decline an invitation by updating its state.
async fn decline(
    ctx: &CommandContext<'_>,
    invitation_id: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<()> {
    validate_entity_type(entity_type)?;

    let client = ctx.build_client()?;
    let value = api::invitation::update_invitation(
        &client,
        entity_type,
        entity_id,
        invitation_id,
        Some("declined"),
        None,
    )
    .await
    .context("failed to decline invitation")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete an invitation.
async fn delete(
    ctx: &CommandContext<'_>,
    invitation_id: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<()> {
    validate_entity_type(entity_type)?;

    let client = ctx.build_client()?;
    api::invitation::delete_invitation(&client, entity_type, entity_id, invitation_id)
        .await
        .context("failed to delete invitation")?;

    let value = json!({
        "status": "deleted",
        "invitation_id": invitation_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}
