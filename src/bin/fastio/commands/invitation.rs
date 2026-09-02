/// Invitation command implementations for `fastio invitation *`.
///
/// Handles listing, accepting, declining, and deleting invitations.
use anyhow::{Context, Result, bail};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Entity types valid on the `/user/invitations/`-backed state updates
/// (`accept` by id, `decline`, `delete`).
const VALID_ENTITY_TYPES: &[&str] = &["workspace", "share"];

/// Entity types valid on the KEY-based join route,
/// `POST /{entity_type}/{entity_id}/members/join/{key}/{action}/`.
///
/// Wider than the list above by one: **org**. The published API docs document
/// the same route family for an organization — "append the invitation key to
/// the URL path … optionally followed by `accept` or `decline`" — with a curl
/// example that spells it out.
///
/// Kept as a SEPARATE list rather than widening the one above, because the two
/// routes genuinely differ: `/user/invitations/` state updates are documented
/// for workspace and share only.
const JOIN_ENTITY_TYPES: &[&str] = &["workspace", "share", "org"];

/// Validate an entity type for the key-based join route.
fn validate_join_entity_type(entity_type: &str) -> Result<()> {
    if JOIN_ENTITY_TYPES.contains(&entity_type) {
        Ok(())
    } else {
        bail!(
            "invalid entity type '{entity_type}'. Valid types: {}",
            JOIN_ENTITY_TYPES.join(", ")
        )
    }
}

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
    /// Accept ONE invitation, or all of them when no id is given.
    Accept {
        /// Invitation ID. Omit to accept every pending invitation.
        invitation_id: Option<String>,
        /// Entity type (workspace or share); required with an id.
        entity_type: Option<String>,
        /// Entity ID; required with an id.
        entity_id: Option<String>,
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
    /// Accept or decline an invitation using the KEY from an invite email.
    Join {
        /// Entity type (workspace or share).
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Invitation key from the invite link.
        invitation_key: String,
        /// `accept` or `decline`.
        action: String,
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
        InvitationCommand::Accept {
            invitation_id,
            entity_type,
            entity_id,
        } => {
            accept(
                ctx,
                invitation_id.as_deref(),
                entity_type.as_deref(),
                entity_id.as_deref(),
            )
            .await
        }
        InvitationCommand::Decline {
            invitation_id,
            entity_type,
            entity_id,
        } => decline(ctx, invitation_id, entity_type, entity_id).await,
        InvitationCommand::Join {
            entity_type,
            entity_id,
            invitation_key,
            action,
        } => join(ctx, entity_type, entity_id, invitation_key, action).await,
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

/// Accept ONE invitation, or every pending invitation when no id is given.
///
/// Passing an id updates that ONE invitation and nothing else. It uses the same
/// mechanism as `decline` directly below, which sets `state` on one specific
/// invitation through `POST /{entity}/{id}/members/invitation/{invitation_id}/`;
/// `"accepted"` is a documented value of that same `state` parameter. Omitting
/// the id accepts every pending invitation instead.
async fn accept(
    ctx: &CommandContext<'_>,
    invitation_id: Option<&str>,
    entity_type: Option<&str>,
    entity_id: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;

    if let Some(id) = invitation_id {
        // clap enforces the pairing; this is the belt-and-braces for any
        // non-clap caller, and it must never silently widen to accept-all.
        let (Some(entity_type), Some(entity_id)) = (entity_type, entity_id) else {
            anyhow::bail!(
                "accepting a single invitation needs --entity-type and --entity-id \
                 (from `fastio invitation list`). Run `fastio invitation accept` with \
                 no arguments to accept ALL pending invitations."
            );
        };
        validate_entity_type(entity_type)?;

        let value = api::invitation::update_invitation(
            &client,
            entity_type,
            entity_id,
            id,
            &api::invitation::UpdateInvitationParams {
                new_state: Some("accepted"),
                ..Default::default()
            },
        )
        .await
        .context("failed to accept invitation")?;
        ctx.output.render(&value)?;
        return Ok(());
    }

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

/// Accept or decline a keyed invitation.
///
/// This is the **invitation-key** route an invite email links to
/// (`POST /{entity_type}/{entity_id}/members/join/{key}/{action}/`), and it is
/// the only way to act on a SINGLE invitation — the `/user/invitations/`
/// surface `accept` uses has no per-id accept at all.
async fn join(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    invitation_key: &str,
    action: &str,
) -> Result<()> {
    validate_join_entity_type(entity_type)?;

    let client = ctx.build_client()?;
    let value =
        api::member::join_invitation(&client, entity_type, entity_id, invitation_key, action)
            .await
            .with_context(|| format!("failed to {action} the {entity_type} invitation"))?;
    ctx.output.render(&value)?;
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
        &api::invitation::UpdateInvitationParams {
            new_state: Some("declined"),
            ..Default::default()
        },
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

#[cfg(test)]
mod tests {
    /// The two entity-type lists are deliberately DIFFERENT, and the
    /// difference is exactly `org`.
    ///
    /// The key-based join route is documented for organizations; the
    /// `/user/invitations/` state updates that `decline` / `delete` use are
    /// documented for workspace and share only. Widening the wrong list would
    /// add a route the platform does not serve.
    #[test]
    fn join_accepts_org_but_the_state_update_routes_do_not() {
        use super::{validate_entity_type, validate_join_entity_type};

        for t in ["workspace", "share"] {
            validate_entity_type(t).unwrap_or_else(|e| panic!("{t} on state update: {e}"));
            validate_join_entity_type(t).unwrap_or_else(|e| panic!("{t} on join: {e}"));
        }

        validate_join_entity_type("org").expect("org IS valid on the key-based join route");
        let err = validate_entity_type("org")
            .expect_err("org is NOT documented on the /user/invitations/ state updates");
        assert!(
            err.to_string().contains("workspace, share"),
            "the error must name what IS accepted there: {err}"
        );

        for bad in ["fileshare", "", "user"] {
            validate_join_entity_type(bad).expect_err("unknown entity types stay rejected");
        }
    }
}
