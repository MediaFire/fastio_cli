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
    /// Get details for a file or folder.
    Info {
        /// Workspace ID.
        workspace: String,
        /// Node ID.
        node_id: String,
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

/// Validate that a node ID is not empty or whitespace-only.
fn validate_node_id(node_id: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!node_id.trim().is_empty(), "{label} must not be empty");
    Ok(())
}

/// Validate that a workspace ID is not empty or whitespace-only.
fn validate_workspace_id(workspace: &str) -> Result<()> {
    anyhow::ensure!(
        !workspace.trim().is_empty(),
        "workspace ID must not be empty"
    );
    Ok(())
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
        FilesCommand::Info { workspace, node_id } => info(ctx, workspace, node_id).await,
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

/// Get file/folder details.
async fn info(ctx: &CommandContext<'_>, workspace: &str, node_id: &str) -> Result<()> {
    validate_workspace_id(workspace)?;
    validate_node_id(node_id, "node ID")?;
    let client = ctx.build_client()?;
    let value = api::storage::get_file_details(&client, workspace, node_id)
        .await
        .context("failed to get file details")?;
    ctx.output.render(&value)?;
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
