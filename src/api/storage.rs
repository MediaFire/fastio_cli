#![allow(clippy::missing_errors_doc)]

/// Storage API endpoints for workspace file and folder operations.
///
/// Maps to endpoints documented at `/current/workspace/{workspace_id}/storage/`.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List files and folders in a workspace folder.
///
/// `GET /workspace/{workspace_id}/storage/{parent_id}/list/`
pub async fn list_files(
    client: &ApiClient,
    workspace_id: &str,
    parent_id: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(v) = sort_by {
        params.insert("sort_by".to_owned(), v.to_owned());
    }
    if let Some(v) = sort_dir {
        params.insert("sort_dir".to_owned(), v.to_owned());
    }
    if let Some(v) = page_size {
        params.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = cursor {
        params.insert("cursor".to_owned(), v.to_owned());
    }
    let path = format!(
        "/workspace/{}/storage/{}/list/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get details for a specific storage node.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/details/`
pub async fn get_file_details(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/details/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Create a new folder in workspace storage.
///
/// `POST /workspace/{workspace_id}/storage/{parent_id}/createfolder/`
pub async fn create_folder(
    client: &ApiClient,
    workspace_id: &str,
    parent_id: &str,
    name: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/createfolder/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    client.post(&path, &form).await
}

/// Move a storage node to a different parent folder.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/move/`
pub async fn move_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    target_parent_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("parent".to_owned(), target_parent_id.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/move/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Copy a storage node to a different parent folder.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/copy/`
pub async fn copy_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    target_parent_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("parent".to_owned(), target_parent_id.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/copy/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Rename (update) a storage node.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/update/`
pub async fn rename_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    new_name: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), new_name.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/update/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Move a storage node to trash (soft delete).
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/delete/`
pub async fn delete_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/delete/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.delete(&path).await
}

/// Restore a node from trash.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/restore/`
pub async fn restore_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/workspace/{}/storage/{}/restore/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Permanently delete a trashed node.
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/purge/`
pub async fn purge_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/purge/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.delete(&path).await
}

/// List items in the trash folder.
///
/// Uses the list endpoint with `trash` as the `parent_id`.
/// `GET /workspace/{workspace_id}/storage/trash/list/`
pub async fn list_trash(
    client: &ApiClient,
    workspace_id: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    list_files(
        client,
        workspace_id,
        "trash",
        sort_by,
        sort_dir,
        page_size,
        cursor,
    )
    .await
}

/// List versions of a storage node.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/versions/`
pub async fn list_versions(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/versions/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Search for files in a workspace.
///
/// `GET /workspace/{workspace_id}/storage/search/?search=<query>`
pub async fn search_files(
    client: &ApiClient,
    workspace_id: &str,
    query: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("search".to_owned(), query.to_owned());
    if let Some(v) = page_size {
        params.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = cursor {
        params.insert("cursor".to_owned(), v.to_owned());
    }
    let path = format!(
        "/workspace/{}/storage/search/",
        urlencoding::encode(workspace_id),
    );
    client.get_with_params(&path, &params).await
}

/// List recently accessed files.
///
/// `GET /workspace/{workspace_id}/storage/recent/`
pub async fn list_recent(
    client: &ApiClient,
    workspace_id: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(v) = page_size {
        params.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = cursor {
        params.insert("cursor".to_owned(), v.to_owned());
    }
    let path = format!(
        "/workspace/{}/storage/recent/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add a share link to a folder.
///
/// `POST /workspace/{workspace_id}/storage/{parent_id}/addlink/`
pub async fn add_link(
    client: &ApiClient,
    workspace_id: &str,
    parent_id: &str,
    share_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("share_id".to_owned(), share_id.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/addlink/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    client.post(&path, &form).await
}

/// Transfer a node to another workspace.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/transfer/`
pub async fn transfer_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    target_workspace_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "target_workspace_id".to_owned(),
        target_workspace_id.to_owned(),
    );
    let path = format!(
        "/workspace/{}/storage/{}/transfer/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Restore a specific version of a file.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/versions/{version_id}/restore/`
pub async fn version_restore(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    version_id: &str,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/workspace/{}/storage/{}/versions/{}/restore/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
        urlencoding::encode(version_id),
    );
    client.post(&path, &form).await
}

/// Acquire a file lock.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/lock/`
pub async fn lock_acquire(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/workspace/{}/storage/{}/lock/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Check lock status.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/lock/`
pub async fn lock_status(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/lock/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Release a file lock.
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/lock/`
///
/// The `lock_token` is the token returned by `lock_acquire` and must be
/// provided to prove ownership of the lock.
pub async fn lock_release(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    lock_token: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("lock_token".to_owned(), lock_token.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/lock/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.delete_with_form(&path, &form).await
}

/// Read file content (text).
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/content/`
pub async fn read_content(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/content/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Get or create a quickshare link.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/quickshare/`
pub async fn quickshare_get(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/quickshare/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}
