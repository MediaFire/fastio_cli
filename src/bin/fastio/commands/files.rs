/// File and folder command implementations for `fastio files *`.
///
/// Handles listing, details, folder creation, move, copy, rename,
/// delete, restore, purge, trash listing, versions, and search.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Files subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FilesCommand {
    /// List files and folders in a workspace directory.
    List {
        /// Workspace ID.
        workspace: String,
        /// Parent folder node ID (defaults to root).
        folder: Option<String>,
        /// Sort column: name, updated, created, type.
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        sort_dir: Option<String>,
        /// Page size: 100, 250, 500.
        page_size: Option<u32>,
        /// Cursor for next page.
        cursor: Option<String>,
    },
    /// Get details for one or more files or folders.
    ///
    /// `node_ids.len() == 1` keeps the single-node endpoint (shape
    /// `{node: {...}}`); 2+ ids route to the bulk endpoint and return
    /// `{nodes: [...], errors: [...]}`.
    Info {
        /// Workspace ID.
        workspace: String,
        /// One or more storage node IDs.
        node_ids: Vec<String>,
    },
    /// Create a new folder.
    CreateFolder {
        /// Workspace ID.
        workspace: String,
        /// Folder name.
        name: String,
        /// Parent folder node ID (defaults to root).
        parent: Option<String>,
    },
    /// Move a file or folder.
    Move {
        /// Workspace ID.
        workspace: String,
        /// Node ID to move.
        node_id: String,
        /// Destination folder node ID.
        to: String,
    },
    /// Copy a file or folder.
    Copy {
        /// Workspace ID.
        workspace: String,
        /// Node ID to copy.
        node_id: String,
        /// Destination folder node ID.
        to: String,
    },
    /// Rename a file or folder.
    Rename {
        /// Workspace ID.
        workspace: String,
        /// Node ID to rename.
        node_id: String,
        /// New name.
        new_name: String,
    },
    /// Delete a file or folder (move to trash).
    Delete {
        /// Workspace ID.
        workspace: String,
        /// Node ID to delete.
        node_id: String,
    },
    /// Restore a file or folder from trash.
    Restore {
        /// Workspace ID.
        workspace: String,
        /// Node ID to restore.
        node_id: String,
    },
    /// Permanently delete a trashed file or folder.
    Purge {
        /// Workspace ID.
        workspace: String,
        /// Node ID to purge.
        node_id: String,
    },
    /// List items in the trash.
    Trash {
        /// Workspace ID.
        workspace: String,
        /// Sort column: name, updated, created, type.
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        sort_dir: Option<String>,
        /// Page size.
        page_size: Option<u32>,
        /// Cursor for next page.
        cursor: Option<String>,
    },
    /// List versions of a file.
    Versions {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Search for files in a workspace.
    Search {
        /// Workspace ID.
        workspace: String,
        /// Search query.
        query: String,
        /// Page size: 100, 250, 500.
        page_size: Option<u32>,
        /// Cursor for next page.
        cursor: Option<String>,
    },
    /// List recently accessed files.
    Recent {
        /// Workspace ID.
        workspace: String,
        /// Page size: 100, 250, 500.
        page_size: Option<u32>,
        /// Cursor for next page.
        cursor: Option<String>,
    },
    /// Add a share link to a folder.
    AddLink {
        /// Workspace ID.
        workspace: String,
        /// Parent folder node ID.
        parent: String,
        /// Share ID to link.
        share_id: String,
    },
    /// Transfer a node to another workspace.
    Transfer {
        /// Workspace ID.
        workspace: String,
        /// Node ID to transfer.
        node_id: String,
        /// Target workspace ID.
        to_workspace: String,
    },
    /// Restore a specific version of a file.
    VersionRestore {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
        /// Version ID.
        version_id: String,
    },
    /// File lock subcommands.
    Lock(FileLockCommand),
    /// Read file content (text).
    Read {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Create or get a quickshare link.
    Quickshare {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
    },
}

/// File lock subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FileLockCommand {
    /// Acquire a file lock.
    Acquire {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Check lock status.
    Status {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Release a file lock.
    Release {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        lock_token: String,
    },
}

/// Allowed page sizes for storage list endpoints.
const VALID_PAGE_SIZES: &[u32] = &[100, 250, 500];

/// Maximum accepted length for a node or workspace identifier.
///
/// Real Fast.io node IDs are short (~32 chars including hyphens) and
/// workspace IDs are 19-digit numerics; the cap is generous but rejects
/// pathological inputs that would otherwise round-trip unchanged into
/// the URL path.
const MAX_ID_LEN: usize = 128;

/// Validate that an identifier is non-empty, within length, and uses
/// only the opaque-ID alphabet `[A-Za-z0-9_-]`.
///
/// Storage node and workspace IDs are documented (CLAUDE.md gotchas
/// #2/#3) as opaque alphanumeric strings (workspaces are 19-digit
/// numerics; nodes use hyphenated tokens like `2yxh5-ojakx-r3mwz`).
/// Special pseudo-IDs (`root`, `trash`) also fit the alphabet.
/// Rejecting anything else closes path-injection (`..`, `/`),
/// comma-smuggling (a node id containing `,` would split into two ids
/// after the proxy decodes `%2C` in some configurations), and
/// terminal-spoofing (control / bidi / zero-width codepoints).
///
/// Whitespace is rejected outright (no implicit `.trim()`) — round-2
/// review caught a defect where validation trimmed but the original
/// padded string flowed downstream into the URL and the dedup key,
/// producing two distinct ids that the server then handled
/// inconsistently. Length is byte-counted (safe because the alphabet
/// is ASCII; if the alphabet ever widens this needs to switch to
/// `chars().count()`).
fn validate_opaque_id(id: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!id.is_empty(), "{label} must not be empty");
    anyhow::ensure!(
        id.len() <= MAX_ID_LEN,
        "{label} must be at most {MAX_ID_LEN} characters (got {})",
        id.len()
    );
    anyhow::ensure!(
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{label} must only contain ASCII letters, digits, '-', and '_'"
    );
    Ok(())
}

/// Validate a storage node ID (delegates to [`validate_opaque_id`]).
fn validate_node_id(node_id: &str, label: &str) -> Result<()> {
    validate_opaque_id(node_id, label)
}

/// Validate a workspace ID (delegates to [`validate_opaque_id`]).
fn validate_workspace_id(workspace: &str) -> Result<()> {
    validate_opaque_id(workspace, "workspace ID")
}

/// Validate that a page size, if provided, is one of the accepted values.
fn validate_page_size(page_size: Option<u32>) -> Result<()> {
    if let Some(ps) = page_size {
        anyhow::ensure!(
            VALID_PAGE_SIZES.contains(&ps),
            "invalid page size {ps}. Must be one of: 100, 250, 500"
        );
    }
    Ok(())
}

/// Execute a files subcommand.
#[allow(clippy::too_many_lines)]
pub async fn execute(command: &FilesCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        FilesCommand::List {
            workspace,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => {
            let f = folder.as_deref().unwrap_or("root");
            list(
                ctx,
                workspace,
                f,
                sort_by.as_deref(),
                sort_dir.as_deref(),
                *page_size,
                cursor.as_deref(),
            )
            .await
        }
        FilesCommand::Info {
            workspace,
            node_ids,
        } => info(ctx, workspace, node_ids).await,
        FilesCommand::CreateFolder {
            workspace,
            name,
            parent,
        } => create_folder(ctx, workspace, parent.as_deref().unwrap_or("root"), name).await,
        FilesCommand::Move {
            workspace,
            node_id,
            to,
        } => move_node(ctx, workspace, node_id, to).await,
        FilesCommand::Copy {
            workspace,
            node_id,
            to,
        } => copy_node(ctx, workspace, node_id, to).await,
        FilesCommand::Rename {
            workspace,
            node_id,
            new_name,
        } => rename_node(ctx, workspace, node_id, new_name).await,
        FilesCommand::Delete { workspace, node_id } => delete_node(ctx, workspace, node_id).await,
        FilesCommand::Restore { workspace, node_id } => restore_node(ctx, workspace, node_id).await,
        FilesCommand::Purge { workspace, node_id } => purge_node(ctx, workspace, node_id).await,
        FilesCommand::Trash {
            workspace,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => {
            list_trash(
                ctx,
                workspace,
                sort_by.as_deref(),
                sort_dir.as_deref(),
                *page_size,
                cursor.as_deref(),
            )
            .await
        }
        FilesCommand::Versions { workspace, node_id } => {
            list_versions(ctx, workspace, node_id).await
        }
        FilesCommand::Search {
            workspace,
            query,
            page_size,
            cursor,
        } => search(ctx, workspace, query, *page_size, cursor.as_deref()).await,
        FilesCommand::Recent {
            workspace,
            page_size,
            cursor,
        } => recent(ctx, workspace, *page_size, cursor.as_deref()).await,
        FilesCommand::AddLink {
            workspace,
            parent,
            share_id,
        } => add_link(ctx, workspace, parent, share_id).await,
        FilesCommand::Transfer {
            workspace,
            node_id,
            to_workspace,
        } => transfer(ctx, workspace, node_id, to_workspace).await,
        FilesCommand::VersionRestore {
            workspace,
            node_id,
            version_id,
        } => version_restore(ctx, workspace, node_id, version_id).await,
        FilesCommand::Lock(cmd) => file_lock(cmd, ctx).await,
        FilesCommand::Read { workspace, node_id } => read_content(ctx, workspace, node_id).await,
        FilesCommand::Quickshare { workspace, node_id } => {
            quickshare(ctx, workspace, node_id).await
        }
    }
}

/// Handle file lock subcommands.
async fn file_lock(cmd: &FileLockCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match cmd {
        FileLockCommand::Acquire { workspace, node_id }
        | FileLockCommand::Status { workspace, node_id }
        | FileLockCommand::Release {
            workspace, node_id, ..
        } => {
            validate_workspace_id(workspace)?;
            validate_node_id(node_id, "node ID")?;
        }
    }
    let client = ctx.build_client()?;
    match cmd {
        FileLockCommand::Acquire { workspace, node_id } => {
            let value = api::storage::lock_acquire(&client, workspace, node_id)
                .await
                .context("failed to acquire lock")?;
            ctx.output.render(&value)?;
        }
        FileLockCommand::Status { workspace, node_id } => {
            let value = api::storage::lock_status(&client, workspace, node_id)
                .await
                .context("failed to get lock status")?;
            ctx.output.render(&value)?;
        }
        FileLockCommand::Release {
            workspace,
            node_id,
            lock_token,
        } => {
            api::storage::lock_release(&client, workspace, node_id, lock_token)
                .await
                .context("failed to release lock")?;
            let value = json!({
                "status": "released",
                "node_id": node_id,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// List files and folders.
async fn list(
    ctx: &CommandContext<'_>,
    workspace: &str,
    folder: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_page_size(page_size)?;
    let client = ctx.build_client()?;
    let value = api::storage::list_files(
        &client, workspace, folder, sort_by, sort_dir, page_size, cursor,
    )
    .await
    .context("failed to list files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get file/folder details for one or more nodes.
///
/// One id keeps the single-node endpoint shape; 2+ ids route through
/// the bulk endpoint with client-side chunking at
/// `api::storage::BULK_DETAILS_MAX_IDS`.
/// Runtime cap on positional node IDs per `fastio files info` invocation.
///
/// Bounds wall-time and rate-limit footprint. Enforced here (rather
/// than via clap `num_args = 1..=N`) so the error message can include
/// the actual count.
const INFO_MAX_NODE_IDS: usize = 1000;

async fn info(ctx: &CommandContext<'_>, workspace: &str, node_ids: &[String]) -> Result<()> {
    use std::collections::HashSet;

    validate_workspace_id(workspace)?;
    anyhow::ensure!(!node_ids.is_empty(), "at least one node ID is required");
    anyhow::ensure!(
        node_ids.len() <= INFO_MAX_NODE_IDS,
        "at most {INFO_MAX_NODE_IDS} node IDs accepted per call (got {})",
        node_ids.len()
    );
    for id in node_ids {
        validate_node_id(id, "node ID")?;
    }

    // Dedupe first (case-insensitive, matching server normalization)
    // — `unique.len()` is the right thing to test for the single-id
    // short-circuit, not the original argv length.
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique: Vec<String> = Vec::with_capacity(node_ids.len());
    for id in node_ids {
        if seen.insert(id.to_ascii_lowercase()) {
            unique.push(id.clone());
        }
    }

    let client = ctx.build_client()?;

    if unique.len() == 1 {
        let value = api::storage::get_file_details(&client, workspace, &unique[0])
            .await
            .context("failed to get file details")?;
        ctx.output.render(&value)?;
        return Ok(());
    }

    let aggregated = run_bulk_info(&client, workspace, &unique).await?;

    let succeeded = aggregated.succeeded.len();
    let errored = aggregated.errored.len();

    render_bulk_info(ctx, &aggregated)?;

    // Per platform docs: a 200 with non-empty errors is NOT a request
    // failure. Only exit nonzero when every requested id failed —
    // OR when the server returned nothing for any of them (a
    // hostile / buggy zero-zero response that would otherwise look
    // like silent success; round-2 review N3 / N6).
    if succeeded == 0 && errored > 0 {
        anyhow::bail!("all {errored} node id(s) failed; see errors output for details");
    }
    if succeeded == 0 && errored == 0 && aggregated.total > 0 {
        anyhow::bail!(
            "server returned no nodes and no errors for {} requested id(s); response was empty",
            aggregated.total
        );
    }
    Ok(())
}

/// Aggregated result of a bulk-info run: per-id success/failure
/// outcome plus the total requested-input count for the
/// `count_*` fields. `total` is captured separately because dedup
/// can drop ids before the server ever sees them.
struct BulkInfoAggregate {
    total: usize,
    succeeded: Vec<serde_json::Value>,
    errored: Vec<serde_json::Value>,
}

/// Issue the chunked bulk-info calls and aggregate per-id outcomes.
///
/// Network I/O lives here; aggregation is delegated to
/// [`aggregate_chunks`] so the dedup and exit-code logic can be
/// tested without an HTTP client.
async fn run_bulk_info(
    client: &fastio_cli::client::ApiClient,
    workspace: &str,
    unique: &[String],
) -> Result<BulkInfoAggregate> {
    let chunk_size = api::storage::BULK_DETAILS_MAX_IDS;
    let mut chunks: Vec<api::storage::BulkDetailsResponse> = Vec::new();
    for chunk in unique.chunks(chunk_size) {
        let resp = api::storage::get_bulk_node_details(client, workspace, chunk)
            .await
            .context("failed to fetch bulk node details")?;
        chunks.push(resp);
    }
    Ok(aggregate_chunks(unique.len(), chunks))
}

/// Aggregate per-chunk responses into a single result.
///
/// Server-returned nodes are deduplicated by id (a hostile or buggy
/// server returning the same node twice can't inflate the success
/// count), and per-id errors are deduplicated case-insensitively by
/// the echoed `node_id`. Both invariants protect the `count_*` fields
/// from going larger than `total`.
fn aggregate_chunks(
    total: usize,
    chunks: Vec<api::storage::BulkDetailsResponse>,
) -> BulkInfoAggregate {
    use std::collections::HashSet;

    let mut succeeded: Vec<serde_json::Value> = Vec::new();
    let mut errored: Vec<serde_json::Value> = Vec::new();
    let mut succeeded_lc: HashSet<String> = HashSet::new();
    let mut errored_lc: HashSet<String> = HashSet::new();

    for resp in chunks {
        for node in resp.nodes {
            // The server is the authority on node id; use the
            // returned object's `id` field to dedupe. Falls back
            // through `node_id` and `nid` for forward compat with
            // shape changes; absent any id field we keep the row.
            let key = node
                .get("id")
                .or_else(|| node.get("node_id"))
                .or_else(|| node.get("nid"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase);
            if let Some(k) = key
                && !succeeded_lc.insert(k)
            {
                tracing::warn!(node = %node, "dropping duplicate node id from server response");
                continue;
            }
            succeeded.push(node);
        }
        for err in resp.errors {
            if errored_lc.insert(err.node_id.to_ascii_lowercase()) {
                errored.push(json!({
                    "node_id": err.node_id,
                    "code": err.code,
                    "message": err.message,
                }));
            }
        }
    }

    BulkInfoAggregate {
        total,
        succeeded,
        errored,
    }
}

/// Render the aggregated bulk-info result.
///
/// Output format dispatch:
/// - JSON: emit the full `{count_*, nodes, errors}` map.
/// - Table / CSV: render the `nodes` array directly. Without
///   `serde_json/preserve_order`, `Value::Object` is a `BTreeMap`, so
///   `output::flatten_response` (`src/output/mod.rs:133`) walks keys
///   alphabetically and returns the first array-valued key. With
///   our key set `{count_errored, count_succeeded, count_total,
///   errors, nodes}`, alphabetical iteration lands on `errors`
///   first (E < N), which would silently hide the resolved nodes —
///   caught in correctness review A2. Passing an array directly
///   bypasses the heuristic and the renderer treats it as the
///   primary row data. Per-error summary lines are written to
///   stderr (suppressed in `--quiet`).
fn render_bulk_info(ctx: &CommandContext<'_>, agg: &BulkInfoAggregate) -> Result<()> {
    use fastio_cli::output::OutputFormat;

    let succeeded = agg.succeeded.len();
    let errored = agg.errored.len();
    let total = agg.total;

    if matches!(ctx.output.format, OutputFormat::Json) {
        let aggregated = json!({
            "count_total": total,
            "count_succeeded": succeeded,
            "count_errored": errored,
            "nodes": agg.succeeded,
            "errors": agg.errored,
        });
        ctx.output.render(&aggregated)?;
        return Ok(());
    }

    // Table / CSV: render nodes directly.
    ctx.output
        .render(&serde_json::Value::Array(agg.succeeded.clone()))?;
    if !agg.errored.is_empty() && !ctx.output.quiet {
        eprintln!("--- {errored} of {total} id(s) failed ---");
        for err in &agg.errored {
            let raw_nid = err
                .get("node_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            // Server-supplied `node_id` is left empty by the parser
            // when missing (no synthetic placeholder); render
            // `<no id>` only at the presentation layer so the data
            // layer stays clean (round-2 review N3).
            let nid = if raw_nid.is_empty() {
                "<no id>"
            } else {
                raw_nid
            };
            let code = err
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let msg = err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            eprintln!("  {nid}: [{code}] {msg}");
        }
    }
    Ok(())
}

/// Create a folder.
async fn create_folder(
    ctx: &CommandContext<'_>,
    workspace: &str,
    parent: &str,
    name: &str,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    anyhow::ensure!(!name.trim().is_empty(), "folder name must not be empty");
    let client = ctx.build_client()?;
    let value = api::storage::create_folder(&client, workspace, parent, name)
        .await
        .context("failed to create folder")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Move a file/folder.
async fn move_node(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    to: &str,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    validate_node_id(to, "destination folder ID")?;
    let client = ctx.build_client()?;
    api::storage::move_node(&client, workspace, node_id, to)
        .await
        .context("failed to move node")?;
    let value = json!({
        "status": "moved",
        "node_id": node_id,
        "destination": to,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Copy a file/folder.
async fn copy_node(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    to: &str,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    validate_node_id(to, "destination folder ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::copy_node(&client, workspace, node_id, to)
        .await
        .context("failed to copy node")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Rename a file/folder.
async fn rename_node(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    new_name: &str,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    anyhow::ensure!(!new_name.trim().is_empty(), "new name must not be empty");
    let client = ctx.build_client()?;
    let value = api::storage::rename_node(&client, workspace, node_id, new_name)
        .await
        .context("failed to rename node")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a file/folder (move to trash).
async fn delete_node(ctx: &CommandContext<'_>, workspace: &str, node_id: &str) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    api::storage::delete_node(&client, workspace, node_id)
        .await
        .context("failed to delete node (move to trash)")?;
    let value = json!({
        "status": "moved_to_trash",
        "node_id": node_id,
        "message": "Node moved to trash. Use 'files purge' to permanently delete or 'files restore' to recover.",
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Restore a file/folder from trash.
async fn restore_node(ctx: &CommandContext<'_>, workspace: &str, node_id: &str) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    api::storage::restore_node(&client, workspace, node_id)
        .await
        .context("failed to restore node from trash")?;
    let value = json!({
        "status": "restored",
        "node_id": node_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Permanently delete a trashed file/folder.
async fn purge_node(ctx: &CommandContext<'_>, workspace: &str, node_id: &str) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    api::storage::purge_node(&client, workspace, node_id)
        .await
        .context("failed to permanently delete node")?;
    let value = json!({
        "status": "permanently_deleted",
        "node_id": node_id,
        "message": "Node has been permanently deleted and cannot be recovered.",
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// List items in the trash.
async fn list_trash(
    ctx: &CommandContext<'_>,
    workspace: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_page_size(page_size)?;
    let client = ctx.build_client()?;
    let value = api::storage::list_trash(&client, workspace, sort_by, sort_dir, page_size, cursor)
        .await
        .context("failed to list trash")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List versions of a file.
async fn list_versions(ctx: &CommandContext<'_>, workspace: &str, node_id: &str) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::list_versions(&client, workspace, node_id)
        .await
        .context("failed to list versions")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Search for files.
async fn search(
    ctx: &CommandContext<'_>,
    workspace: &str,
    query: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_page_size(page_size)?;
    anyhow::ensure!(!query.trim().is_empty(), "search query must not be empty");
    let client = ctx.build_client()?;
    let value = api::storage::search_files(&client, workspace, query, page_size, cursor)
        .await
        .context("failed to search files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List recent files.
async fn recent(
    ctx: &CommandContext<'_>,
    workspace: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_page_size(page_size)?;
    let client = ctx.build_client()?;
    let value = api::storage::list_recent(&client, workspace, page_size, cursor)
        .await
        .context("failed to list recent files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Add a share link to a folder.
async fn add_link(
    ctx: &CommandContext<'_>,
    workspace: &str,
    parent: &str,
    share_id: &str,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(parent, "parent folder ID")?;
    validate_node_id(share_id, "share ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::add_link(&client, workspace, parent, share_id)
        .await
        .context("failed to add link")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Transfer a node to another workspace.
async fn transfer(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    to_workspace: &str,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    validate_workspace_id(to_workspace)?;
    let client = ctx.build_client()?;
    let value = api::storage::transfer_node(&client, workspace, node_id, to_workspace)
        .await
        .context("failed to transfer node")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Restore a specific version of a file.
async fn version_restore(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    version_id: &str,
) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    validate_node_id(version_id, "version ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::version_restore(&client, workspace, node_id, version_id)
        .await
        .context("failed to restore version")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Read file content.
async fn read_content(ctx: &CommandContext<'_>, workspace: &str, node_id: &str) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::read_content(&client, workspace, node_id)
        .await
        .context("failed to read file content")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get quickshare information for a file.
async fn quickshare(ctx: &CommandContext<'_>, workspace: &str, node_id: &str) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::quickshare_get(&client, workspace, node_id)
        .await
        .context("failed to get quickshare")?;
    ctx.output.render(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{aggregate_chunks, validate_node_id, validate_opaque_id};
    use fastio_cli::api::storage::{BulkDetailsResponse, parse_bulk_details_response};
    use serde_json::json;

    fn make_chunk(body: &serde_json::Value) -> BulkDetailsResponse {
        parse_bulk_details_response(body).expect("test body should parse")
    }

    #[test]
    fn aggregate_chunks_dedupes_repeated_node_ids_from_server() {
        // Hostile/buggy server returns the same node twice across two
        // chunks (different casings). count_succeeded must NOT exceed
        // total.
        let chunk_a = make_chunk(&json!({
            "format": "multi",
            "nodes": [{"id": "ABC", "name": "a.txt"}],
            "errors": []
        }));
        let chunk_b = make_chunk(&json!({
            "format": "multi",
            "nodes": [{"id": "abc", "name": "a.txt"}],
            "errors": []
        }));
        let agg = aggregate_chunks(2, vec![chunk_a, chunk_b]);
        assert_eq!(agg.succeeded.len(), 1);
        assert!(agg.errored.is_empty());
    }

    #[test]
    fn aggregate_chunks_dedupes_repeated_error_node_ids() {
        let chunk = make_chunk(&json!({
            "format": "multi",
            "nodes": [],
            "errors": [
                {"node_id": "X", "code": 133_123, "message": "missing"},
                {"node_id": "x", "code": 133_123, "message": "missing"}
            ]
        }));
        let agg = aggregate_chunks(1, vec![chunk]);
        assert!(agg.succeeded.is_empty());
        assert_eq!(agg.errored.len(), 1);
    }

    #[test]
    fn aggregate_chunks_keeps_nodes_without_id_field() {
        // No `id`/`node_id`/`nid` field — nothing to dedupe on, so
        // both rows pass through (better than silently dropping).
        let chunk = make_chunk(&json!({
            "format": "multi",
            "nodes": [{"name": "a"}, {"name": "b"}],
            "errors": []
        }));
        let agg = aggregate_chunks(2, vec![chunk]);
        assert_eq!(agg.succeeded.len(), 2);
    }

    #[test]
    fn aggregate_chunks_partial_success_yields_both_lists() {
        let chunk = make_chunk(&json!({
            "format": "multi",
            "nodes": [{"id": "ok"}],
            "errors": [{"node_id": "missing", "code": 133_123, "message": "missing"}]
        }));
        let agg = aggregate_chunks(2, vec![chunk]);
        assert_eq!(agg.succeeded.len(), 1);
        assert_eq!(agg.errored.len(), 1);
        assert_eq!(agg.total, 2);
    }

    #[test]
    fn aggregate_chunks_all_errored() {
        let chunk = make_chunk(&json!({
            "format": "multi",
            "nodes": [],
            "errors": [
                {"node_id": "a", "code": 133_123, "message": "missing"},
                {"node_id": "b", "code": 191_878, "message": "invalid"}
            ]
        }));
        let agg = aggregate_chunks(2, vec![chunk]);
        assert!(agg.succeeded.is_empty());
        assert_eq!(agg.errored.len(), 2);
    }

    #[test]
    fn aggregate_chunks_dedupes_node_id_alias_field() {
        // Server uses `node_id` instead of `id` on a node row — the
        // alias-aware dedup still catches the duplicate.
        let chunk = make_chunk(&json!({
            "format": "multi",
            "nodes": [
                {"node_id": "abc", "name": "a"},
                {"node_id": "ABC", "name": "a"}
            ],
            "errors": []
        }));
        let agg = aggregate_chunks(1, vec![chunk]);
        assert_eq!(agg.succeeded.len(), 1);
    }

    #[test]
    fn validate_opaque_id_accepts_real_node_ids() {
        // Hyphenated tokens like 2yxh5-ojakx-r3mwz-ty6tv-k66cj-nqsw,
        // 19-digit workspace numerics, and pseudo-IDs (root, trash).
        validate_opaque_id("2yxh5-ojakx-r3mwz-ty6tv-k66cj-nqsw", "node ID").unwrap();
        validate_opaque_id("4467703271501769252", "workspace ID").unwrap();
        validate_opaque_id("root", "node ID").unwrap();
        validate_opaque_id("trash", "node ID").unwrap();
    }

    #[test]
    fn validate_opaque_id_rejects_path_smuggling_chars() {
        for bad in [",", "..", "/", "abc/def", "a,b", "abc..def"] {
            let err =
                validate_node_id(bad, "node ID").expect_err(&format!("should reject {bad:?}"));
            assert!(
                err.to_string().contains("ASCII letters"),
                "unexpected error for {bad:?}: {err}"
            );
        }
    }

    #[test]
    fn validate_opaque_id_rejects_control_and_bidi() {
        for bad in [
            "abc\u{0000}",    // NUL
            "a\nb",           // embedded LF
            "abc\u{202E}xyz", // RLO bidi override
            "abc\u{200B}xyz", // zero-width space
            "abc\u{FEFF}",    // BOM
        ] {
            assert!(
                validate_node_id(bad, "node ID").is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn validate_opaque_id_rejects_whitespace_anywhere() {
        // Round-2: ANY whitespace (leading, trailing, embedded, NBSP)
        // is rejected outright. The previous version trimmed for
        // validation but then forwarded the untrimmed string,
        // creating a path/dedup mismatch.
        for bad in [
            "  abc",       // leading space
            "abc  ",       // trailing space
            "ab cd",       // embedded space
            "\tabc",       // leading tab
            "\u{00A0}abc", // NBSP (would also slip through trim)
        ] {
            assert!(
                validate_node_id(bad, "node ID").is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn validate_opaque_id_rejects_empty_and_oversize() {
        assert!(validate_node_id("", "node ID").is_err());
        let huge = "a".repeat(super::MAX_ID_LEN + 1);
        assert!(validate_node_id(&huge, "node ID").is_err());
    }
}
