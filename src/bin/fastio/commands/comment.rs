/// Comment command implementations for `fastio comment *`.
///
/// Handles listing, creating, replying to, and deleting comments
/// on workspace and share files.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Comment subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CommentCommand {
    /// List comments on a file.
    List {
        /// Entity type: workspace or share.
        entity_type: String,
        /// Entity ID (workspace or share ID).
        entity_id: String,
        /// Storage node ID.
        node_id: String,
        /// Sort order.
        sort: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Add a comment to a file.
    Create {
        /// Entity type: workspace or share.
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Storage node ID.
        node_id: String,
        /// Comment text.
        text: String,
    },
    /// Reply to an existing comment.
    Reply {
        /// Entity type: workspace or share.
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Storage node ID.
        node_id: String,
        /// Parent comment ID to reply to.
        comment_id: String,
        /// Reply text.
        text: String,
    },
    /// Delete a comment.
    Delete {
        /// Comment ID.
        comment_id: String,
    },
    /// List all comments across an entity (workspace or share).
    ListAll {
        /// Entity type: workspace or share.
        entity_type: String,
        /// Entity ID (workspace or share ID).
        entity_id: String,
        /// Sort order.
        sort: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
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
        /// Emoji to react with.
        emoji: String,
    },
    /// Remove your emoji reaction from a comment.
    Unreact {
        /// Comment ID.
        comment_id: String,
    },
}

/// Valid entity types for comments.
const VALID_ENTITY_TYPES: &[&str] = &["workspace", "share"];

/// Validate entity type is one of the accepted values.
fn validate_entity_type(entity_type: &str) -> Result<()> {
    anyhow::ensure!(
        VALID_ENTITY_TYPES.contains(&entity_type),
        "invalid entity type '{entity_type}'. Must be one of: workspace, share"
    );
    Ok(())
}

/// Validate entity type and IDs common to several comment subcommands.
fn validate_entity_args(entity_type: &str, entity_id: &str, node_id: &str) -> Result<()> {
    validate_entity_type(entity_type)?;
    anyhow::ensure!(!entity_id.trim().is_empty(), "entity ID must not be empty");
    anyhow::ensure!(!node_id.trim().is_empty(), "node ID must not be empty");
    Ok(())
}

/// Execute a comment subcommand.
pub async fn execute(command: &CommentCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        CommentCommand::List {
            entity_type,
            entity_id,
            node_id,
            sort,
            limit,
            offset,
        } => {
            validate_entity_args(entity_type, entity_id, node_id)?;
            list(
                ctx,
                entity_type,
                entity_id,
                node_id,
                sort.as_deref(),
                *limit,
                *offset,
            )
            .await
        }
        CommentCommand::Create {
            entity_type,
            entity_id,
            node_id,
            text,
        } => {
            validate_entity_args(entity_type, entity_id, node_id)?;
            create(ctx, entity_type, entity_id, node_id, text).await
        }
        CommentCommand::Reply {
            entity_type,
            entity_id,
            node_id,
            comment_id,
            text,
        } => {
            validate_entity_args(entity_type, entity_id, node_id)?;
            reply(ctx, entity_type, entity_id, node_id, comment_id, text).await
        }
        CommentCommand::Delete { comment_id } => delete(ctx, comment_id).await,
        CommentCommand::ListAll {
            entity_type,
            entity_id,
            sort,
            limit,
            offset,
        } => {
            validate_entity_type(entity_type)?;
            anyhow::ensure!(!entity_id.trim().is_empty(), "entity ID must not be empty");
            list_all(
                ctx,
                entity_type,
                entity_id,
                sort.as_deref(),
                *limit,
                *offset,
            )
            .await
        }
        CommentCommand::Info { comment_id } => info(ctx, comment_id).await,
        CommentCommand::React { comment_id, emoji } => react(ctx, comment_id, emoji).await,
        CommentCommand::Unreact { comment_id } => unreact(ctx, comment_id).await,
    }
}

/// List comments on a file.
async fn list(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    node_id: &str,
    sort: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::list_comments(
        &client,
        &api::comment::ListCommentsParams {
            entity_type,
            entity_id,
            node_id,
            sort,
            limit,
            offset,
        },
    )
    .await
    .context("failed to list comments")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Add a comment.
async fn create(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    node_id: &str,
    text: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::add_comment(&client, entity_type, entity_id, node_id, text, None)
        .await
        .context("failed to create comment")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Reply to an existing comment.
async fn reply(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    node_id: &str,
    comment_id: &str,
    text: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::add_comment(
        &client,
        entity_type,
        entity_id,
        node_id,
        text,
        Some(comment_id),
    )
    .await
    .context("failed to reply to comment")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a comment.
async fn delete(ctx: &CommandContext<'_>, comment_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::comment::delete_comment(&client, comment_id)
        .await
        .context("failed to delete comment")?;
    let value = json!({
        "status": "deleted",
        "comment_id": comment_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// List all comments across an entity.
async fn list_all(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    sort: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::comment::list_all_comments(&client, entity_type, entity_id, sort, limit, offset)
            .await
            .context("failed to list all comments")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get comment details.
async fn info(ctx: &CommandContext<'_>, comment_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::get_comment_details(&client, comment_id)
        .await
        .context("failed to get comment details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Add an emoji reaction to a comment.
async fn react(ctx: &CommandContext<'_>, comment_id: &str, emoji: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::add_reaction(&client, comment_id, emoji)
        .await
        .context("failed to add reaction")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Remove your emoji reaction from a comment.
async fn unreact(ctx: &CommandContext<'_>, comment_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::remove_reaction(&client, comment_id)
        .await
        .context("failed to remove reaction")?;
    ctx.output.render(&value)?;
    Ok(())
}
