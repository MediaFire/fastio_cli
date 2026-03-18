/// Download command implementations for `fastio download *`.
///
/// Handles downloading individual files, folders as ZIP, and batch downloads
/// with progress bars.
use std::path::PathBuf;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::auth::token;
use fastio_cli::client::ApiClient;
use fastio_cli::output::OutputConfig;

/// Download subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DownloadCommand {
    /// Download a single file.
    File {
        /// Workspace ID.
        workspace: String,
        /// Node ID of the file to download.
        node_id: String,
        /// Output file path (auto-determined if omitted).
        output_path: Option<String>,
    },
    /// Download a folder as a ZIP archive.
    Folder {
        /// Workspace ID.
        workspace: String,
        /// Node ID of the folder to download.
        node_id: String,
        /// Output file path (auto-determined if omitted).
        output_path: Option<String>,
    },
    /// Download multiple files.
    Batch {
        /// Workspace ID.
        workspace: String,
        /// Node IDs to download.
        node_ids: Vec<String>,
        /// Output directory (defaults to current directory).
        output_dir: Option<String>,
    },
}

/// Execute a download subcommand.
pub async fn execute(command: &DownloadCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        DownloadCommand::File {
            workspace,
            node_id,
            output_path,
        } => download_file(ctx, workspace, node_id, output_path.as_deref()).await,
        DownloadCommand::Folder {
            workspace,
            node_id,
            output_path,
        } => download_folder(ctx, workspace, node_id, output_path.as_deref()).await,
        DownloadCommand::Batch {
            workspace,
            node_ids,
            output_dir,
        } => download_batch(ctx, workspace, node_ids, output_dir.as_deref()).await,
    }
}

/// Resolve authentication token.
fn resolve_auth(
    profile_name: &str,
    flag_token: Option<&str>,
    config_dir: &std::path::Path,
) -> Result<String> {
    let resolved = token::resolve_token(flag_token, profile_name, config_dir)
        .context("failed to resolve token")?;
    resolved.ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))
}

/// Format bytes as a human-readable string.
#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Create a progress bar for downloads.
///
/// Returns a hidden progress bar when `output.quiet` is set, and uses
/// plain (uncolored) templates when `output.no_color` is set.
fn create_progress_bar(total: Option<u64>, output: &OutputConfig) -> ProgressBar {
    if output.quiet {
        return ProgressBar::hidden();
    }

    let pb = if let Some(total) = total {
        ProgressBar::new(total)
    } else {
        ProgressBar::new_spinner()
    };

    let style = if total.is_some() {
        let template = if output.no_color {
            "{spinner} [{bar:40}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})"
        } else {
            "{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})"
        };
        ProgressStyle::with_template(template)
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("#>-")
    } else {
        let template = if output.no_color {
            "{spinner} {bytes} ({bytes_per_sec})"
        } else {
            "{spinner:.green} {bytes} ({bytes_per_sec})"
        };
        ProgressStyle::with_template(template).unwrap_or_else(|_| ProgressStyle::default_spinner())
    };

    pb.set_style(style);
    pb
}

/// Determine output filename from node details or use a default.
async fn determine_output_path(
    client: &ApiClient,
    workspace: &str,
    node_id: &str,
    user_path: Option<&str>,
    default_ext: &str,
) -> Result<PathBuf> {
    if let Some(p) = user_path {
        return Ok(PathBuf::from(p));
    }

    // Get file details to determine name
    let details = api::download::get_node_details_for_download(client, workspace, node_id)
        .await
        .ok();

    let filename = details
        .as_ref()
        .and_then(api::download::extract_filename)
        .unwrap_or_else(|| format!("{node_id}{default_ext}"));

    Ok(PathBuf::from(filename))
}

/// Download a single file.
async fn download_file(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    user_output: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    anyhow::ensure!(!node_id.trim().is_empty(), "node ID must not be empty");
    let client = ctx.build_client()?;

    // Determine output path
    let output_path = determine_output_path(&client, workspace, node_id, user_output, "").await?;

    if !ctx.output.quiet {
        eprintln!("Downloading to: {}", output_path.display());
    }

    // Get download token
    let token_resp = api::download::get_download_url(&client, workspace, node_id)
        .await
        .context("failed to get download URL")?;

    let download_token = api::download::extract_download_token(&token_resp)
        .ok_or_else(|| anyhow::anyhow!("server returned empty download token"))?;

    let download_url =
        api::download::build_download_url(ctx.api_base, workspace, node_id, &download_token);

    // Stream download with progress
    let pb = create_progress_bar(None, ctx.output);

    let total_bytes = api::download::download_file(
        &download_url,
        &output_path,
        None, // Token is in the URL
        |downloaded, total| {
            if let Some(t) = total {
                pb.set_length(t);
            }
            pb.set_position(downloaded);
        },
    )
    .await
    .context("download failed")?;

    pb.finish_with_message("Download complete");

    let value = json!({
        "status": "downloaded",
        "node_id": node_id,
        "output": output_path.display().to_string(),
        "size": total_bytes,
        "size_human": format_bytes(total_bytes),
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Download a folder as a ZIP archive.
async fn download_folder(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    user_output: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    anyhow::ensure!(!node_id.trim().is_empty(), "node ID must not be empty");
    let token_str = resolve_auth(ctx.profile_name, ctx.flag_token, ctx.config_dir)?;
    let client = ApiClient::new(ctx.api_base, Some(token_str.clone()))
        .context("failed to create API client")?;

    // Determine output path
    let output_path =
        determine_output_path(&client, workspace, node_id, user_output, ".zip").await?;

    if !ctx.output.quiet {
        eprintln!("Downloading folder as ZIP to: {}", output_path.display());
    }

    let zip_url = api::download::get_zip_url(ctx.api_base, workspace, node_id);

    // Stream download with progress (ZIP requires auth header)
    let pb = create_progress_bar(None, ctx.output);

    let total_bytes = api::download::download_file(
        &zip_url,
        &output_path,
        Some(&token_str),
        |downloaded, total| {
            if let Some(t) = total {
                pb.set_length(t);
            }
            pb.set_position(downloaded);
        },
    )
    .await
    .context("folder download failed")?;

    pb.finish_with_message("Download complete");

    let value = json!({
        "status": "downloaded",
        "node_id": node_id,
        "output": output_path.display().to_string(),
        "size": total_bytes,
        "size_human": format_bytes(total_bytes),
        "format": "zip",
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Download multiple files.
async fn download_batch(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_ids: &[String],
    output_dir: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;

    let dir = output_dir.map_or_else(|| PathBuf::from("."), PathBuf::from);
    if !dir.exists() {
        std::fs::create_dir_all(&dir).context("failed to create output directory")?;
    }

    let mut results = Vec::new();

    for node_id in node_ids {
        if !ctx.output.quiet {
            eprintln!("Downloading {node_id}...");
        }

        // Get filename
        let details = api::download::get_node_details_for_download(&client, workspace, node_id)
            .await
            .ok();
        let filename = details
            .as_ref()
            .and_then(api::download::extract_filename)
            .unwrap_or_else(|| node_id.clone());

        let output_path = dir.join(&filename);

        // Get download token
        let token_resp = match api::download::get_download_url(&client, workspace, node_id).await {
            Ok(resp) => resp,
            Err(e) => {
                if !ctx.output.quiet {
                    eprintln!("  Failed to get download URL for {node_id}: {e}");
                }
                results.push(json!({
                    "node_id": node_id,
                    "status": "failed",
                    "error": e.to_string(),
                }));
                continue;
            }
        };

        let Some(download_token) = api::download::extract_download_token(&token_resp) else {
            if !ctx.output.quiet {
                eprintln!("  Empty download token for {node_id}");
            }
            results.push(json!({
                "node_id": node_id,
                "status": "failed",
                "error": "empty download token",
            }));
            continue;
        };

        let download_url =
            api::download::build_download_url(ctx.api_base, workspace, node_id, &download_token);

        let pb = create_progress_bar(None, ctx.output);

        match api::download::download_file(
            &download_url,
            &output_path,
            None,
            |downloaded, total| {
                if let Some(t) = total {
                    pb.set_length(t);
                }
                pb.set_position(downloaded);
            },
        )
        .await
        {
            Ok(total_bytes) => {
                pb.finish_with_message("done");
                results.push(json!({
                    "node_id": node_id,
                    "status": "downloaded",
                    "output": output_path.display().to_string(),
                    "size": total_bytes,
                    "size_human": format_bytes(total_bytes),
                }));
            }
            Err(e) => {
                pb.finish_with_message("failed");
                if !ctx.output.quiet {
                    eprintln!("  Download failed for {node_id}: {e}");
                }
                results.push(json!({
                    "node_id": node_id,
                    "status": "failed",
                    "error": e.to_string(),
                }));
            }
        }
    }

    let value = json!({
        "downloads": results,
    });
    ctx.output.render(&value)?;
    Ok(())
}
