#![allow(clippy::missing_errors_doc)]

/// Share management API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for share CRUD, storage operations, and member listing.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List all shares accessible to the current user.
///
/// `GET /shares/all/`
pub async fn list_shares(
    client: &ApiClient,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get("/shares/all/").await
    } else {
        client.get_with_params("/shares/all/", &params).await
    }
}

/// Parameters for creating a new share.
pub struct CreateShareParams<'a> {
    /// Workspace ID to create the share in.
    pub workspace_id: &'a str,
    /// Share name/title.
    pub title: &'a str,
    /// Description.
    pub description: Option<&'a str>,
    /// Access options.
    pub access_options: Option<&'a str>,
    /// Password for share access.
    pub password: Option<&'a str>,
    /// Enable anonymous uploads.
    pub anonymous_uploads_enabled: Option<bool>,
    /// Enable AI intelligence features.
    pub intelligence: Option<bool>,
    /// Download security level ("high", "medium", or "off").
    pub download_security: Option<&'a str>,
}

/// Create a new share on a workspace.
///
/// `POST /workspace/{workspace_id}/create/share/`
pub async fn create_share(
    client: &ApiClient,
    params: &CreateShareParams<'_>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("title".to_owned(), params.title.to_owned());
    form.insert("share_type".to_owned(), "send".to_owned());
    if let Some(v) = params.description {
        form.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = params.access_options {
        form.insert("access_options".to_owned(), v.to_owned());
    }
    if let Some(v) = params.password {
        form.insert("password".to_owned(), v.to_owned());
    }
    if let Some(v) = params.anonymous_uploads_enabled {
        form.insert("anonymous_uploads_enabled".to_owned(), v.to_string());
    }
    if let Some(v) = params.intelligence {
        form.insert("intelligence".to_owned(), v.to_string());
    }
    if let Some(v) = params.download_security {
        form.insert("download_security".to_owned(), v.to_owned());
    }
    let path = format!(
        "/workspace/{}/create/share/",
        urlencoding::encode(params.workspace_id),
    );
    client.post(&path, &form).await
}

/// Get share details.
///
/// `GET /share/{share_id}/details/`
pub async fn get_share_details(client: &ApiClient, share_id: &str) -> Result<Value, CliError> {
    let path = format!("/share/{}/details/", urlencoding::encode(share_id),);
    client.get(&path).await
}

/// Parameters for updating share settings.
pub struct UpdateShareParams<'a> {
    /// Share ID.
    pub share_id: &'a str,
    /// New name.
    pub name: Option<&'a str>,
    /// New description.
    pub description: Option<&'a str>,
    /// New access options.
    pub access_options: Option<&'a str>,
    /// Enable/disable downloads (legacy — prefer `download_security`).
    pub download_enabled: Option<bool>,
    /// Enable/disable comments.
    pub comments_enabled: Option<bool>,
    /// Enable/disable anonymous uploads.
    pub anonymous_uploads_enabled: Option<bool>,
    /// Download security level ("high", "medium", or "off").
    pub download_security: Option<&'a str>,
}

/// Update share settings.
///
/// `POST /share/{share_id}/update/`
pub async fn update_share(
    client: &ApiClient,
    params: &UpdateShareParams<'_>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(v) = params.name {
        form.insert("name".to_owned(), v.to_owned());
    }
    if let Some(v) = params.description {
        form.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = params.access_options {
        form.insert("access_options".to_owned(), v.to_owned());
    }
    if let Some(v) = params.download_enabled {
        form.insert("download_enabled".to_owned(), v.to_string());
    }
    if let Some(v) = params.comments_enabled {
        form.insert("comments_enabled".to_owned(), v.to_string());
    }
    if let Some(v) = params.anonymous_uploads_enabled {
        form.insert("anonymous_uploads_enabled".to_owned(), v.to_string());
    }
    if let Some(v) = params.download_security {
        form.insert("download_security".to_owned(), v.to_owned());
    }
    let path = format!("/share/{}/update/", urlencoding::encode(params.share_id),);
    client.post(&path, &form).await
}

/// Delete a share.
///
/// `DELETE /share/{share_id}/delete/`
pub async fn delete_share(
    client: &ApiClient,
    share_id: &str,
    confirm: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("confirm".to_owned(), confirm.to_owned());
    let path = format!("/share/{}/delete/", urlencoding::encode(share_id),);
    client.delete_with_form(&path, &form).await
}

/// Parameters for listing files in a share's storage.
pub struct ListShareFilesParams<'a> {
    /// Share ID.
    pub share_id: &'a str,
    /// Parent folder node ID.
    pub parent_id: &'a str,
    /// Sort column.
    pub sort_by: Option<&'a str>,
    /// Sort direction.
    pub sort_dir: Option<&'a str>,
    /// Page size.
    pub page_size: Option<u32>,
    /// Cursor for pagination.
    pub cursor: Option<&'a str>,
}

/// List files in a share's storage.
///
/// `GET /share/{share_id}/storage/{parent_id}/list/`
pub async fn list_share_files(
    client: &ApiClient,
    params: &ListShareFilesParams<'_>,
) -> Result<Value, CliError> {
    let mut query = HashMap::new();
    if let Some(v) = params.sort_by {
        query.insert("sort_by".to_owned(), v.to_owned());
    }
    if let Some(v) = params.sort_dir {
        query.insert("sort_dir".to_owned(), v.to_owned());
    }
    if let Some(v) = params.page_size {
        query.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = params.cursor {
        query.insert("cursor".to_owned(), v.to_owned());
    }
    let path = format!(
        "/share/{}/storage/{}/list/",
        urlencoding::encode(params.share_id),
        urlencoding::encode(params.parent_id),
    );
    if query.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &query).await
    }
}

/// List members of a share.
///
/// `GET /share/{share_id}/members/list/`
pub async fn list_share_members(
    client: &ApiClient,
    share_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!("/share/{}/members/list/", urlencoding::encode(share_id),);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add a member to a share.
///
/// `POST /share/{share_id}/members/{email}/`
pub async fn add_share_member(
    client: &ApiClient,
    share_id: &str,
    email: &str,
    role: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "permissions".to_owned(),
        role.unwrap_or("member").to_owned(),
    );
    let path = format!(
        "/share/{}/members/{}/",
        urlencoding::encode(share_id),
        urlencoding::encode(email),
    );
    client.post(&path, &form).await
}

/// Get public details for a share (no auth required for some shares).
///
/// `GET /share/{share_id}/public/details/`
pub async fn get_share_public_details(
    client: &ApiClient,
    share_id: &str,
) -> Result<Value, CliError> {
    let path = format!("/share/{}/public/details/", urlencoding::encode(share_id));
    client.get(&path).await
}

/// Archive a share.
///
/// `POST /share/{share_id}/archive/`
pub async fn archive_share(client: &ApiClient, share_id: &str) -> Result<Value, CliError> {
    let path = format!("/share/{}/archive/", urlencoding::encode(share_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// Unarchive a share.
///
/// `POST /share/{share_id}/unarchive/`
pub async fn unarchive_share(client: &ApiClient, share_id: &str) -> Result<Value, CliError> {
    let path = format!("/share/{}/unarchive/", urlencoding::encode(share_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// Authenticate to a password-protected share.
///
/// `POST /share/{share_id}/auth/password/`
pub async fn password_auth_share(
    client: &ApiClient,
    share_id: &str,
    password: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("password".to_owned(), password.to_owned());
    let path = format!("/share/{}/auth/password/", urlencoding::encode(share_id));
    client.post(&path, &form).await
}

/// Authenticate as a guest to a share.
///
/// `POST /share/{share_id}/auth/guest/`
pub async fn guest_auth(client: &ApiClient, share_id: &str) -> Result<Value, CliError> {
    let path = format!("/share/{}/auth/guest/", urlencoding::encode(share_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// Create a quickshare from a workspace file.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/quickshare/`
pub async fn create_quickshare(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    expires: Option<&str>,
    expires_at: Option<&str>,
) -> Result<Value, CliError> {
    let mut body = serde_json::Map::new();
    if let Some(e) = expires_at {
        body.insert("expires_at".to_owned(), Value::String(e.to_owned()));
    } else if let Some(e) = expires {
        body.insert("expires".to_owned(), Value::String(e.to_owned()));
    }
    let path = format!(
        "/workspace/{}/storage/{}/quickshare/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post_json(&path, &Value::Object(body)).await
}

/// List available shares for the current user.
///
/// `GET /shares/available/`
pub async fn available_shares(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/shares/available/").await
}

/// Check share name availability.
///
/// `GET /shares/check/name/{name}/`
pub async fn check_share_name(client: &ApiClient, name: &str) -> Result<Value, CliError> {
    let path = format!("/shares/check/name/{}/", urlencoding::encode(name));
    client.get(&path).await
}

/// Enable workflow on a share.
///
/// `POST /share/{share_id}/workflow/enable/`
pub async fn enable_share_workflow(client: &ApiClient, share_id: &str) -> Result<Value, CliError> {
    let path = format!("/share/{}/workflow/enable/", urlencoding::encode(share_id));
    client.post_json(&path, &serde_json::json!({})).await
}

/// Disable workflow on a share.
///
/// `POST /share/{share_id}/workflow/disable/`
pub async fn disable_share_workflow(client: &ApiClient, share_id: &str) -> Result<Value, CliError> {
    let path = format!("/share/{}/workflow/disable/", urlencoding::encode(share_id));
    client.post_json(&path, &serde_json::json!({})).await
}
