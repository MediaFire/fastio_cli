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
        /// Anchoring reference as a JSON object string (or `@file.json`).
        reference: Option<String>,
        /// Arbitrary metadata as a JSON object string (or `@file.json`).
        properties: Option<String>,
        /// Inline-attach a single object (object ID).
        target_id: Option<String>,
        /// Inline-attach multiple objects (object IDs, ≤25).
        target_ids: Vec<String>,
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
    /// Edit a comment's text (author-only; by comment ID).
    Edit {
        /// Comment ID.
        comment_id: String,
        /// New comment text.
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
    /// Bulk soft-delete up to 100 comments by ID.
    BulkDelete {
        /// Comment IDs to delete.
        comment_ids: Vec<String>,
    },
    /// Link a comment to a workflow entity (task or `workflow_review`).
    Link {
        /// Comment ID.
        comment_id: String,
        /// Workflow entity type (task or `workflow_review`).
        entity_type: String,
        /// Workflow entity ID.
        entity_id: String,
    },
    /// Remove a comment's link to its workflow entity.
    Unlink {
        /// Comment ID.
        comment_id: String,
    },
    /// List comments linked to a workflow entity (task or `workflow_review`).
    Linked {
        /// Workflow entity type (task or `workflow_review`).
        entity_type: String,
        /// Workflow entity ID.
        entity_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// List the objects attached to a comment.
    Attachments {
        /// Comment ID.
        comment_id: String,
    },
    /// Attach one or more objects to a comment.
    Attach {
        /// Comment ID.
        comment_id: String,
        /// Attach a single object (object ID).
        target_id: Option<String>,
        /// Attach multiple objects (object IDs, ≤25).
        target_ids: Vec<String>,
    },
    /// Detach a single object from a comment.
    Detach {
        /// Comment ID.
        comment_id: String,
        /// Object ID to detach.
        target_id: String,
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
#[allow(clippy::too_many_lines)]
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
            reference,
            properties,
            target_id,
            target_ids,
        } => {
            validate_entity_args(entity_type, entity_id, node_id)?;
            create(
                ctx,
                entity_type,
                entity_id,
                node_id,
                text,
                reference.as_deref(),
                properties.as_deref(),
                target_id.as_deref(),
                target_ids,
            )
            .await
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
        CommentCommand::Edit { comment_id, text } => edit(ctx, comment_id, text).await,
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
        CommentCommand::BulkDelete { comment_ids } => bulk_delete(ctx, comment_ids).await,
        CommentCommand::Link {
            comment_id,
            entity_type,
            entity_id,
        } => link(ctx, comment_id, entity_type, entity_id).await,
        CommentCommand::Unlink { comment_id } => unlink(ctx, comment_id).await,
        CommentCommand::Linked {
            entity_type,
            entity_id,
            limit,
            offset,
        } => linked(ctx, entity_type, entity_id, *limit, *offset).await,
        CommentCommand::Attachments { comment_id } => attachments(ctx, comment_id).await,
        CommentCommand::Attach {
            comment_id,
            target_id,
            target_ids,
        } => attach(ctx, comment_id, target_id.as_deref(), target_ids).await,
        CommentCommand::Detach {
            comment_id,
            target_id,
        } => detach(ctx, comment_id, target_id).await,
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
///
/// Supports the optional create extensions: an anchoring `reference`, arbitrary
/// `properties`, and inline attachment(s) (`target_id` single or `target_ids`
/// batch, ≤25). `--target-id` / `--target-ids` are mutually exclusive (enforced
/// by clap), so an empty `target_ids` slice means "single or none".
#[allow(clippy::too_many_arguments)]
async fn create(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    node_id: &str,
    text: &str,
    reference: Option<&str>,
    properties: Option<&str>,
    target_id: Option<&str>,
    target_ids: &[String],
) -> Result<()> {
    let reference = super::parse_json_object_arg(reference, "reference")?;
    let properties = super::parse_json_object_arg(properties, "properties")?;
    let target_ids = clean_target_ids(target_ids);
    anyhow::ensure!(
        target_ids.len() <= 25,
        "a comment accepts at most 25 attachments (got {})",
        target_ids.len()
    );
    let client = ctx.build_client()?;
    let value = api::comment::add_comment(
        &client,
        &api::comment::AddCommentParams {
            entity_type,
            entity_id,
            node_id,
            body: text,
            parent_id: None,
            reference: reference.as_ref(),
            properties: properties.as_ref(),
            target_id,
            target_ids: (!target_ids.is_empty()).then_some(target_ids.as_slice()),
        },
    )
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
        &api::comment::AddCommentParams {
            entity_type,
            entity_id,
            node_id,
            body: text,
            parent_id: Some(comment_id),
            reference: None,
            properties: None,
            target_id: None,
            target_ids: None,
        },
    )
    .await
    .context("failed to reply to comment")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Edit a comment's text.
async fn edit(ctx: &CommandContext<'_>, comment_id: &str, text: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::update_comment(&client, comment_id, text)
        .await
        .context("failed to edit comment")?;
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

/// Bulk soft-delete up to 100 comments.
async fn bulk_delete(ctx: &CommandContext<'_>, comment_ids: &[String]) -> Result<()> {
    let ids: Vec<String> = comment_ids
        .iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    anyhow::ensure!(!ids.is_empty(), "at least one comment ID is required");
    anyhow::ensure!(
        ids.len() <= 100,
        "bulk-delete accepts at most 100 comment ids (got {})",
        ids.len()
    );
    let client = ctx.build_client()?;
    let value = api::comment::bulk_delete_comments(&client, &ids)
        .await
        .context("failed to bulk-delete comments")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Link a comment to a workflow entity (task or `workflow_review`).
async fn link(
    ctx: &CommandContext<'_>,
    comment_id: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::link_comment(&client, comment_id, entity_type, entity_id)
        .await
        .context("failed to link comment")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Remove a comment's link to its workflow entity.
async fn unlink(ctx: &CommandContext<'_>, comment_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::unlink_comment(&client, comment_id)
        .await
        .context("failed to unlink comment")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List comments linked to a workflow entity (task or `workflow_review`).
async fn linked(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::linked_comments(&client, entity_type, entity_id, limit, offset)
        .await
        .context("failed to list linked comments")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List the objects attached to a comment.
async fn attachments(ctx: &CommandContext<'_>, comment_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::list_comment_attachments(&client, comment_id)
        .await
        .context("failed to list comment attachments")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Attach one or more objects to a comment.
///
/// `--target-id` / `--target-ids` are mutually exclusive (enforced by clap);
/// exactly one must be supplied (guarded here).
async fn attach(
    ctx: &CommandContext<'_>,
    comment_id: &str,
    target_id: Option<&str>,
    target_ids: &[String],
) -> Result<()> {
    let ids = clean_target_ids(target_ids);
    let targets = match (target_id, ids.is_empty()) {
        (Some(id), true) => api::comment::CommentAttachTargets::Single(id),
        (None, false) => api::comment::CommentAttachTargets::Multiple(ids.as_slice()),
        (Some(_), false) => {
            // Defensive: clap's `conflicts_with` already rejects this pairing.
            anyhow::bail!("--target-id and --target-ids are mutually exclusive");
        }
        (None, true) => {
            anyhow::bail!("one of --target-id or --target-ids is required");
        }
    };
    anyhow::ensure!(
        ids.len() <= 25,
        "a comment accepts at most 25 attachments (got {})",
        ids.len()
    );
    let client = ctx.build_client()?;
    let value = api::comment::attach_comment(&client, comment_id, &targets)
        .await
        .context("failed to attach to comment")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Detach a single object from a comment.
async fn detach(ctx: &CommandContext<'_>, comment_id: &str, target_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::comment::detach_comment(&client, comment_id, target_id)
        .await
        .context("failed to detach from comment")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Trim and drop blank entries from a `--target-ids` list.
fn clean_target_ids(target_ids: &[String]) -> Vec<String> {
    target_ids
        .iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::clean_target_ids;

    #[test]
    fn clean_target_ids_trims_and_drops_blanks() {
        let input = vec![
            " a ".to_owned(),
            String::new(),
            "b".to_owned(),
            "   ".to_owned(),
        ];
        assert_eq!(
            clean_target_ids(&input),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }
}
