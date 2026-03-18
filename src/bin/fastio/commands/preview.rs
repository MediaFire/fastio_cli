/// Preview command implementations for `fastio preview *`.
///
/// Handles preview URL generation and file transformation requests.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Preview subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PreviewCommand {
    /// Get a preauthorized preview URL.
    Get {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID (workspace or share ID).
        context_id: String,
        /// Storage node ID.
        node_id: String,
        /// Preview type: binary, thumbnail, image, pdf, hlsstream, audio, spreadsheet.
        preview_type: String,
    },
    /// Get a thumbnail preview URL.
    Thumbnail {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
        /// Storage node ID.
        node_id: String,
    },
    /// Request a file transformation URL.
    Transform {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
        /// Storage node ID.
        node_id: String,
        /// Transform name (e.g. "image").
        transform_name: String,
        /// Target width in pixels.
        width: Option<u32>,
        /// Target height in pixels.
        height: Option<u32>,
        /// Output format: png, jpg, webp.
        output_format: Option<String>,
        /// Size preset.
        size: Option<String>,
    },
}

/// Execute a preview subcommand.
pub async fn execute(command: &PreviewCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        PreviewCommand::Get {
            context_type,
            context_id,
            node_id,
            preview_type,
        } => get_preview(ctx, context_type, context_id, node_id, preview_type).await,
        PreviewCommand::Thumbnail {
            context_type,
            context_id,
            node_id,
        } => get_preview(ctx, context_type, context_id, node_id, "thumbnail").await,
        PreviewCommand::Transform {
            context_type,
            context_id,
            node_id,
            transform_name,
            width,
            height,
            output_format,
            size,
        } => {
            let client = ctx.build_client()?;
            let value = api::preview::get_transform_url(
                &client,
                &api::preview::TransformUrlParams {
                    context_type,
                    context_id,
                    node_id,
                    transform_name,
                    width: *width,
                    height: *height,
                    output_format: output_format.as_deref(),
                    size: size.as_deref(),
                },
            )
            .await
            .context("failed to get transform URL")?;
            ctx.output.render(&value)?;
            Ok(())
        }
    }
}

/// Get a preview URL.
async fn get_preview(
    ctx: &CommandContext<'_>,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    preview_type: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::preview::get_preview_url(&client, context_type, context_id, node_id, preview_type)
            .await
            .context("failed to get preview URL")?;
    ctx.output.render(&value)?;
    Ok(())
}
