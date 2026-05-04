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
        /// Optional template field name to sort by.
        sort_field: Option<String>,
        /// Sort direction (`asc` or `desc`).
        sort_dir: Option<String>,
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
    /// Enqueue an async metadata extraction for a single file.
    Extract {
        /// Workspace ID.
        workspace: String,
        /// Node ID of the file.
        node_id: String,
        /// Template ID to extract against.
        template_id: String,
        /// JSON-encoded array of field names for partial extraction.
        fields: Option<String>,
    },
    /// Preview files that match a proposed template name + description.
    PreviewMatch {
        /// Workspace ID.
        workspace: String,
        /// Proposed template name (1-255 chars).
        name: String,
        /// Natural-language template description.
        description: String,
    },
    /// Request AI-suggested column definitions for a proposed template.
    SuggestFields {
        /// Workspace ID.
        workspace: String,
        /// JSON-encoded array of 1-25 sample node IDs.
        node_ids: String,
        /// Template description.
        description: String,
        /// Optional short user hint (max 64 chars, letters/numbers/spaces).
        user_context: Option<String>,
    },
    /// Create a metadata template (a.k.a. view).
    CreateTemplate {
        /// Workspace ID.
        workspace: String,
        /// Template name.
        name: String,
        /// Template description.
        description: String,
        /// Template category.
        category: String,
        /// JSON-encoded fields array (suggest-fields output is compatible).
        fields: String,
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
            sort_field,
            sort_dir,
        } => {
            list_nodes(
                ctx,
                workspace,
                template_id,
                *limit,
                *offset,
                sort_field.as_deref(),
                sort_dir.as_deref(),
            )
            .await
        }
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
            fields,
        } => extract(ctx, workspace, node_id, template_id, fields.as_deref()).await,
        MetadataCommand::PreviewMatch {
            workspace,
            name,
            description,
        } => preview_match(ctx, workspace, name, description).await,
        MetadataCommand::SuggestFields {
            workspace,
            node_ids,
            description,
            user_context,
        } => {
            suggest_fields(
                ctx,
                workspace,
                node_ids,
                description,
                user_context.as_deref(),
            )
            .await
        }
        MetadataCommand::CreateTemplate {
            workspace,
            name,
            description,
            category,
            fields,
        } => create_template(ctx, workspace, name, description, category, fields).await,
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
    sort_field: Option<&str>,
    sort_dir: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::list_template_nodes(
        &client,
        workspace,
        template_id,
        limit,
        offset,
        sort_field,
        sort_dir,
    )
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

/// Enqueue an async metadata extraction for a single file.
async fn extract(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    template_id: &str,
    fields: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::extract_node_metadata(&client, workspace, node_id, template_id, fields)
            .await
            .context("failed to enqueue metadata extraction")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Preview files that match a proposed template name + description.
async fn preview_match(
    ctx: &CommandContext<'_>,
    workspace: &str,
    name: &str,
    description: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::preview_match(&client, workspace, name, description)
        .await
        .context("failed to preview matching files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Request AI-suggested column definitions for a proposed template.
async fn suggest_fields(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_ids: &str,
    description: &str,
    user_context: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::suggest_fields(&client, workspace, node_ids, description, user_context)
            .await
            .context("failed to request suggested fields")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a metadata template (a.k.a. view) with the given column definitions.
async fn create_template(
    ctx: &CommandContext<'_>,
    workspace: &str,
    name: &str,
    description: &str,
    category: &str,
    fields: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::create_template(&client, workspace, name, description, category, fields)
            .await
            .context("failed to create metadata template")?;
    ctx.output.render(&value)?;
    Ok(())
}
