/// Upload command implementations for `fastio upload *`.
///
/// Handles chunked file uploads with progress bars, text file upload,
/// and URL imports.
use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
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
    /// Upload one or more local files.
    ///
    /// When `file_paths` has exactly one entry and `preserve_tree` is `None`,
    /// the single-file pipeline is used. Otherwise the batch orchestrator
    /// runs: small files go through `/upload/batch/` (≤ 200 files,
    /// ≤ 100 MB, ≤ 4 MB/file) and oversize files fall back to the chunked
    /// single-file pipeline.
    File {
        /// Workspace ID.
        workspace: String,
        /// Paths to local files (positional). Empty when `preserve_tree` is set.
        file_paths: Vec<String>,
        /// Destination folder node ID (defaults to root).
        folder: Option<String>,
        /// Directory to walk, preserving sub-folder structure via
        /// per-file `relative_path`.
        preserve_tree: Option<String>,
        /// Exit 0 even if some files in a batch errored.
        allow_partial: bool,
        /// Optional echo-back correlation tag.
        creator: Option<String>,
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
            file_paths,
            folder,
            preserve_tree,
            allow_partial,
            creator,
        } => {
            dispatch_upload_file(
                ctx,
                workspace,
                file_paths,
                folder.as_deref().unwrap_or("root"),
                preserve_tree.as_deref(),
                *allow_partial,
                creator.as_deref(),
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

/// One resolved input to the batch orchestrator.
///
/// `relative_path` is optional and, when present, encodes the source tree
/// position under the batch-level `folder_id`.
#[derive(Debug, Clone)]
struct ResolvedInput {
    absolute_path: std::path::PathBuf,
    filename: String,
    relative_path: Option<String>,
    size: u64,
}

/// Route the user's `fastio upload file` invocation to either the single-file
/// pipeline or the batch orchestrator.
///
/// - Exactly one positional path, no `--preserve-tree`: single-file pipeline
///   (preserves the existing single-call / chunked behavior).
/// - Two or more paths, or `--preserve-tree DIR`: batch orchestrator.
async fn dispatch_upload_file(
    ctx: &CommandContext<'_>,
    workspace: &str,
    file_paths: &[String],
    folder: &str,
    preserve_tree: Option<&str>,
    allow_partial: bool,
    creator: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );

    if let Some(tree_root) = preserve_tree {
        let inputs = walk_tree_inputs(tree_root)?;
        anyhow::ensure!(
            !inputs.is_empty(),
            "no files found under --preserve-tree root: {tree_root}"
        );
        return run_batch_upload(ctx, workspace, folder, &inputs, allow_partial, creator).await;
    }

    match file_paths.len() {
        // Unreachable under normal CLI invocation — clap enforces
        // `required_unless_present = "preserve_tree"`. Kept as a defensive
        // guard for library consumers who call this directly.
        0 => anyhow::bail!("no files provided"),
        1 => upload_file(ctx, workspace, &file_paths[0], folder).await,
        _ => {
            let inputs = resolve_file_inputs(file_paths)?;
            run_batch_upload(ctx, workspace, folder, &inputs, allow_partial, creator).await
        }
    }
}

/// Resolve a list of bare file paths into [`ResolvedInput`] entries.
///
/// Follows symlinks for explicit user-supplied paths (same behavior as
/// `cat`, `cp`, `tar`). Symlinks are filtered out only during
/// [`walk_tree_inputs`] where they could create cycles or surface files
/// outside the user's intended root.
fn resolve_file_inputs(file_paths: &[String]) -> Result<Vec<ResolvedInput>> {
    let mut out = Vec::with_capacity(file_paths.len());
    for p in file_paths {
        let path = Path::new(p);
        let metadata = std::fs::metadata(path).with_context(|| format!("failed to stat: {p}"))?;
        if !metadata.is_file() {
            anyhow::bail!("not a regular file: {p} (use --preserve-tree for directories)");
        }
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid filename: {p}"))?
            .to_owned();
        fastio_cli::api::upload::validate_filename(&filename)
            .map_err(|e| anyhow::anyhow!("invalid filename {filename:?}: {e}"))?;
        out.push(ResolvedInput {
            absolute_path: path.to_path_buf(),
            filename,
            relative_path: None,
            size: metadata.len(),
        });
    }
    Ok(out)
}

/// Recursively walk `root`, returning [`ResolvedInput`] entries with
/// `relative_path` set so the server reconstructs the sub-folder tree under
/// the batch-level `folder_id`.
///
/// `DirEntry::file_type` does not follow symlinks, so symlinks are classified
/// as neither file nor directory and are silently skipped — preventing cycles
/// and preventing a symlink target outside `root` from being uploaded.
fn walk_tree_inputs(root: &str) -> Result<Vec<ResolvedInput>> {
    let root_path = Path::new(root);
    let root_meta =
        std::fs::metadata(root_path).with_context(|| format!("failed to stat: {root}"))?;
    if !root_meta.is_dir() {
        anyhow::bail!("not a directory: {root}");
    }

    let mut out = Vec::new();
    let mut stack = vec![root_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory: {}", dir.display()))?;
        for entry in entries {
            let entry = entry.context("failed to read directory entry")?;
            let file_type = entry
                .file_type()
                .context("failed to read dir entry file type")?;
            let entry_path = entry.path();
            if file_type.is_dir() {
                stack.push(entry_path);
                continue;
            }
            // Skip symlinks and other non-regular files (sockets, fifos,
            // devices) — we only upload ordinary files.
            if !file_type.is_file() {
                continue;
            }
            // DirEntry::metadata is lstat-equivalent on Unix and cheap on
            // Windows, avoiding the symlink-follow race that std::fs::metadata
            // would open here.
            let size = entry
                .metadata()
                .with_context(|| format!("failed to stat: {}", entry_path.display()))?
                .len();
            let rel = entry_path
                .strip_prefix(root_path)
                .with_context(|| format!("path outside root: {}", entry_path.display()))?;
            let filename = rel
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow::anyhow!("invalid filename: {}", entry_path.display()))?
                .to_owned();
            fastio_cli::api::upload::validate_filename(&filename)
                .map_err(|e| anyhow::anyhow!("invalid filename {:?}: {e}", entry_path.display()))?;
            let relative_path = rel.parent().and_then(|p| {
                if p.as_os_str().is_empty() {
                    None
                } else {
                    // Always forward slashes, always trailing slash. Normalizes
                    // Windows `\` to server-expected `/`.
                    let joined: Vec<String> = p
                        .components()
                        .filter_map(|c| match c {
                            std::path::Component::Normal(s) => s.to_str().map(ToOwned::to_owned),
                            _ => None,
                        })
                        .collect();
                    if joined.is_empty() {
                        None
                    } else {
                        let mut s = joined.join("/");
                        s.push('/');
                        Some(s)
                    }
                }
            });
            out.push(ResolvedInput {
                absolute_path: entry_path,
                filename,
                relative_path,
                size,
            });
        }
    }
    // Deterministic order — helps reproducibility and diff-friendly output.
    out.sort_by(|a, b| a.absolute_path.cmp(&b.absolute_path));
    Ok(out)
}

/// Aggregate outcome from a multi-batch orchestration run.
struct BatchRun {
    succeeded: Vec<BatchEntryOk>,
    errored: Vec<BatchEntryErr>,
}

struct BatchEntryOk {
    filename: String,
    relative_path: Option<String>,
    upload_id: Option<String>,
    /// `Some(id)` when finalize completed inline, `None` when storage is async.
    node_id: Option<String>,
    size: u64,
    via: &'static str,
}

struct BatchEntryErr {
    filename: String,
    relative_path: Option<String>,
    error_code: Option<u32>,
    error_message: String,
    via: &'static str,
}

/// Orchestrate a multi-file upload:
/// - Files > 4 MB fall back to the chunked single-file pipeline.
/// - Files ≤ 4 MB are packed into sequential batches capped by file count
///   and total body size.
/// - Rate-limit / retry handling is inherited from `api::upload::upload_batch`.
#[allow(clippy::too_many_lines)]
async fn run_batch_upload(
    ctx: &CommandContext<'_>,
    workspace: &str,
    folder: &str,
    inputs: &[ResolvedInput],
    allow_partial: bool,
    creator: Option<&str>,
) -> Result<()> {
    use fastio_cli::api::upload::{
        BATCH_MAX_BODY_BYTES, BATCH_MAX_FILE_BYTES, BATCH_MAX_FILES, BatchUploadItem,
        validate_creator_tag, validate_relative_path,
    };

    // Fail fast on a bad --creator tag: the server would reject the whole
    // batch (HTTP 4xx) after the request body is uploaded.
    if let Some(tag) = creator {
        validate_creator_tag(tag).map_err(|e| anyhow::anyhow!("invalid --creator tag: {e}"))?;
    }

    // Validate relative_paths up-front so the user sees input errors without
    // a half-run batch.
    for inp in inputs {
        if let Some(rp) = &inp.relative_path {
            validate_relative_path(rp)
                .map_err(|e| anyhow::anyhow!("invalid relative_path for {}: {e}", inp.filename))?;
        }
    }

    let token_str = resolve_auth(ctx.profile_name, ctx.flag_token, ctx.config_dir)?;
    let client = ApiClient::new(ctx.api_base, Some(token_str.clone()))
        .context("failed to create API client")?;

    let total_count = inputs.len();
    let total_bytes: u64 = inputs.iter().map(|i| i.size).sum();

    if !ctx.output.quiet {
        eprintln!(
            "Uploading {total_count} file(s), {} total",
            format_bytes(total_bytes),
        );
    }

    // Split into large (> 4 MB, chunked pipeline) and small (batchable).
    let (large, small): (Vec<_>, Vec<_>) = inputs
        .iter()
        .cloned()
        .partition(|i| i.size > BATCH_MAX_FILE_BYTES);

    let mut run = BatchRun {
        succeeded: Vec::new(),
        errored: Vec::new(),
    };

    // Route oversize files through the existing chunked pipeline, one at a
    // time. Failures here don't block batches — they land in the errored
    // bucket alongside per-file batch errors.
    for inp in large {
        match upload_large_via_chunked(ctx, &client, &token_str, workspace, folder, &inp).await {
            Ok((upload_id, node_id)) => run.succeeded.push(BatchEntryOk {
                filename: inp.filename.clone(),
                relative_path: inp.relative_path.clone(),
                upload_id: Some(upload_id),
                node_id,
                size: inp.size,
                via: "chunked",
            }),
            Err(e) => {
                // Preserve the numeric API error code through the anyhow chain
                // so the aggregated summary's errored[].error_code is
                // meaningful to scripting callers, not silently `null`.
                let error_code = e.chain().find_map(|cause| {
                    cause
                        .downcast_ref::<fastio_cli::error::CliError>()
                        .and_then(|ce| match ce {
                            fastio_cli::error::CliError::Api(api) => Some(api.code),
                            _ => None,
                        })
                });
                run.errored.push(BatchEntryErr {
                    filename: inp.filename.clone(),
                    relative_path: inp.relative_path.clone(),
                    error_code,
                    error_message: format!("{e:#}"),
                    via: "chunked",
                });
            }
        }
    }

    // Pack small files into batches respecting the 200-file count limit and
    // the 100 MB total-body cap (minus 10% multipart-overhead headroom so the
    // server's post-parse check passes).
    let body_budget = BATCH_MAX_BODY_BYTES - BATCH_MAX_BODY_BYTES / 10;
    let sizes: Vec<u64> = small.iter().map(|i| i.size).collect();
    let groups = pack_batches(&sizes, BATCH_MAX_FILES, body_budget);
    let small_vec: Vec<ResolvedInput> = small;
    let mut batches: Vec<Vec<ResolvedInput>> = Vec::with_capacity(groups.len());
    for group in groups {
        let mut b = Vec::with_capacity(group.len());
        for idx in group {
            b.push(small_vec[idx].clone());
        }
        batches.push(b);
    }

    let total_batches = batches.len();
    for (batch_idx, batch) in batches.into_iter().enumerate() {
        // Build batch items. Defend against TOCTOU: the file's recorded size
        // came from a stat during tree-walk / resolution, but the file can
        // grow between stat and read. Failures here stay local to the file —
        // one unreadable file or one oversize file doesn't sink the whole
        // orchestration.
        let mut items: Vec<BatchUploadItem> = Vec::with_capacity(batch.len());
        let mut kept_inputs: Vec<ResolvedInput> = Vec::with_capacity(batch.len());
        let mut running_bytes: u64 = 0;
        for inp in &batch {
            let raw = match std::fs::read(&inp.absolute_path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    run.errored.push(BatchEntryErr {
                        filename: inp.filename.clone(),
                        relative_path: inp.relative_path.clone(),
                        error_code: None,
                        error_message: format!(
                            "failed to read {}: {e}",
                            inp.absolute_path.display(),
                        ),
                        via: "skipped",
                    });
                    continue;
                }
            };
            if raw.len() as u64 > BATCH_MAX_FILE_BYTES {
                run.errored.push(BatchEntryErr {
                    filename: inp.filename.clone(),
                    relative_path: inp.relative_path.clone(),
                    error_code: None,
                    error_message: format!(
                        "file grew past {} between scan and upload (now {} bytes)",
                        format_bytes(BATCH_MAX_FILE_BYTES),
                        raw.len(),
                    ),
                    via: "skipped",
                });
                continue;
            }
            // The batch was packed against walk-time sizes. If any file grew
            // between scan and read, the real batch body can overshoot the
            // 100 MB server cap. Re-check against the same headroom-adjusted
            // budget the packer used.
            let new_total = running_bytes.saturating_add(raw.len() as u64);
            if new_total > body_budget {
                run.errored.push(BatchEntryErr {
                    filename: inp.filename.clone(),
                    relative_path: inp.relative_path.clone(),
                    error_code: None,
                    error_message: format!(
                        "batch body grew past {} between scan and upload",
                        format_bytes(body_budget),
                    ),
                    via: "skipped",
                });
                continue;
            }
            running_bytes = new_total;
            let hash = fastio_cli::api::upload::sha256_hex(&raw);
            items.push(BatchUploadItem {
                filename: inp.filename.clone(),
                relative_path: inp.relative_path.clone(),
                data: bytes::Bytes::from(raw),
                hash: Some(hash),
                hash_algo: Some("sha256".to_owned()),
            });
            kept_inputs.push(inp.clone());
        }

        if items.is_empty() {
            // Every file in this batch was filtered out (unreadable, grew
            // past the per-file cap, or blew the body budget).
            continue;
        }

        // Emit the per-batch progress line AFTER the filter so the user sees
        // the real submission size, not the stale walk-time sum. If any
        // filter fired above, it already printed its own error line.
        if !ctx.output.quiet {
            eprintln!(
                "Batch {}/{total_batches}: {} files, {}",
                batch_idx + 1,
                items.len(),
                format_bytes(running_bytes),
            );
        }

        let folder_opt = if folder.is_empty() {
            None
        } else {
            Some(folder)
        };
        let resp = fastio_cli::api::upload::upload_batch(
            &token_str,
            ctx.api_base,
            workspace,
            folder_opt,
            creator,
            &items,
        )
        .await
        .with_context(|| format!("batch {} failed", batch_idx + 1))?;

        // Pair server results back to submitted inputs by manifest index, not
        // by zip — guards against a response with reordered, missing, duplicated,
        // or extra entries. Any submitted input without a matching result is
        // reported as an error so the total count stays honest.
        if resp.results.len() != items.len() && !ctx.output.quiet {
            eprintln!(
                "{} Batch {}/{total_batches}: server returned {} results for {} submitted files",
                "warning:".yellow().bold(),
                batch_idx + 1,
                resp.results.len(),
                items.len(),
            );
        }
        let mut matched = vec![false; kept_inputs.len()];
        for entry in &resp.results {
            let idx = entry.index as usize;
            let Some(inp) = kept_inputs.get(idx) else {
                // Out-of-range index from the server. Record as an orphaned
                // error instead of a panic.
                run.errored.push(BatchEntryErr {
                    filename: entry.filename.clone(),
                    relative_path: None,
                    error_code: entry.error_code,
                    error_message: format!(
                        "server returned out-of-range result index {idx} for batch of {} files",
                        items.len(),
                    ),
                    via: "batch",
                });
                continue;
            };
            if matched[idx] {
                // Duplicate index from the server. Without this guard the same
                // file would be booked into both succeeded[] and errored[],
                // inflating count_total and tripping --allow-partial logic.
                run.errored.push(BatchEntryErr {
                    filename: entry.filename.clone(),
                    relative_path: inp.relative_path.clone(),
                    error_code: entry.error_code,
                    error_message: format!(
                        "server returned duplicate result for manifest index {idx}",
                    ),
                    via: "batch",
                });
                continue;
            }
            matched[idx] = true;
            if entry.status == "ok" {
                run.succeeded.push(BatchEntryOk {
                    filename: entry.filename.clone(),
                    relative_path: inp.relative_path.clone(),
                    upload_id: entry.upload_id.clone(),
                    // Flatten Option<Option<String>>: outer Some means the
                    // key was present; inner None means async-storage success.
                    node_id: entry.node_id.clone().and_then(|v| v),
                    size: inp.size,
                    via: "batch",
                });
            } else {
                run.errored.push(BatchEntryErr {
                    filename: entry.filename.clone(),
                    relative_path: inp.relative_path.clone(),
                    error_code: entry.error_code,
                    error_message: entry
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "unknown per-file error".to_owned()),
                    via: "batch",
                });
            }
        }
        // Any input we submitted but the server didn't acknowledge must be
        // surfaced — the caller otherwise sees "all good" while a file was
        // silently dropped.
        for (i, inp) in kept_inputs.iter().enumerate() {
            if !matched[i] {
                run.errored.push(BatchEntryErr {
                    filename: inp.filename.clone(),
                    relative_path: inp.relative_path.clone(),
                    error_code: None,
                    error_message:
                        "server did not return a result for this file (batch response truncated?)"
                            .to_owned(),
                    via: "batch",
                });
            }
        }
    }

    // Render aggregated result to the user-selected output format.
    let succeeded: Vec<serde_json::Value> = run
        .succeeded
        .iter()
        .map(|s| {
            json!({
                "filename": s.filename,
                "relative_path": s.relative_path,
                "size": s.size,
                "upload_id": s.upload_id,
                // node_id is nullable on success — async-storage workspaces
                // assign it later; poll `/storage/{folder}/list/` or subscribe
                // to upload events if callers need the final id.
                "node_id": s.node_id,
                "via": s.via,
            })
        })
        .collect();
    let errored: Vec<serde_json::Value> = run
        .errored
        .iter()
        .map(|e| {
            json!({
                "filename": e.filename,
                "relative_path": e.relative_path,
                "error_code": e.error_code,
                "error_message": e.error_message,
                "via": e.via,
            })
        })
        .collect();

    let summary = json!({
        "count_total": total_count,
        "count_succeeded": run.succeeded.len(),
        "count_errored": run.errored.len(),
        "succeeded": succeeded,
        "errored": errored,
    });
    ctx.output.render(&summary)?;

    if !run.errored.is_empty() && !allow_partial {
        anyhow::bail!(
            "{} of {} file(s) failed (pass --allow-partial to exit 0 on partial success)",
            run.errored.len(),
            total_count,
        );
    }
    Ok(())
}

/// Upload a single > 4 MB file via the existing chunked pipeline, returning
/// `(upload_id, node_id)` on success. `node_id` may be `None` if the API
/// finalized asynchronously and did not report it.
async fn upload_large_via_chunked(
    ctx: &CommandContext<'_>,
    client: &ApiClient,
    token_str: &str,
    workspace: &str,
    folder: &str,
    inp: &ResolvedInput,
) -> Result<(String, Option<String>)> {
    if !ctx.output.quiet {
        eprintln!(
            "Large file (> 4 MB) → chunked: {} ({})",
            inp.filename,
            format_bytes(inp.size),
        );
    }

    let session = fastio_cli::api::upload::create_upload_session(
        client,
        workspace,
        folder,
        &inp.filename,
        inp.size,
    )
    .await
    .context("failed to create upload session")?;
    let upload_id = session
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("upload session did not return an ID"))?
        .to_owned();

    let pb = create_progress_bar(inp.size, ctx.output.quiet, ctx.output.no_color);
    let mut file_handle = std::fs::File::open(&inp.absolute_path)
        .with_context(|| format!("failed to open {}", inp.absolute_path.display()))?;
    send_chunks(
        &mut file_handle,
        inp.size,
        token_str,
        ctx.api_base,
        &upload_id,
        &pb,
    )
    .await?;
    fastio_cli::api::upload::complete_upload(client, &upload_id)
        .await
        .context("failed to complete upload")?;
    poll_upload_completion(client, &upload_id, 60).await?;

    let details = fastio_cli::api::upload::get_upload_status(client, &upload_id)
        .await
        .ok();
    let node_id = details
        .as_ref()
        .and_then(|d| d.get("session"))
        .and_then(|s| s.get("new_file_id"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    Ok((upload_id, node_id))
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

/// Pack file sizes into batch groups honoring both the file-count cap and a
/// total-bytes cap. Returns groups of original indices.
///
/// Greedy first-fit: each file is appended to the current batch; when adding
/// it would exceed either cap, the batch is closed and a fresh one starts.
/// Tested directly (see `tests::pack_batches_*`).
fn pack_batches(sizes: &[u64], max_files: usize, max_bytes: u64) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_bytes: u64 = 0;
    for (i, &sz) in sizes.iter().enumerate() {
        let would_overflow = current_bytes.saturating_add(sz) > max_bytes;
        if !current.is_empty() && (current.len() >= max_files || would_overflow) {
            groups.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(sz);
        current.push(i);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
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

#[cfg(test)]
mod tests {
    use super::pack_batches;

    #[test]
    fn pack_batches_count_boundary() {
        // 201 files of size 1 — should split into 200 + 1 at the count cap.
        let sizes = vec![1u64; 201];
        let groups = pack_batches(&sizes, 200, 100 * 1024 * 1024);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 200);
        assert_eq!(groups[1].len(), 1);
    }

    #[test]
    fn pack_batches_bytes_boundary() {
        // 30 files × 4 MB = 120 MB total; body cap 100 MB means at most 25
        // files (100 MB) fit in one batch. Next file starts a new batch.
        let four_mb = 4u64 * 1024 * 1024;
        let sizes = vec![four_mb; 30];
        let groups = pack_batches(&sizes, 200, 100 * 1024 * 1024);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 25);
        assert_eq!(groups[1].len(), 5);
    }

    #[test]
    fn pack_batches_empty_input_yields_no_groups() {
        let groups = pack_batches(&[], 200, 100 * 1024 * 1024);
        assert!(groups.is_empty());
    }

    #[test]
    fn pack_batches_single_small_file_one_group() {
        let groups = pack_batches(&[42], 200, 100 * 1024 * 1024);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![0]);
    }

    #[test]
    fn pack_batches_exact_count_fits_one_group() {
        // Exactly 200 1-byte files should fit in a single group.
        let sizes = vec![1u64; 200];
        let groups = pack_batches(&sizes, 200, 100 * 1024 * 1024);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 200);
    }
}
