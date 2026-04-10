/// Upload command implementations for `fastio upload *`.
///
/// Handles chunked file uploads with progress bars, text file upload,
/// and URL imports.
use std::path::Path;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::auth::token;
use fastio_cli::client::ApiClient;

/// Default chunk size for uploads: 4 MB.
const DEFAULT_CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Upload subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum UploadCommand {
    /// Upload a local file with progress bar and chunking.
    File {
        /// Workspace ID.
        workspace: String,
        /// Path to the local file.
        file_path: String,
        /// Destination folder node ID (defaults to root).
        folder: Option<String>,
    },
    /// Upload text content as a file.
    Text {
        /// Workspace ID.
        workspace: String,
        /// Filename for the uploaded file.
        name: String,
        /// Text content to upload.
        content: String,
        /// Destination folder node ID (defaults to root).
        folder: Option<String>,
    },
    /// Import a file from a URL.
    Url {
        /// Workspace ID.
        workspace: String,
        /// Source URL.
        url: String,
        /// Destination folder node ID (defaults to root).
        folder: Option<String>,
        /// Override filename (derived from URL if omitted).
        name: Option<String>,
    },
    /// Create an upload session manually.
    CreateSession {
        /// Workspace ID.
        workspace: String,
        /// Filename.
        filename: String,
        /// File size in bytes.
        filesize: u64,
        /// Destination folder node ID (defaults to root).
        folder: Option<String>,
    },
    /// Upload a single chunk.
    Chunk {
        /// Upload key/ID.
        upload_key: String,
        /// Chunk number.
        chunk_num: u32,
        /// Path to chunk data file.
        file: String,
    },
    /// Trigger assembly.
    Finalize {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Check upload status.
    Status {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Cancel an upload.
    Cancel {
        /// Upload key/ID.
        upload_key: String,
    },
    /// List active upload sessions.
    ListSessions,
    /// Cancel all uploads.
    CancelAll,
    /// Check chunk status.
    ChunkStatus {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Delete a chunk.
    ChunkDelete {
        /// Upload key/ID.
        upload_key: String,
        /// Chunk number.
        chunk_num: u32,
    },
    /// List web imports.
    WebList,
    /// Cancel a web import.
    WebCancel {
        /// Upload ID.
        upload_id: String,
    },
    /// Check web import status.
    WebStatus {
        /// Upload ID.
        upload_id: String,
    },
    /// Get upload limits.
    Limits,
    /// Get restricted extensions.
    Extensions,
    /// Upload a local file via streaming (no exact size required upfront).
    Stream {
        /// Workspace ID.
        workspace: String,
        /// Path to the local file (use `-` for stdin).
        file_path: String,
        /// Destination folder node ID (defaults to root).
        folder: Option<String>,
        /// Maximum upload size in bytes (defaults to plan limit).
        max_size: Option<u64>,
        /// Override filename (required for stdin, derived from path otherwise).
        name: Option<String>,
        /// Pre-computed hash of the file content for integrity verification.
        hash: Option<String>,
        /// Hash algorithm used (e.g. sha256).
        hash_algo: Option<String>,
    },
    /// Create a streaming upload session manually.
    CreateStreamSession {
        /// Workspace ID.
        workspace: String,
        /// Filename.
        filename: String,
        /// Destination folder node ID (defaults to root).
        folder: Option<String>,
        /// Maximum upload size in bytes (defaults to plan limit).
        max_size: Option<u64>,
    },
    /// Send data to a streaming upload session (auto-finalizes).
    StreamSend {
        /// Upload key/ID from create-stream-session.
        upload_key: String,
        /// Path to data file.
        file: String,
        /// Maximum file size in bytes (rejects before reading if exceeded).
        max_size: Option<u64>,
        /// Pre-computed hash of the file content.
        hash: Option<String>,
        /// Hash algorithm used (e.g. sha256).
        hash_algo: Option<String>,
    },
}

/// Execute an upload subcommand.
#[allow(clippy::too_many_lines)]
pub async fn execute(command: &UploadCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        UploadCommand::File {
            workspace,
            file_path,
            folder,
        } => {
            upload_file(
                ctx,
                workspace,
                file_path,
                folder.as_deref().unwrap_or("root"),
            )
            .await
        }
        UploadCommand::Text {
            workspace,
            name,
            content,
            folder,
        } => {
            upload_text(
                ctx,
                workspace,
                name,
                content,
                folder.as_deref().unwrap_or("root"),
            )
            .await
        }
        UploadCommand::Url {
            workspace,
            url,
            folder,
            name,
        } => {
            upload_url(
                ctx,
                workspace,
                url,
                folder.as_deref().unwrap_or("root"),
                name.as_deref(),
            )
            .await
        }
        UploadCommand::CreateSession {
            workspace,
            filename,
            filesize,
            folder,
        } => create_session(ctx, workspace, filename, *filesize, folder.as_deref()).await,
        UploadCommand::Chunk {
            upload_key,
            chunk_num,
            file,
        } => upload_chunk(ctx, upload_key, *chunk_num, file).await,
        UploadCommand::Finalize { upload_key } => finalize(ctx, upload_key).await,
        UploadCommand::Status { upload_key } => status(ctx, upload_key).await,
        UploadCommand::Cancel { upload_key } => cancel(ctx, upload_key).await,
        UploadCommand::ListSessions => list_sessions(ctx).await,
        UploadCommand::CancelAll => cancel_all(ctx).await,
        UploadCommand::ChunkStatus { upload_key } => chunk_status(ctx, upload_key).await,
        UploadCommand::ChunkDelete {
            upload_key,
            chunk_num,
        } => chunk_delete(ctx, upload_key, *chunk_num).await,
        UploadCommand::WebList => web_list(ctx).await,
        UploadCommand::WebCancel { upload_id } => web_cancel(ctx, upload_id).await,
        UploadCommand::WebStatus { upload_id } => web_status(ctx, upload_id).await,
        UploadCommand::Limits => upload_limits(ctx).await,
        UploadCommand::Extensions => upload_extensions(ctx).await,
        UploadCommand::Stream {
            workspace,
            file_path,
            folder,
            max_size,
            name,
            hash,
            hash_algo,
        } => {
            stream_file(
                ctx,
                workspace,
                file_path,
                folder.as_deref().unwrap_or("root"),
                *max_size,
                name.as_deref(),
                hash.as_deref(),
                hash_algo.as_deref(),
            )
            .await
        }
        UploadCommand::CreateStreamSession {
            workspace,
            filename,
            folder,
            max_size,
        } => create_stream_session(ctx, workspace, filename, folder.as_deref(), *max_size).await,
        UploadCommand::StreamSend {
            upload_key,
            file,
            max_size,
            hash,
            hash_algo,
        } => {
            stream_send(
                ctx,
                upload_key,
                file,
                *max_size,
                hash.as_deref(),
                hash_algo.as_deref(),
            )
            .await
        }
    }
}

/// Create an upload session.
async fn create_session(
    ctx: &CommandContext<'_>,
    workspace: &str,
    filename: &str,
    filesize: u64,
    folder: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;
    let value = api::upload::create_upload_session(
        &client,
        workspace,
        folder.unwrap_or("root"),
        filename,
        filesize,
    )
    .await
    .context("failed to create upload session")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Upload a single chunk.
async fn upload_chunk(
    ctx: &CommandContext<'_>,
    upload_key: &str,
    chunk_num: u32,
    file: &str,
) -> Result<()> {
    let token_str = resolve_auth(ctx.profile_name, ctx.flag_token, ctx.config_dir)?;
    let data = std::fs::read(file).context("failed to read chunk file")?;
    let value = api::upload::upload_chunk(&token_str, ctx.api_base, upload_key, chunk_num, data)
        .await
        .context("failed to upload chunk")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Finalize an upload.
async fn finalize(ctx: &CommandContext<'_>, upload_key: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::complete_upload(&client, upload_key)
        .await
        .context("failed to finalize upload")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Check upload status.
async fn status(ctx: &CommandContext<'_>, upload_key: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::get_upload_status(&client, upload_key)
        .await
        .context("failed to get upload status")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Cancel an upload.
async fn cancel(ctx: &CommandContext<'_>, upload_key: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::upload::cancel_upload(&client, upload_key)
        .await
        .context("failed to cancel upload")?;
    let value = json!({ "status": "cancelled", "upload_key": upload_key });
    ctx.output.render(&value)?;
    Ok(())
}

/// List active upload sessions.
async fn list_sessions(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::list_sessions(&client)
        .await
        .context("failed to list upload sessions")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Cancel all uploads.
async fn cancel_all(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    api::upload::cancel_all(&client)
        .await
        .context("failed to cancel all uploads")?;
    let value = json!({ "status": "all_cancelled" });
    ctx.output.render(&value)?;
    Ok(())
}

/// Check chunk status.
async fn chunk_status(ctx: &CommandContext<'_>, upload_key: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::chunk_status(&client, upload_key)
        .await
        .context("failed to get chunk status")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a chunk.
async fn chunk_delete(ctx: &CommandContext<'_>, upload_key: &str, chunk_num: u32) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::chunk_delete(&client, upload_key, chunk_num)
        .await
        .context("failed to delete chunk")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List web imports.
async fn web_list(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::web_list(&client)
        .await
        .context("failed to list web imports")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Cancel a web import.
async fn web_cancel(ctx: &CommandContext<'_>, upload_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::upload::web_cancel(&client, upload_id)
        .await
        .context("failed to cancel web import")?;
    let value = json!({ "status": "cancelled", "upload_id": upload_id });
    ctx.output.render(&value)?;
    Ok(())
}

/// Check web import status.
async fn web_status(ctx: &CommandContext<'_>, upload_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::web_import_status(&client, upload_id)
        .await
        .context("failed to get web import status")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get upload limits.
async fn upload_limits(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::upload_limits(&client)
        .await
        .context("failed to get upload limits")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get restricted extensions.
async fn upload_extensions(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::upload::upload_extensions(&client)
        .await
        .context("failed to get upload extensions")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Build an authenticated client and resolve the token string.
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

/// Create a progress bar for file uploads.
fn create_progress_bar(file_size: u64, quiet: bool, no_color: bool) -> ProgressBar {
    let pb = if quiet {
        ProgressBar::hidden()
    } else {
        ProgressBar::new(file_size)
    };
    let template = if no_color {
        "{spinner} [{bar:40}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})"
    } else {
        "{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})"
    };
    pb.set_style(
        ProgressStyle::with_template(template)
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("#>-"),
    );
    pb
}

/// Upload file chunks from a file handle, reporting progress.
async fn send_chunks(
    file_handle: &mut std::fs::File,
    file_size: u64,
    token_str: &str,
    api_base: &str,
    upload_id: &str,
    pb: &ProgressBar,
) -> Result<()> {
    use std::io::Read;

    let total_chunks = file_size.div_ceil(DEFAULT_CHUNK_SIZE as u64);
    let mut chunk_number: u32 = 0;
    let mut bytes_uploaded: u64 = 0;

    loop {
        let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];
        let mut total_read = 0;
        loop {
            let n = file_handle
                .read(&mut buf[total_read..])
                .context("failed to read file chunk")?;
            if n == 0 {
                break;
            }
            total_read += n;
            if total_read >= DEFAULT_CHUNK_SIZE {
                break;
            }
        }
        if total_read == 0 {
            break;
        }
        buf.truncate(total_read);

        chunk_number = chunk_number
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("too many chunks"))?;

        api::upload::upload_chunk(token_str, api_base, upload_id, chunk_number, buf)
            .await
            .with_context(|| format!("failed to upload chunk {chunk_number}/{total_chunks}"))?;

        bytes_uploaded = bytes_uploaded.saturating_add(total_read as u64);
        pb.set_position(std::cmp::min(bytes_uploaded, file_size));
    }

    pb.finish_with_message("Upload complete, assembling...");
    Ok(())
}

/// Poll upload status until completion or failure.
async fn poll_upload_completion(
    client: &ApiClient,
    upload_id: &str,
    max_attempts: u32,
) -> Result<String> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let status_resp = api::upload::get_upload_status(client, upload_id)
            .await
            .context("failed to get upload status")?;

        let status_str = status_resp
            .get("session")
            .and_then(|s| s.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match status_str {
            "stored" | "complete" => return Ok(status_str.to_owned()),
            "assembly_failed" | "store_failed" => {
                let msg = status_resp
                    .get("session")
                    .and_then(|s| s.get("status_message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                anyhow::bail!("upload failed ({status_str}): {msg}");
            }
            _ => {
                if attempts >= max_attempts {
                    anyhow::bail!(
                        "upload timed out after {max_attempts} attempts (status: {status_str})"
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

/// Upload a local file with chunking and progress bar.
///
/// Files ≤ 4 MB use a single-call upload (one request, immediate result).
/// Larger files use the multi-step chunked flow.
#[allow(clippy::too_many_lines)]
async fn upload_file(
    ctx: &CommandContext<'_>,
    workspace: &str,
    file_path: &str,
    folder: &str,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let path = Path::new(file_path);
    if !path.exists() {
        anyhow::bail!("file not found: {file_path}");
    }

    let metadata = std::fs::metadata(path).context("failed to read file metadata")?;
    let file_size = metadata.len();
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid filename"))?;

    let token_str = resolve_auth(ctx.profile_name, ctx.flag_token, ctx.config_dir)?;

    if !ctx.output.quiet {
        eprintln!("Uploading {} ({})", filename, format_bytes(file_size));
    }

    // Use single-call upload for small files (≤ 4 MB)
    if file_size <= api::upload::SINGLE_CALL_MAX_SIZE {
        let file_data = std::fs::read(path).context("failed to read file")?;
        let client = ApiClient::new(ctx.api_base, Some(token_str.clone()))
            .context("failed to create API client")?;

        let resp = api::upload::single_call_upload(
            &token_str,
            ctx.api_base,
            workspace,
            folder,
            filename,
            file_data,
        )
        .await
        .context("single-call upload failed")?;

        let upload_id = resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        // The API may return new_file_id immediately or after brief processing.
        // Poll if not immediately available.
        let mut new_file_id = resp
            .get("new_file_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        if new_file_id.is_none() && !upload_id.is_empty() {
            let final_status = poll_upload_completion(&client, &upload_id, 30).await?;
            if final_status == "stored" || final_status == "complete" {
                let details = api::upload::get_upload_status(&client, &upload_id)
                    .await
                    .ok();
                new_file_id = details
                    .as_ref()
                    .and_then(|d| d.get("session"))
                    .and_then(|s| s.get("new_file_id"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
        }

        let value = json!({
            "status": "uploaded",
            "filename": filename,
            "size": file_size,
            "size_human": format_bytes(file_size),
            "upload_id": if upload_id.is_empty() { None } else { Some(&upload_id) },
            "new_file_id": new_file_id.as_deref().unwrap_or("unknown"),
            "final_status": "stored",
        });
        ctx.output.render(&value)?;
        return Ok(());
    }

    // Multi-step chunked upload for larger files
    let client = ApiClient::new(ctx.api_base, Some(token_str.clone()))
        .context("failed to create API client")?;

    let session =
        api::upload::create_upload_session(&client, workspace, folder, filename, file_size)
            .await
            .context("failed to create upload session")?;

    let upload_id = session
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("upload session did not return an ID"))?
        .to_owned();

    let pb = create_progress_bar(file_size, ctx.output.quiet, ctx.output.no_color);
    let mut file_handle = std::fs::File::open(path).context("failed to open file for reading")?;
    send_chunks(
        &mut file_handle,
        file_size,
        &token_str,
        ctx.api_base,
        &upload_id,
        &pb,
    )
    .await?;

    api::upload::complete_upload(&client, &upload_id)
        .await
        .context("failed to complete upload")?;

    let final_status = poll_upload_completion(&client, &upload_id, 60).await?;

    let final_details = api::upload::get_upload_status(&client, &upload_id)
        .await
        .ok();
    let new_file_id = final_details
        .as_ref()
        .and_then(|d| d.get("session"))
        .and_then(|s| s.get("new_file_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if final_status != "stored" {
        let _ = api::upload::cancel_upload(&client, &upload_id).await;
    }

    let value = json!({
        "status": "uploaded",
        "filename": filename,
        "size": file_size,
        "size_human": format_bytes(file_size),
        "upload_id": upload_id,
        "new_file_id": new_file_id,
        "final_status": final_status,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Upload text content as a file.
///
/// Text content ≤ 4 MB uses a single-call upload. Larger content falls back
/// to the multi-step chunked flow.
async fn upload_text(
    ctx: &CommandContext<'_>,
    workspace: &str,
    name: &str,
    content: &str,
    folder: &str,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let token_str = resolve_auth(ctx.profile_name, ctx.flag_token, ctx.config_dir)?;

    let content_bytes = content.as_bytes();
    let file_size = u64::try_from(content_bytes.len()).context("content size exceeds u64 range")?;

    if !ctx.output.quiet {
        eprintln!("Uploading text file {} ({})", name, format_bytes(file_size));
    }

    // Use single-call upload for small content (≤ 4 MB)
    if file_size <= api::upload::SINGLE_CALL_MAX_SIZE {
        let client = ApiClient::new(ctx.api_base, Some(token_str.clone()))
            .context("failed to create API client")?;

        let resp = api::upload::single_call_upload(
            &token_str,
            ctx.api_base,
            workspace,
            folder,
            name,
            content_bytes.to_vec(),
        )
        .await
        .context("single-call upload failed")?;

        let upload_id = resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        let mut new_file_id = resp
            .get("new_file_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        if new_file_id.is_none() && !upload_id.is_empty() {
            let final_status = poll_upload_completion(&client, &upload_id, 30).await?;
            if final_status == "stored" || final_status == "complete" {
                let details = api::upload::get_upload_status(&client, &upload_id)
                    .await
                    .ok();
                new_file_id = details
                    .as_ref()
                    .and_then(|d| d.get("session"))
                    .and_then(|s| s.get("new_file_id"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
        }

        let value = json!({
            "status": "uploaded",
            "filename": name,
            "size": file_size,
            "size_human": format_bytes(file_size),
            "new_file_id": new_file_id.as_deref().unwrap_or("unknown"),
        });
        ctx.output.render(&value)?;
        return Ok(());
    }

    // Multi-step chunked upload for larger content
    let client = ApiClient::new(ctx.api_base, Some(token_str.clone()))
        .context("failed to create API client")?;

    let session = api::upload::create_upload_session(&client, workspace, folder, name, file_size)
        .await
        .context("failed to create upload session")?;

    let upload_id = session
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("upload session did not return an ID"))?
        .to_owned();

    // Upload as single chunk
    api::upload::upload_chunk(
        &token_str,
        ctx.api_base,
        &upload_id,
        1,
        content_bytes.to_vec(),
    )
    .await
    .context("failed to upload content")?;

    api::upload::complete_upload(&client, &upload_id)
        .await
        .context("failed to complete upload")?;

    let final_status = poll_upload_completion(&client, &upload_id, 30).await?;

    if final_status != "stored" {
        let _ = api::upload::cancel_upload(&client, &upload_id).await;
    }

    let value = json!({
        "status": "uploaded",
        "filename": name,
        "size": file_size,
        "size_human": format_bytes(file_size),
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Upload a file via streaming mode.
///
/// Creates a stream session (no exact size required), reads the file into
/// memory, and POSTs the raw body to the `/stream/` endpoint which
/// auto-finalizes on completion.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn stream_file(
    ctx: &CommandContext<'_>,
    workspace: &str,
    file_path: &str,
    folder: &str,
    max_size: Option<u64>,
    name_override: Option<&str>,
    hash: Option<&str>,
    hash_algo: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );

    let (data, filename) = if file_path == "-" {
        use std::io::Read;
        let name = name_override
            .ok_or_else(|| anyhow::anyhow!("--name is required when reading from stdin"))?;
        let mut buf = Vec::new();
        if let Some(limit) = max_size {
            // Read at most `limit + 1` bytes so we can detect overflow
            // without consuming unbounded memory.
            let mut handle = std::io::stdin().take(limit.saturating_add(1));
            handle
                .read_to_end(&mut buf)
                .context("failed to read from stdin")?;
            anyhow::ensure!(
                u64::try_from(buf.len()).unwrap_or(u64::MAX) <= limit,
                "stdin data exceeds --max-size limit ({})",
                format_bytes(limit),
            );
        } else {
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("failed to read from stdin")?;
        }
        (buf, name.to_owned())
    } else {
        let path = Path::new(file_path);
        if !path.exists() {
            anyhow::bail!("file not found: {file_path}");
        }
        // Check file size against max_size before reading into memory.
        let metadata = std::fs::metadata(path).context("failed to read file metadata")?;
        let file_size = metadata.len();
        if let Some(limit) = max_size {
            anyhow::ensure!(
                file_size <= limit,
                "file size ({}) exceeds --max-size limit ({})",
                format_bytes(file_size),
                format_bytes(limit),
            );
        }
        let data = std::fs::read(path).context("failed to read file")?;
        let derived = name_override.map_or_else(
            || {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
                    .ok_or_else(|| anyhow::anyhow!("invalid filename"))
            },
            |n| Ok(n.to_owned()),
        )?;
        (data, derived)
    };

    let data_len = u64::try_from(data.len()).context("data size exceeds u64 range")?;
    if let Some(limit) = max_size {
        anyhow::ensure!(
            data_len <= limit,
            "data size ({}) exceeds --max-size limit ({})",
            format_bytes(data_len),
            format_bytes(limit),
        );
    }
    let data_bytes = bytes::Bytes::from(data);
    let token_str = resolve_auth(ctx.profile_name, ctx.flag_token, ctx.config_dir)?;
    let client = ApiClient::new(ctx.api_base, Some(token_str.clone()))
        .context("failed to create API client")?;

    if !ctx.output.quiet {
        eprintln!("Streaming upload {} ({})", filename, format_bytes(data_len));
    }

    let session =
        api::upload::create_stream_session(&client, workspace, folder, &filename, max_size)
            .await
            .context("failed to create stream session")?;

    let upload_id = session
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("stream session did not return an ID"))?
        .to_owned();

    // Use a spinner instead of a progress bar — the stream is sent in a
    // single request so there is no incremental progress to report.
    let spinner = if ctx.output.quiet {
        ProgressBar::hidden()
    } else {
        let sp = ProgressBar::new_spinner();
        let template = if ctx.output.no_color {
            "{spinner} Uploading {msg}"
        } else {
            "{spinner:.green} Uploading {msg}"
        };
        sp.set_style(
            ProgressStyle::with_template(template)
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        sp.set_message(format!("{} ({})", filename, format_bytes(data_len)));
        sp.enable_steady_tick(std::time::Duration::from_millis(120));
        sp
    };

    let result = api::upload::stream_upload(
        &token_str,
        ctx.api_base,
        &upload_id,
        data_bytes,
        hash,
        hash_algo,
    )
    .await
    .context("failed to stream upload data");

    match result {
        Ok(_) => {
            spinner.finish_with_message(format!(
                "{} ({}) — complete",
                filename,
                format_bytes(data_len)
            ));
        }
        Err(e) => {
            spinner.finish_with_message("failed");
            let _ = api::upload::cancel_upload(&client, &upload_id).await;
            return Err(e);
        }
    }

    let final_details = api::upload::get_upload_status(&client, &upload_id)
        .await
        .ok();
    let final_status = final_details
        .as_ref()
        .and_then(|d| d.get("session"))
        .and_then(|s| s.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let new_file_id = final_details
        .as_ref()
        .and_then(|d| d.get("session"))
        .and_then(|s| s.get("new_file_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let value = json!({
        "status": "uploaded",
        "mode": "stream",
        "filename": filename,
        "size": data_len,
        "size_human": format_bytes(data_len),
        "upload_id": upload_id,
        "new_file_id": new_file_id,
        "final_status": final_status,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a streaming upload session manually.
async fn create_stream_session(
    ctx: &CommandContext<'_>,
    workspace: &str,
    filename: &str,
    folder: Option<&str>,
    max_size: Option<u64>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;
    let value = api::upload::create_stream_session(
        &client,
        workspace,
        folder.unwrap_or("root"),
        filename,
        max_size,
    )
    .await
    .context("failed to create stream session")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Send data to a streaming upload session (auto-finalizes).
async fn stream_send(
    ctx: &CommandContext<'_>,
    upload_key: &str,
    file: &str,
    max_size: Option<u64>,
    hash: Option<&str>,
    hash_algo: Option<&str>,
) -> Result<()> {
    let token_str = resolve_auth(ctx.profile_name, ctx.flag_token, ctx.config_dir)?;
    // Check file size before reading into memory.
    let metadata = std::fs::metadata(file).context("failed to read file metadata")?;
    if let Some(limit) = max_size {
        anyhow::ensure!(
            metadata.len() <= limit,
            "file size ({}) exceeds --max-size limit ({})",
            format_bytes(metadata.len()),
            format_bytes(limit),
        );
    }
    let data = bytes::Bytes::from(std::fs::read(file).context("failed to read data file")?);
    let value =
        api::upload::stream_upload(&token_str, ctx.api_base, upload_key, data, hash, hash_algo)
            .await
            .context("failed to stream upload data")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Import a file from a URL.
async fn upload_url(
    ctx: &CommandContext<'_>,
    workspace: &str,
    url: &str,
    folder: &str,
    name: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    let client = ctx.build_client()?;

    if !ctx.output.quiet {
        eprintln!("Importing from URL: {url}");
    }

    let result = api::upload::web_import(&client, workspace, folder, url, name)
        .await
        .context("failed to start web import")?;

    ctx.output.render(&result)?;
    Ok(())
}
