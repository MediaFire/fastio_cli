/// File and folder command implementations for `fastio files *`.
///
/// Handles listing, details, folder creation, move, copy, rename,
/// delete, restore, purge, trash listing, versions, and search.
use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use super::secret_output::capture_or_redact_lock_token;
use fastio_cli::api;

/// Which profile a flat file search (`/storage/search/`) runs against.
///
/// The two legs are the same endpoint family and take the identical parameter
/// set, so they differ only in the path the builder assembles — modelled as a
/// sum type rather than two `Option`s so an unreachable "neither" or "both"
/// state cannot be represented past the clap layer (clap already enforces
/// exactly-one via `conflicts_with` + `required_unless_present`).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SearchTarget {
    /// A workspace — the full-featured leg (`--filters` is workspace-only).
    Workspace(String),
    /// A share. Workspace-backed (folder) shares refuse search entirely.
    Share(String),
}

/// Files subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FilesCommand {
    /// List files and folders in a workspace directory.
    List {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
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
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// One or more storage node IDs.
        node_ids: Vec<String>,
    },
    /// Create a new folder.
    CreateFolder {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Folder name.
        name: String,
        /// Parent folder node ID (defaults to root).
        parent: Option<String>,
        /// Always create a new folder (auto-renamed on a name collision)
        /// instead of returning an existing same-named folder.
        force: bool,
    },
    /// Move a file or folder.
    Move {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID to move.
        node_id: String,
        /// Destination folder node ID.
        to: String,
    },
    /// Copy a file or folder.
    Copy {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID to copy.
        node_id: String,
        /// Destination folder node ID.
        to: String,
    },
    /// Rename a file or folder.
    Rename {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID to rename.
        node_id: String,
        /// New name.
        new_name: String,
    },
    /// Update a file or folder: rename, replace content, or set metadata
    /// title/short overrides. At least one field must be provided.
    Update {
        /// Workspace ID (omit when targeting a share).
        workspace: Option<String>,
        /// Share ID (alternative storage context to `--workspace`).
        share: Option<String>,
        /// Node ID to update.
        node_id: String,
        /// New name.
        name: Option<String>,
        /// JSON-encoded content source (same shape as add-file's `from`).
        from: Option<String>,
        /// Custom title override (max 50 chars; `null` clears).
        metadata_title: Option<String>,
        /// Custom short description override (max 2048 chars; `null` clears).
        metadata_short: Option<String>,
        /// Compare-and-swap precondition: the version id this update was
        /// derived from.
        if_version: Option<String>,
    },
    /// Add a file to a folder from a completed upload or by content hash.
    AddFile {
        /// Workspace ID (omit when targeting a share).
        workspace: Option<String>,
        /// Share ID (alternative storage context to `--workspace`).
        share: Option<String>,
        /// Parent folder node ID (defaults to root).
        parent: Option<String>,
        /// Filename for the new node.
        name: String,
        /// Completed upload session ID to attach (mutually exclusive with --hash).
        upload_id: Option<String>,
        /// Content hash to deduplicate against (requires --hash-type).
        hash: Option<String>,
        /// Hash algorithm for --hash (md5, sha1, sha256, sha384).
        hash_type: Option<String>,
    },
    /// Delete a file or folder (move to trash).
    Delete {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID to delete.
        node_id: String,
    },
    /// Restore a file or folder from trash.
    Restore {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID to restore.
        node_id: String,
    },
    /// Permanently delete a trashed file or folder.
    Purge {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID to purge.
        node_id: String,
    },
    /// List items in the trash.
    Trash {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
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
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID.
        node_id: String,
    },
    /// Search for files in a workspace or share (keyword + semantic).
    Search {
        /// Which profile to search — a workspace or a share.
        target: SearchTarget,
        /// Search query.
        query: String,
        /// Maximum number of results.
        limit: Option<u32>,
        /// Result offset for pagination.
        offset: Option<u32>,
        /// Comma-separated `nodeId:versionId` pairs (max 100).
        scope: Option<String>,
        /// Comma-separated `nodeId:depth` pairs (max 100).
        folder_scope: Option<String>,
        /// Metadata predicate array (workspace only) as the RAW `--filters`
        /// argument — inline JSON or `@path`. Shape-validated and resolved by
        /// `search`; kept as **text** end to end rather than a parsed `Value`
        /// so it reaches the wire byte-identical.
        filters: Option<String>,
        /// Enrich each hit with the full node resource.
        details: bool,
        /// `search_in` / `name_match` / `case_sensitive`.
        modes: api::types::SearchModeParams,
    },
    /// List recently accessed files.
    Recent {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Page size: 100, 250, 500.
        page_size: Option<u32>,
        /// Cursor for next page.
        cursor: Option<String>,
        /// Filter by node type: file, folder, link, note.
        node_type: Option<String>,
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
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
        /// Node ID.
        node_id: String,
        /// Version ID.
        version_id: String,
    },
    /// File lock subcommands.
    Lock(FileLockCommand),
    /// Read file content (text).
    Read {
        /// Workspace ID (`None` when `share` is set).
        workspace: Option<String>,
        /// Share ID — the alternative storage context.
        share: Option<String>,
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
        /// Lock duration in seconds (60-3600).
        duration: Option<u32>,
        /// Client metadata as a JSON object (e.g.
        /// `{"device_name":"…","client_version":"…"}`).
        client_info: Option<String>,
        /// Optional path to write the returned lock token to (0600). When
        /// absent the token is redacted from output.
        lock_token_file: Option<std::path::PathBuf>,
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
/// Storage node and workspace IDs are opaque alphanumeric strings
/// (workspaces are 19-digit numerics; nodes use hyphenated tokens like
/// `2yxh5-ojakx-r3mwz`).
/// Special pseudo-IDs (`root`, `trash`) also fit the alphabet.
/// Rejecting anything else closes path-injection (`..`, `/`),
/// comma-smuggling (a node id containing `,` would split into two ids
/// after the proxy decodes `%2C` in some configurations), and
/// terminal-spoofing (control / bidi / zero-width codepoints).
///
/// Whitespace is rejected outright (no implicit `.trim()`): trimming
/// during validation while the original padded string flows downstream
/// into the URL and the dedup key produces two distinct ids that the
/// server then handles inconsistently. Length is byte-counted (safe
/// because the alphabet
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
            share,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => {
            let f = folder.as_deref().unwrap_or("root");
            list(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
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
            share,
            node_ids,
        } => info(ctx, workspace.as_deref(), share.as_deref(), node_ids).await,
        FilesCommand::CreateFolder {
            workspace,
            share,
            name,
            parent,
            force,
        } => {
            create_folder(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                parent.as_deref().unwrap_or("root"),
                name,
                *force,
            )
            .await
        }
        FilesCommand::Move {
            workspace,
            share,
            node_id,
            to,
        } => move_node(ctx, workspace.as_deref(), share.as_deref(), node_id, to).await,
        FilesCommand::Copy {
            workspace,
            share,
            node_id,
            to,
        } => copy_node(ctx, workspace.as_deref(), share.as_deref(), node_id, to).await,
        FilesCommand::Rename {
            workspace,
            share,
            node_id,
            new_name,
        } => {
            rename_node(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                node_id,
                new_name,
            )
            .await
        }
        FilesCommand::Update {
            workspace,
            share,
            node_id,
            name,
            from,
            metadata_title,
            metadata_short,
            if_version,
        } => {
            update_node(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                node_id,
                name.as_deref(),
                from.as_deref(),
                metadata_title.as_deref(),
                metadata_short.as_deref(),
                if_version.as_deref(),
            )
            .await
        }
        FilesCommand::AddFile {
            workspace,
            share,
            parent,
            name,
            upload_id,
            hash,
            hash_type,
        } => {
            add_file(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                parent.as_deref().unwrap_or("root"),
                name,
                upload_id.as_deref(),
                hash.as_deref(),
                hash_type.as_deref(),
            )
            .await
        }
        FilesCommand::Delete {
            workspace,
            share,
            node_id,
        } => delete_node(ctx, workspace.as_deref(), share.as_deref(), node_id).await,
        FilesCommand::Restore {
            workspace,
            share,
            node_id,
        } => restore_node(ctx, workspace.as_deref(), share.as_deref(), node_id).await,
        FilesCommand::Purge {
            workspace,
            share,
            node_id,
        } => purge_node(ctx, workspace.as_deref(), share.as_deref(), node_id).await,
        FilesCommand::Trash {
            workspace,
            share,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => {
            list_trash(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                sort_by.as_deref(),
                sort_dir.as_deref(),
                *page_size,
                cursor.as_deref(),
            )
            .await
        }
        FilesCommand::Versions {
            workspace,
            share,
            node_id,
        } => list_versions(ctx, workspace.as_deref(), share.as_deref(), node_id).await,
        FilesCommand::Search {
            target,
            query,
            limit,
            offset,
            scope,
            folder_scope,
            filters,
            details,
            modes,
        } => {
            search(
                ctx,
                target,
                query,
                *limit,
                *offset,
                scope.as_deref(),
                folder_scope.as_deref(),
                filters.as_deref(),
                *details,
                modes.clone(),
            )
            .await
        }
        FilesCommand::Recent {
            workspace,
            share,
            page_size,
            cursor,
            node_type,
        } => {
            recent(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                *page_size,
                cursor.as_deref(),
                node_type.as_deref(),
            )
            .await
        }
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
            share,
            node_id,
            version_id,
        } => {
            version_restore(
                ctx,
                workspace.as_deref(),
                share.as_deref(),
                node_id,
                version_id,
            )
            .await
        }
        FilesCommand::Lock(cmd) => file_lock(cmd, ctx).await,
        FilesCommand::Read {
            workspace,
            share,
            node_id,
        } => read_content(ctx, workspace.as_deref(), share.as_deref(), node_id).await,
    }
}

/// Handle file lock subcommands.
async fn file_lock(cmd: &FileLockCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match cmd {
        FileLockCommand::Acquire {
            workspace, node_id, ..
        }
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
        FileLockCommand::Acquire {
            workspace,
            node_id,
            duration,
            client_info,
            lock_token_file,
        } => {
            let mut value = api::storage::lock_acquire(
                &client,
                "workspace",
                workspace,
                node_id,
                *duration,
                client_info.as_deref(),
            )
            .await
            .context("failed to acquire lock")?;
            // Same guard as `fastio lock acquire`, same helper: a bearer token
            // must not reach stdout. Two acquire surfaces, one implementation.
            capture_or_redact_lock_token(&mut value, lock_token_file.as_deref(), ctx.output.quiet)?;
            ctx.output.render(&value)?;
        }
        FileLockCommand::Status { workspace, node_id } => {
            let value = api::storage::lock_status(&client, "workspace", workspace, node_id)
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
// One over the lint, for the same reason as `api::storage::list_files`: a
// params struct for six positionals — five already `Option` — would cost more
// clarity than it buys.
#[allow(clippy::too_many_arguments)]
async fn list(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    folder: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_page_size(page_size)?;
    let client = ctx.build_client()?;
    let value = api::storage::list_files(
        &client,
        context_type,
        workspace,
        folder,
        sort_by,
        sort_dir,
        page_size,
        cursor,
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

async fn info(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_ids: &[String],
) -> Result<()> {
    use std::collections::HashSet;

    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;

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
        let value = api::storage::get_file_details(&client, context_type, workspace, &unique[0])
            .await
            .context("failed to get file details")?;
        ctx.output.render(&value)?;
        return Ok(());
    }

    let aggregated = run_bulk_info(&client, context_type, workspace, &unique).await?;

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
    context_type: &str,
    workspace: &str,
    unique: &[String],
) -> Result<BulkInfoAggregate> {
    let chunk_size = api::storage::BULK_DETAILS_MAX_IDS;
    let mut chunks: Vec<api::storage::BulkDetailsResponse> = Vec::new();
    for chunk in unique.chunks(chunk_size) {
        let resp = api::storage::get_bulk_node_details(client, context_type, workspace, chunk)
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
    workspace: Option<&str>,
    share: Option<&str>,
    parent: &str,
    name: &str,
    force: bool,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    anyhow::ensure!(!name.trim().is_empty(), "folder name must not be empty");
    let client = ctx.build_client()?;
    let value = api::storage::create_folder(&client, context_type, workspace, parent, name, force)
        .await
        .context("failed to create folder")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Resolve the storage context (`type`, `id`) from a `--workspace` /
/// `--share` selector. Exactly one must be supplied.
fn resolve_storage_ctx<'a>(
    workspace: Option<&'a str>,
    share: Option<&'a str>,
) -> Result<(&'static str, &'a str)> {
    match (workspace, share) {
        (Some(w), None) => {
            validate_workspace_id(w)?;
            Ok(("workspace", w))
        }
        (None, Some(s)) => {
            validate_opaque_id(s, "share ID")?;
            Ok(("share", s))
        }
        (Some(_), Some(_)) => {
            anyhow::bail!("provide either --workspace or --share, not both")
        }
        (None, None) => anyhow::bail!("provide --workspace or --share"),
    }
}

/// Update a file/folder: rename, replace content, or override metadata.
#[allow(clippy::too_many_arguments)]
async fn update_node(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
    name: Option<&str>,
    from: Option<&str>,
    metadata_title: Option<&str>,
    metadata_short: Option<&str>,
    if_version: Option<&str>,
) -> Result<()> {
    let (context_type, context_id) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    api::storage::reject_unsupported_if_version(if_version)?;
    anyhow::ensure!(
        name.is_some() || from.is_some() || metadata_title.is_some() || metadata_short.is_some(),
        "provide at least one of --name, --from, --metadata-title, or --metadata-short"
    );
    // A precondition that cannot fail protects nothing.
    //
    // The test is NOT "did the version advance" — an identical-byte replace
    // does not advance it either, and that case is harmless because nothing
    // different was written. The test is: **was something written that the
    // version does not cover?** Custom metadata is written OUTSIDE the locked,
    // version-creating transaction, so a metadata-only update can leave the
    // version where it was while having written something real and DIFFERENT
    // per writer. The base never becomes stale, two writers on the same base
    // both pass, and the second silently overwrites the first — while
    // `--if-version` reports success.
    //
    // That distinction is why this guard keys on `--name`/`--from` (the only
    // inputs the server turns into version operations) and not on "the version
    // did not move", which would wrongly condemn a no-op replace.
    //
    // Measured against the deployed API (2026-08-24) through this binary:
    // writer A `--metadata-short alpha --if-version V` → exit 0; writer B
    // `--metadata-short beta --if-version V` (same V, no re-read) → exit 0;
    // version unchanged, B's value won. Refusing is the only honest response —
    // accepting it would report a protection that cannot exist.
    anyhow::ensure!(
        if_version.is_none() || name.is_some() || from.is_some(),
        "--if-version cannot protect a metadata-only update: the server creates no new version \
         for --metadata-title/--metadata-short, so the base never goes stale and a concurrent \
         writer on the same base also succeeds. Pair it with --name or --from (which do create a \
         version), or drop --if-version and accept last-write-wins for metadata."
    );
    let client = ctx.build_client()?;
    let value = api::storage::update_node(
        &client,
        context_type,
        context_id,
        node_id,
        name,
        from,
        metadata_title,
        metadata_short,
        if_version,
    )
    .await
    .context("failed to update node")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Build the JSON `from` source object for `files add-file`. Exactly one
/// source must be provided: a completed `upload_id`, or a `hash` paired with
/// a `hash_type`.
pub(crate) fn build_addfile_from(
    upload_id: Option<&str>,
    hash: Option<&str>,
    hash_type: Option<&str>,
) -> Result<String> {
    match (upload_id, hash, hash_type) {
        (Some(id), None, None) => {
            anyhow::ensure!(!id.trim().is_empty(), "--upload-id must not be empty");
            Ok(json!({ "type": "upload", "upload": { "id": id } }).to_string())
        }
        (None, Some(h), Some(ht)) => {
            anyhow::ensure!(!h.trim().is_empty(), "--hash must not be empty");
            Ok(json!({ "type": "hash", "hash": { "hash": h, "hash_type": ht } }).to_string())
        }
        (None, Some(_), None) => {
            anyhow::bail!("--hash requires --hash-type (md5, sha1, sha256, sha384)")
        }
        (None, None, Some(_)) => anyhow::bail!("--hash-type requires --hash"),
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            anyhow::bail!("provide either --upload-id or --hash/--hash-type, not both")
        }
        (None, None, None) => {
            anyhow::bail!("provide a source: --upload-id, or --hash with --hash-type")
        }
    }
}

/// Reject a hash-based `add-file` source in a workspace context.
///
/// The workspace add-file handler accepts only an upload source — hash-based
/// dedup is currently supported only in a share context. Reject early with a
/// clear message instead of letting the server return a generic 1605 error.
///
/// Shared with the MCP `files` handler so an MCP `add-file` (which is
/// workspace-scoped) rejects a hash source the same way the CLI does.
pub(crate) fn validate_addfile_hash_context(context_type: &str, hash: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        !(hash.is_some() && context_type == "workspace"),
        "hash-based add-file is currently only supported in a share context; \
         use --upload-id for a workspace (or supply --share with --hash)"
    );
    Ok(())
}

/// Add a file to a folder from a completed upload or by content-hash dedup.
#[allow(clippy::too_many_arguments)]
async fn add_file(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    parent: &str,
    name: &str,
    upload_id: Option<&str>,
    hash: Option<&str>,
    hash_type: Option<&str>,
) -> Result<()> {
    let (context_type, context_id) = resolve_storage_ctx(workspace, share)?;
    anyhow::ensure!(!name.trim().is_empty(), "filename must not be empty");
    validate_addfile_hash_context(context_type, hash)?;
    let from = build_addfile_from(upload_id, hash, hash_type)?;
    let client = ctx.build_client()?;
    let value = api::storage::add_file(&client, context_type, context_id, parent, name, &from)
        .await
        .context("failed to add file")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Move a file/folder.
async fn move_node(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
    to: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    validate_node_id(to, "destination folder ID")?;
    let client = ctx.build_client()?;
    api::storage::move_node(&client, context_type, workspace, node_id, to)
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
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
    to: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    validate_node_id(to, "destination folder ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::copy_node(&client, context_type, workspace, node_id, to)
        .await
        .context("failed to copy node")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Rename a file/folder.
async fn rename_node(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
    new_name: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    anyhow::ensure!(!new_name.trim().is_empty(), "new name must not be empty");
    let client = ctx.build_client()?;
    let value = api::storage::rename_node(&client, context_type, workspace, node_id, new_name)
        .await
        .context("failed to rename node")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a file/folder (move to trash).
async fn delete_node(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    api::storage::delete_node(&client, context_type, workspace, node_id)
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
async fn restore_node(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    api::storage::restore_node(&client, context_type, workspace, node_id)
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
async fn purge_node(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    api::storage::purge_node(&client, context_type, workspace, node_id)
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
    workspace: Option<&str>,
    share: Option<&str>,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_page_size(page_size)?;
    let client = ctx.build_client()?;
    let value = api::storage::list_trash(
        &client,
        context_type,
        workspace,
        sort_by,
        sort_dir,
        page_size,
        cursor,
    )
    .await
    .context("failed to list trash")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List versions of a file.
async fn list_versions(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::list_versions(&client, context_type, workspace, node_id)
        .await
        .context("failed to list versions")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Search for files by filename, contents, or both.
#[allow(clippy::too_many_arguments)]
async fn search(
    ctx: &CommandContext<'_>,
    target: &SearchTarget,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    scope: Option<&str>,
    folder_scope: Option<&str>,
    filters: Option<&str>,
    details: bool,
    modes: api::types::SearchModeParams,
) -> Result<()> {
    match target {
        SearchTarget::Workspace(id) => validate_workspace_id(id)?,
        SearchTarget::Share(id) => validate_opaque_id(id, "share ID")?,
    }
    anyhow::ensure!(!query.trim().is_empty(), "search query must not be empty");
    // Shape-validate and resolve any `@file` form BEFORE building a client, so
    // a malformed predicate fails without a network round trip.
    let filters = super::parse_json_object_array_arg(filters, "--filters")?;
    let client = ctx.build_client()?;
    let params = api::storage::SearchFilesParams::new()
        .files_scope(scope)
        .folders_scope(folder_scope)
        .filters(filters.as_deref())
        .limit(limit)
        .offset(offset)
        .details(details)
        .modes(modes);
    // Same parameter set on both legs — only the path differs.
    //
    // `--filters` is forwarded on the share leg rather than refused here.
    // "Workspace routes only" is SERVER policy, and a client-side refusal would
    // outlive the policy it encodes; a share currently answers 200 with
    // UNFILTERED results and no `metadata_filter` block, which the
    // response-side check below reports loudly. That keeps the client honest
    // without freezing today's server behaviour into it.
    let value = match target {
        SearchTarget::Workspace(id) => {
            api::storage::search_files(&client, "workspace", id, query, params)
                .await
                .context("failed to search files")?
        }
        SearchTarget::Share(id) => api::storage::search_files_share(&client, id, query, params)
            .await
            .map_err(map_share_file_search_error)?,
    };
    // The keyword-only response returns `files` as a MAP keyed by node id;
    // normalize it to an array so every format renders one row per file.
    // (In-place rewrite of `files` only — the top-level `metadata_filter`
    // acknowledgement the warning below reads is preserved.)
    let value = api::storage::normalize_search_response(value);
    ctx.warn_if_content_search_unavailable(&value);
    // Only meaningful when we actually sent a predicate: for an unfiltered
    // search the block's absence is the normal, correct response shape.
    if filters.is_some() {
        ctx.warn_about_metadata_filter(&value);
    }
    ctx.output.render(&value)?;
    Ok(())
}

/// The message shown when a FLAT file search hits a workspace-backed (folder)
/// share.
///
/// Distinct from `commands::search::FOLDER_SHARE_SEARCH_UNAVAILABLE`, which
/// names *unified* search: the condition is the same and the predicate is
/// shared, but the two surfaces must not describe each other. Shared with the
/// MCP `files search` share leg so CLI and MCP cannot drift on the wording.
pub const FILE_SEARCH_UNAVAILABLE_ON_FOLDER_SHARE: &str =
    "file search is not available for workspace-backed (folder) shares";

/// Map a share FLAT-search (`/share/{id}/storage/search/`) error to a friendlier
/// message for the folder-share case.
///
/// Reuses the unified-search predicate deliberately: **both** share search
/// routes emit `139420` for this same condition, confirmed server-side, so the
/// classifier is shared and only the wording differs (this leg is a flat file
/// list, not the grouped-bucket search).
///
/// Every other error passes through carrying the server's own message — in
/// particular the **orphaned share**, which is `179101` on THIS route and
/// `142794` on the unified one. That is a different condition ("this shared
/// folder no longer exists") and relabelling it as "folder shares cannot be
/// searched" would be a confidently wrong answer.
fn map_share_file_search_error(err: fastio_cli::error::CliError) -> anyhow::Error {
    if super::search::is_folder_share_search_error(&err) {
        return anyhow::anyhow!(FILE_SEARCH_UNAVAILABLE_ON_FOLDER_SHARE);
    }
    anyhow::Error::new(err).context("failed to search files in share")
}

/// List recent files.
async fn recent(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
    node_type: Option<&str>,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_page_size(page_size)?;
    let client = ctx.build_client()?;
    let value = api::storage::list_recent(
        &client,
        context_type,
        workspace,
        page_size,
        cursor,
        node_type,
    )
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
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
    version_id: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    validate_node_id(version_id, "version ID")?;
    let client = ctx.build_client()?;
    let value =
        api::storage::version_restore(&client, context_type, workspace, node_id, version_id)
            .await
            .context("failed to restore version")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Read file content.
async fn read_content(
    ctx: &CommandContext<'_>,
    workspace: Option<&str>,
    share: Option<&str>,
    node_id: &str,
) -> Result<()> {
    let (context_type, workspace) = resolve_storage_ctx(workspace, share)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::read_content(&client, context_type, workspace, node_id)
        .await
        .context("failed to read file content")?;
    ctx.output.render(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        aggregate_chunks, build_addfile_from, map_share_file_search_error, resolve_storage_ctx,
        validate_addfile_hash_context, validate_node_id, validate_opaque_id,
    };
    use fastio_cli::api::storage::{BulkDetailsResponse, parse_bulk_details_response};
    use serde_json::{Value, json};

    fn make_chunk(body: &serde_json::Value) -> BulkDetailsResponse {
        parse_bulk_details_response(body).expect("test body should parse")
    }

    #[test]
    fn addfile_from_upload_source() {
        let s = build_addfile_from(Some("u1"), None, None).expect("ok");
        let v: Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(v["type"], "upload");
        assert_eq!(v["upload"]["id"], "u1");
    }

    #[test]
    fn addfile_from_hash_source() {
        let s = build_addfile_from(None, Some("abc123"), Some("sha256")).expect("ok");
        let v: Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(v["type"], "hash");
        assert_eq!(v["hash"]["hash"], "abc123");
        assert_eq!(v["hash"]["hash_type"], "sha256");
    }

    #[test]
    fn addfile_from_rejects_missing_source() {
        assert!(build_addfile_from(None, None, None).is_err());
    }

    #[test]
    fn addfile_from_rejects_hash_without_type() {
        assert!(build_addfile_from(None, Some("abc"), None).is_err());
    }

    #[test]
    fn addfile_from_rejects_both_sources() {
        assert!(build_addfile_from(Some("u1"), Some("abc"), Some("sha256")).is_err());
    }

    #[test]
    fn addfile_hash_rejected_in_workspace_context() {
        // The workspace handler only accepts an upload source.
        assert!(validate_addfile_hash_context("workspace", Some("abc")).is_err());
    }

    #[test]
    fn addfile_hash_allowed_in_share_context() {
        assert!(validate_addfile_hash_context("share", Some("abc")).is_ok());
    }

    #[test]
    fn addfile_upload_allowed_in_both_contexts() {
        // No hash → upload-only path, valid for workspace and share alike.
        assert!(validate_addfile_hash_context("workspace", None).is_ok());
        assert!(validate_addfile_hash_context("share", None).is_ok());
    }

    #[test]
    fn storage_ctx_resolves_workspace_or_share() {
        assert_eq!(
            resolve_storage_ctx(Some("19"), None).expect("ok"),
            ("workspace", "19")
        );
        assert_eq!(
            resolve_storage_ctx(None, Some("55")).expect("ok"),
            ("share", "55")
        );
        assert!(resolve_storage_ctx(None, None).is_err());
        assert!(resolve_storage_ctx(Some("19"), Some("55")).is_err());
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
    fn validate_opaque_id_accepts_29_and_30_char_forms() {
        // Regression guard: OpaqueIds are no longer fixed-length. Workflow-family
        // ids are 30 chars (35 hyphenated); everything else is 29 (34 hyphenated).
        // The validator must accept BOTH lengths in raw and hyphenated form so a
        // future 29-only assumption can't slip back in.
        for id in [
            "f3jm5zqzfxpxdr2dx8z5bvnb3rpjf",       // 29-char raw (non-workflow)
            "f3jm5-zqzfx-pxdr2-dx8z5-bvnb3-rpjf",  // 34-char hyphenated
            "wa3jm5zqzfxpxdr2dx8z5bvnb3rpjf",      // 30-char raw (workflow)
            "wa3jm-5zqzf-xpxdr-2dx8z-5bvnb-3rpjf", // 35-char hyphenated
        ] {
            validate_opaque_id(id, "id").unwrap_or_else(|e| panic!("rejected {id:?}: {e}"));
        }
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

    /// Only the folder-share condition gets the friendly flat-search message;
    /// every other 404 on this route keeps the server's own text.
    ///
    /// The orphaned-share case is the sharp one: on THIS route it is `179101`
    /// (the unified route uses `142794` for the same condition), and it is
    /// reachable on the same call. Relabelling it as "folder shares cannot be
    /// searched" would tell a user their share cannot be searched when the
    /// truth is it no longer exists.
    #[test]
    fn share_file_search_error_maps_only_the_folder_share_case() {
        let err = |code: u32, msg: &str| {
            fastio_cli::error::CliError::Api(fastio_cli::error::ApiError::new(
                code,
                None,
                msg.to_owned(),
                404,
            ))
        };

        let mapped =
            map_share_file_search_error(err(139_420, "Search is not available for Shared Folders"));
        assert!(
            mapped.to_string().contains("file search is not available"),
            "the folder-share case gets the FLAT wording, got: {mapped}"
        );
        assert!(
            !mapped.to_string().contains("unified"),
            "must not borrow the unified-search wording, got: {mapped}"
        );

        for (code, msg) in [
            (179_101_u32, "This shared folder no longer exists"),
            (142_794, "This shared folder no longer exists"),
            (9992, "Resource not found."),
        ] {
            let mapped = map_share_file_search_error(err(code, msg));
            // `{:#}` — plain Display shows only the outermost context; the
            // server's message lives in the source chain.
            let full = format!("{mapped:#}");
            assert!(
                full.contains(msg),
                "code {code} must keep the server's own message, got: {full}"
            );
            assert!(
                !full.contains("workspace-backed"),
                "code {code} must NOT be relabelled as the folder-share case, got: {full}"
            );
        }
    }
}
