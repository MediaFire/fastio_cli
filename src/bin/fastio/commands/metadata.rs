/// Metadata extraction command implementations for `fastio metadata *`.
///
/// Handles listing eligible files, managing template-file mappings,
/// AI-based matching, and metadata extraction.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Metadata subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MetadataCommand {
    /// List files eligible for metadata extraction.
    Eligible {
        /// Workspace ID.
        workspace: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Add files to a metadata template.
    AddNodes {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// JSON-encoded array of node IDs.
        node_ids: String,
    },
    /// Remove files from a metadata template.
    RemoveNodes {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// JSON-encoded array of node IDs.
        node_ids: String,
    },
    /// List files mapped to a metadata template.
    ListNodes {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// AI-based file matching for a template.
    AutoMatch {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
    },
    /// Batch extract metadata for all files in a template.
    ExtractAll {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
    },
    /// Extract metadata from a single file.
    Extract {
        /// Workspace ID.
        workspace: String,
        /// Node ID of the file.
        node_id: String,
        /// Template ID to extract against.
        template_id: String,
    },
}

/// Execute a metadata subcommand.
pub async fn execute(command: &MetadataCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        MetadataCommand::Eligible {
            workspace,
            limit,
            offset,
        } => eligible(ctx, workspace, *limit, *offset).await,
        MetadataCommand::AddNodes {
            workspace,
            template_id,
            node_ids,
        } => add_nodes(ctx, workspace, template_id, node_ids).await,
        MetadataCommand::RemoveNodes {
            workspace,
            template_id,
            node_ids,
        } => remove_nodes(ctx, workspace, template_id, node_ids).await,
        MetadataCommand::ListNodes {
            workspace,
            template_id,
            limit,
            offset,
        } => list_nodes(ctx, workspace, template_id, *limit, *offset).await,
        MetadataCommand::AutoMatch {
            workspace,
            template_id,
        } => auto_match(ctx, workspace, template_id).await,
        MetadataCommand::ExtractAll {
            workspace,
            template_id,
        } => extract_all(ctx, workspace, template_id).await,
        MetadataCommand::Extract {
            workspace,
            node_id,
            template_id,
        } => extract(ctx, workspace, node_id, template_id).await,
    }
}

/// List files eligible for metadata extraction.
async fn eligible(
    ctx: &CommandContext<'_>,
    workspace: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::list_eligible(&client, workspace, limit, offset)
        .await
        .context("failed to list eligible files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Add files to a metadata template.
async fn add_nodes(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::add_nodes_to_template(&client, workspace, template_id, node_ids)
        .await
        .context("failed to add nodes to template")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Remove files from a metadata template.
async fn remove_nodes(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::remove_nodes_from_template(&client, workspace, template_id, node_ids)
            .await
            .context("failed to remove nodes from template")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List files mapped to a metadata template.
async fn list_nodes(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::list_template_nodes(&client, workspace, template_id, limit, offset)
        .await
        .context("failed to list template nodes")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// AI-based file matching for a template.
async fn auto_match(ctx: &CommandContext<'_>, workspace: &str, template_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::auto_match_template(&client, workspace, template_id)
        .await
        .context("failed to auto-match files to template")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Batch extract metadata for all files in a template.
async fn extract_all(ctx: &CommandContext<'_>, workspace: &str, template_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::extract_all(&client, workspace, template_id)
        .await
        .context("failed to batch extract metadata")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Extract metadata from a single file.
async fn extract(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    template_id: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::extract_node_metadata(&client, workspace, node_id, template_id)
        .await
        .context("failed to extract metadata")?;
    ctx.output.render(&value)?;
    Ok(())
}
