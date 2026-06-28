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
        /// Preview type: bin, thumbnail, image, hlsstream, pdf, spreadsheet, audio, mp4.
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
    /// Request an image transformation URL.
    ///
    /// Returns `{transform_name, token, read_url}` (the two-step model): fetch
    /// `read_url` to get the transformed bytes.
    Transform {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
        /// Storage node ID.
        node_id: String,
        /// Transform name (must be "image").
        transform_name: String,
        /// Target width in pixels.
        width: Option<u32>,
        /// Target height in pixels.
        height: Option<u32>,
        /// Output format: png, jpg, or jpeg.
        output_format: Option<String>,
        /// Size preset.
        size: Option<String>,
        /// Crop rectangle width.
        crop_width: Option<u32>,
        /// Crop rectangle height.
        crop_height: Option<u32>,
        /// Crop rectangle x offset.
        crop_x: Option<u32>,
        /// Crop rectangle y offset.
        crop_y: Option<u32>,
        /// Rotation in degrees (0, 90, 180, 270).
        rotate: Option<u32>,
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
            crop_width,
            crop_height,
            crop_x,
            crop_y,
            rotate,
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
                    crop_width: *crop_width,
                    crop_height: *crop_height,
                    crop_x: *crop_x,
                    crop_y: *crop_y,
                    rotate: *rotate,
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
