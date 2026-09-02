/// Asset command implementations for `fastio asset *`.
///
/// Handles uploading and deleting brand assets (logos, banners, etc.)
/// for organizations, workspaces, shares, and users.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Asset subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AssetCommand {
    /// Upload an asset.
    Upload {
        /// Entity type: org, workspace, share.
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Asset type name (e.g. logo, banner).
        asset_type: String,
        /// Path to the file to upload.
        file: String,
    },
    /// Remove an asset.
    Remove {
        /// Entity type: org, workspace, share.
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Asset type name.
        asset_type: String,
    },
    /// List assets on an entity.
    List {
        /// Entity type: org, workspace, share.
        entity_type: String,
        /// Entity ID.
        entity_id: String,
    },
    /// List available asset types.
    Types {
        /// Entity type: org, workspace, share.
        entity_type: String,
    },
}

/// Execute an asset subcommand.
pub async fn execute(command: &AssetCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        AssetCommand::Upload {
            entity_type,
            entity_id,
            asset_type,
            file,
        } => upload(ctx, entity_type, entity_id, asset_type, file).await,
        AssetCommand::Remove {
            entity_type,
            entity_id,
            asset_type,
        } => remove(ctx, entity_type, entity_id, asset_type).await,
        AssetCommand::List {
            entity_type,
            entity_id,
        } => list_assets(ctx, entity_type, entity_id).await,
        AssetCommand::Types { entity_type } => list_types(ctx, entity_type).await,
    }
}

/// Upload an asset (`POST /{entity_type}/{entity_id}/assets/{asset_type}/`).
///
/// Delegates to `api::user::upload_entity_asset`, which implements the multipart
/// contract generically. The route family is uniform across user / org /
/// workspace / share, so one implementation serves all four — the same reason
/// `delete_asset` next door is generic.
async fn upload(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    asset_type: &str,
    file: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::user::upload_entity_asset(&client, entity_type, entity_id, asset_type, file)
        .await
        .with_context(|| {
            format!("failed to upload the {asset_type} asset to {entity_type} {entity_id}")
        })?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Remove an asset.
async fn remove(
    ctx: &CommandContext<'_>,
    entity_type: &str,
    entity_id: &str,
    asset_type: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    api::asset::delete_asset(&client, entity_type, entity_id, asset_type)
        .await
        .context("failed to remove asset")?;
    let value = json!({
        "status": "removed",
        "entity_type": entity_type,
        "entity_id": entity_id,
        "asset_type": asset_type,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// List assets on an entity.
async fn list_assets(ctx: &CommandContext<'_>, entity_type: &str, entity_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::asset::list_assets(&client, entity_type, entity_id)
        .await
        .context("failed to list assets")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List available asset types.
async fn list_types(ctx: &CommandContext<'_>, entity_type: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::asset::list_asset_types(&client, entity_type)
        .await
        .context("failed to list asset types")?;
    ctx.output.render(&value)?;
    Ok(())
}
