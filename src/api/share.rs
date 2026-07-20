#![allow(clippy::missing_errors_doc)]

/// Share management API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for share CRUD, storage operations, and member listing.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Insert an optional string form field when present.
fn put_str(form: &mut HashMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        form.insert(key.to_owned(), v.to_owned());
    }
}

/// Insert an optional boolean form field (serialized as `"true"`/`"false"`).
fn put_bool(form: &mut HashMap<String, String>, key: &str, value: Option<bool>) {
    if let Some(v) = value {
        form.insert(key.to_owned(), v.to_string());
    }
}

/// Insert an optional integer form field.
fn put_i64(form: &mut HashMap<String, String>, key: &str, value: Option<i64>) {
    if let Some(v) = value {
        form.insert(key.to_owned(), v.to_string());
    }
}

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
///
/// Mirrors the documented `POST /workspace/{id}/create/share/` body
/// (shares.txt "Create Share"). The server **requires** `intelligence`, so it
/// is a plain `bool` (default `false`) rather than an option; the server
/// defaults `share_type` to `exchange` when it is omitted.
pub struct CreateShareParams<'a> {
    /// Workspace ID to create the share in.
    pub workspace_id: &'a str,
    /// Share display title (2-80 chars).
    pub title: &'a str,
    /// Share direction type: `send`, `receive`, `exchange` (server default
    /// `exchange` when omitted).
    pub share_type: Option<&'a str>,
    /// Description (10-500 chars).
    pub description: Option<&'a str>,
    /// Access options (see shares.txt "Access Options").
    pub access_options: Option<&'a str>,
    /// Who can manage share invitations: `owners`, `guests`.
    pub invite: Option<&'a str>,
    /// Storage mode: `independent` (default) or `workspace_folder`.
    pub storage_mode: Option<&'a str>,
    /// Backing workspace folder opaque ID (`workspace_folder` mode).
    pub folder_node_id: Option<&'a str>,
    /// Create a new backing folder (`workspace_folder` mode, with `folder_name`).
    pub create_folder: Option<bool>,
    /// Name for the new backing folder (with `create_folder`).
    pub folder_name: Option<&'a str>,
    /// URL-friendly custom name (auto-generated when omitted).
    pub custom_name: Option<&'a str>,
    /// Password for share access (Send + 'Anyone with the link' only).
    pub password: Option<&'a str>,
    /// Expiration datetime `YYYY-MM-DD HH:MM:SS` (portal mode only).
    pub expires: Option<&'a str>,
    /// Notification preference: `never`, `notify_on_file_received`,
    /// `notify_on_file_sent_or_received`.
    pub notify: Option<&'a str>,
    /// Enable comments.
    pub comments_enabled: Option<bool>,
    /// Download security level ("high", "medium", or "off").
    pub download_security: Option<&'a str>,
    /// Enable guest AI chat.
    pub guest_chat_enabled: Option<bool>,
    /// Visual display mode: `grid`, `list`.
    pub display_type: Option<&'a str>,
    /// Workspace visual style.
    pub workspace_style: Option<&'a str>,
    /// Enable anonymous file uploads (Receive/Exchange + public + premium).
    pub anonymous_uploads_enabled: Option<bool>,
    /// Enable AI indexing. **Required server-side** — always sent.
    pub intelligence: bool,
    /// Accent color (JSON color object).
    pub accent_color: Option<&'a str>,
    /// Primary background color (JSON color object).
    pub background_color1: Option<&'a str>,
    /// Secondary background color (JSON color object).
    pub background_color2: Option<&'a str>,
    /// Background image selection (numeric).
    pub background_image: Option<i64>,
    /// Custom link #1 (JSON link object).
    pub link_1: Option<&'a str>,
    /// Custom link #2 (JSON link object).
    pub link_2: Option<&'a str>,
    /// Custom link #3 (JSON link object).
    pub link_3: Option<&'a str>,
    /// Custom owner-defined properties (JSON or `"null"`).
    pub owner_defined: Option<&'a str>,
}

/// Build the form body for [`create_share`] (pure; unit-tested).
fn build_create_share_form(params: &CreateShareParams<'_>) -> HashMap<String, String> {
    let mut form = HashMap::new();
    form.insert("title".to_owned(), params.title.to_owned());
    // `intelligence` is `Assert\Required` server-side: always send it.
    form.insert("intelligence".to_owned(), params.intelligence.to_string());
    put_str(&mut form, "share_type", params.share_type);
    put_str(&mut form, "description", params.description);
    put_str(&mut form, "access_options", params.access_options);
    put_str(&mut form, "invite", params.invite);
    put_str(&mut form, "storage_mode", params.storage_mode);
    put_str(&mut form, "folder_node_id", params.folder_node_id);
    put_bool(&mut form, "create_folder", params.create_folder);
    put_str(&mut form, "folder_name", params.folder_name);
    put_str(&mut form, "custom_name", params.custom_name);
    put_str(&mut form, "password", params.password);
    put_str(&mut form, "expires", params.expires);
    put_str(&mut form, "notify", params.notify);
    put_bool(&mut form, "comments_enabled", params.comments_enabled);
    put_str(&mut form, "download_security", params.download_security);
    put_bool(&mut form, "guest_chat_enabled", params.guest_chat_enabled);
    put_str(&mut form, "display_type", params.display_type);
    put_str(&mut form, "workspace_style", params.workspace_style);
    put_bool(
        &mut form,
        "anonymous_uploads_enabled",
        params.anonymous_uploads_enabled,
    );
    put_str(&mut form, "accent_color", params.accent_color);
    put_str(&mut form, "background_color1", params.background_color1);
    put_str(&mut form, "background_color2", params.background_color2);
    put_i64(&mut form, "background_image", params.background_image);
    put_str(&mut form, "link_1", params.link_1);
    put_str(&mut form, "link_2", params.link_2);
    put_str(&mut form, "link_3", params.link_3);
    put_str(&mut form, "owner_defined", params.owner_defined);
    form
}

/// Create a new share on a workspace.
///
/// `POST /workspace/{workspace_id}/create/share/`
pub async fn create_share(
    client: &ApiClient,
    params: &CreateShareParams<'_>,
) -> Result<Value, CliError> {
    let form = build_create_share_form(params);
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
///
/// Mirrors the documented `POST /share/{id}/update/` body (shares.txt "Update
/// Share"). All fields are optional — only provided fields are modified. String
/// fields documented as clearable accept `"null"` (or `""`) to clear them.
pub struct UpdateShareParams<'a> {
    /// Share ID.
    pub share_id: &'a str,
    /// New share display name.
    pub name: Option<&'a str>,
    /// New display title (2-80 chars), or `"null"` to clear.
    pub title: Option<&'a str>,
    /// New URL-friendly custom name (10-100 chars), or `"null"` to clear.
    pub custom_name: Option<&'a str>,
    /// New description (10-500 chars), or `"null"`/`""` to clear.
    pub description: Option<&'a str>,
    /// Share direction type: `send`, `receive`, `exchange`.
    pub share_type: Option<&'a str>,
    /// New access options.
    pub access_options: Option<&'a str>,
    /// Who can manage invitations: `owners`, `guests`.
    pub invite: Option<&'a str>,
    /// Password (Send + 'Anyone with the link'); `"null"`/`""` to clear.
    pub password: Option<&'a str>,
    /// Expiration datetime (portal mode only), or `"null"` to clear.
    pub expires: Option<&'a str>,
    /// Notification preference.
    pub notify: Option<&'a str>,
    /// Enable/disable downloads (legacy — prefer `download_security`).
    pub download_enabled: Option<bool>,
    /// Enable/disable comments.
    pub comments_enabled: Option<bool>,
    /// Download security level ("high", "medium", or "off").
    pub download_security: Option<&'a str>,
    /// Visual display mode: `grid`, `list`.
    pub display_type: Option<&'a str>,
    /// Workspace visual style.
    pub workspace_style: Option<&'a str>,
    /// Enable/disable guest AI chat.
    pub guest_chat_enabled: Option<bool>,
    /// Toggle AI indexing. Enabling requires `content_ai` + `ai_agent`.
    pub intelligence: Option<bool>,
    /// Enable/disable anonymous uploads.
    pub anonymous_uploads_enabled: Option<bool>,
    /// Accent color (JSON color object), or `"null"`.
    pub accent_color: Option<&'a str>,
    /// Primary background color (JSON color object), or `"null"`.
    pub background_color1: Option<&'a str>,
    /// Secondary background color (JSON color object), or `"null"`.
    pub background_color2: Option<&'a str>,
    /// Background image selection (numeric).
    pub background_image: Option<i64>,
    /// Custom link #1 (JSON link object), or `"null"`.
    pub link_1: Option<&'a str>,
    /// Custom link #2 (JSON link object), or `"null"`.
    pub link_2: Option<&'a str>,
    /// Custom link #3 (JSON link object), or `"null"`.
    pub link_3: Option<&'a str>,
    /// Custom owner-defined properties (JSON or `"null"`).
    pub owner_defined: Option<&'a str>,
    /// Remove the workspace share-link node. Per shares.txt this accepts ONLY
    /// the literal string `"null"`; the value is passed through verbatim.
    pub share_link_node_id: Option<&'a str>,
}

/// Build the form body for [`update_share`] (pure; unit-tested).
fn build_update_share_form(params: &UpdateShareParams<'_>) -> HashMap<String, String> {
    let mut form = HashMap::new();
    put_str(&mut form, "name", params.name);
    put_str(&mut form, "title", params.title);
    put_str(&mut form, "custom_name", params.custom_name);
    put_str(&mut form, "description", params.description);
    put_str(&mut form, "share_type", params.share_type);
    put_str(&mut form, "access_options", params.access_options);
    put_str(&mut form, "invite", params.invite);
    put_str(&mut form, "password", params.password);
    put_str(&mut form, "expires", params.expires);
    put_str(&mut form, "notify", params.notify);
    put_bool(&mut form, "download_enabled", params.download_enabled);
    put_bool(&mut form, "comments_enabled", params.comments_enabled);
    put_str(&mut form, "download_security", params.download_security);
    put_str(&mut form, "display_type", params.display_type);
    put_str(&mut form, "workspace_style", params.workspace_style);
    put_bool(&mut form, "guest_chat_enabled", params.guest_chat_enabled);
    put_bool(&mut form, "intelligence", params.intelligence);
    put_bool(
        &mut form,
        "anonymous_uploads_enabled",
        params.anonymous_uploads_enabled,
    );
    put_str(&mut form, "accent_color", params.accent_color);
    put_str(&mut form, "background_color1", params.background_color1);
    put_str(&mut form, "background_color2", params.background_color2);
    put_i64(&mut form, "background_image", params.background_image);
    put_str(&mut form, "link_1", params.link_1);
    put_str(&mut form, "link_2", params.link_2);
    put_str(&mut form, "link_3", params.link_3);
    put_str(&mut form, "owner_defined", params.owner_defined);
    // `"null"` (the only documented value) is passed through verbatim to clear
    // the workspace share-link node — do NOT coerce it.
    put_str(&mut form, "share_link_node_id", params.share_link_node_id);
    form
}

/// Update share settings.
///
/// `POST /share/{share_id}/update/`
pub async fn update_share(
    client: &ApiClient,
    params: &UpdateShareParams<'_>,
) -> Result<Value, CliError> {
    let form = build_update_share_form(params);
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

/// Parameters for adding a member to (or inviting a member to) a share.
///
/// The endpoint `POST /share/{id}/members/{email_or_user_id}/` accepts a
/// 19-digit user ID (add an existing user) or an email (send an invitation).
/// `notify_options`/`expires`/`force_notification` apply when adding an existing
/// user; `message`/`invitation_expires` apply when inviting by email. Irrelevant
/// fields are ignored by the server.
pub struct AddShareMemberParams<'a> {
    /// Share ID.
    pub share_id: &'a str,
    /// Email address (invite) or 19-digit user ID (add existing user).
    pub email_or_user_id: &'a str,
    /// Permission level: `admin`, `member`, `guest`, `view` (default `member`).
    /// Cannot be `owner` (use transfer ownership).
    pub permissions: Option<&'a str>,
    /// Notification preference (existing-user add).
    pub notify_options: Option<&'a str>,
    /// Membership expiration `YYYY-MM-DD HH:MM:SS UTC`; `null`/`""` to clear.
    pub expires: Option<&'a str>,
    /// Resend notification email (60s cooldown after initial add).
    pub force_notification: Option<bool>,
    /// Custom message for the invitation email (email invite).
    pub message: Option<&'a str>,
    /// Invitation expiration datetime (email invite).
    pub invitation_expires: Option<&'a str>,
}

/// Build the form body for [`add_share_member`] (pure; unit-tested).
fn build_add_share_member_form(params: &AddShareMemberParams<'_>) -> HashMap<String, String> {
    let mut form = HashMap::new();
    // `permissions` defaults to `member` server-side; send it explicitly.
    form.insert(
        "permissions".to_owned(),
        params.permissions.unwrap_or("member").to_owned(),
    );
    put_str(&mut form, "notify_options", params.notify_options);
    put_str(&mut form, "expires", params.expires);
    put_bool(&mut form, "force_notification", params.force_notification);
    put_str(&mut form, "message", params.message);
    put_str(&mut form, "invitation_expires", params.invitation_expires);
    form
}

/// Add a member to (or invite a member to) a share.
///
/// `POST /share/{share_id}/members/{email_or_user_id}/`
pub async fn add_share_member(
    client: &ApiClient,
    params: &AddShareMemberParams<'_>,
) -> Result<Value, CliError> {
    let form = build_add_share_member_form(params);
    let path = format!(
        "/share/{}/members/{}/",
        urlencoding::encode(params.share_id),
        urlencoding::encode(params.email_or_user_id),
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

#[cfg(test)]
mod tests {
    use super::{
        AddShareMemberParams, CreateShareParams, UpdateShareParams, build_add_share_member_form,
        build_create_share_form, build_update_share_form,
    };

    /// Helper: a `CreateShareParams` with only the required fields set.
    fn minimal_create<'a>(workspace_id: &'a str, title: &'a str) -> CreateShareParams<'a> {
        CreateShareParams {
            workspace_id,
            title,
            share_type: None,
            description: None,
            access_options: None,
            invite: None,
            storage_mode: None,
            folder_node_id: None,
            create_folder: None,
            folder_name: None,
            custom_name: None,
            password: None,
            expires: None,
            notify: None,
            comments_enabled: None,
            download_security: None,
            guest_chat_enabled: None,
            display_type: None,
            workspace_style: None,
            anonymous_uploads_enabled: None,
            intelligence: false,
            accent_color: None,
            background_color1: None,
            background_color2: None,
            background_image: None,
            link_1: None,
            link_2: None,
            link_3: None,
            owner_defined: None,
        }
    }

    #[test]
    fn create_form_does_not_hardcode_share_type_send() {
        // Regression: the old builder hardcoded `share_type=send`, overriding
        // the server's `exchange` default. With no `--share-type`, the field
        // must be ABSENT so the server applies its documented default.
        let form = build_create_share_form(&minimal_create("ws", "My Share"));
        assert!(
            !form.contains_key("share_type"),
            "share_type must be omitted when not requested (server defaults to exchange)"
        );
        assert_eq!(form.get("title").map(String::as_str), Some("My Share"));
    }

    #[test]
    fn create_form_always_sends_intelligence_default_false() {
        // `intelligence` is Assert\Required server-side: it must ALWAYS be sent,
        // defaulting to false, or create returns a 400.
        let form = build_create_share_form(&minimal_create("ws", "t"));
        assert_eq!(form.get("intelligence").map(String::as_str), Some("false"));
    }

    #[test]
    fn create_form_sends_intelligence_true_when_set() {
        let mut p = minimal_create("ws", "t");
        p.intelligence = true;
        let form = build_create_share_form(&p);
        assert_eq!(form.get("intelligence").map(String::as_str), Some("true"));
    }

    #[test]
    fn create_form_carries_all_supplied_params() {
        let mut p = minimal_create("ws", "t");
        p.share_type = Some("exchange");
        p.storage_mode = Some("workspace_folder");
        p.folder_node_id = Some("node-abc");
        p.create_folder = Some(true);
        p.folder_name = Some("Shared");
        p.custom_name = Some("my-share");
        p.comments_enabled = Some(true);
        p.guest_chat_enabled = Some(false);
        p.notify = Some("notify_on_file_received");
        p.invite = Some("guests");
        p.expires = Some("2030-01-01 00:00:00");
        p.display_type = Some("grid");
        p.background_image = Some(7);
        let form = build_create_share_form(&p);
        assert_eq!(form.get("share_type").map(String::as_str), Some("exchange"));
        assert_eq!(
            form.get("storage_mode").map(String::as_str),
            Some("workspace_folder")
        );
        assert_eq!(
            form.get("folder_node_id").map(String::as_str),
            Some("node-abc")
        );
        assert_eq!(form.get("create_folder").map(String::as_str), Some("true"));
        assert_eq!(form.get("folder_name").map(String::as_str), Some("Shared"));
        assert_eq!(
            form.get("custom_name").map(String::as_str),
            Some("my-share")
        );
        assert_eq!(
            form.get("comments_enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            form.get("guest_chat_enabled").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            form.get("notify").map(String::as_str),
            Some("notify_on_file_received")
        );
        assert_eq!(form.get("invite").map(String::as_str), Some("guests"));
        assert_eq!(form.get("display_type").map(String::as_str), Some("grid"));
        assert_eq!(form.get("background_image").map(String::as_str), Some("7"));
    }

    #[test]
    fn update_form_only_includes_provided_fields() {
        let params = UpdateShareParams {
            share_id: "123",
            name: None,
            title: Some("New Title"),
            custom_name: None,
            description: None,
            share_type: Some("receive"),
            access_options: None,
            invite: None,
            password: None,
            expires: Some("null"),
            notify: None,
            download_enabled: None,
            comments_enabled: Some(true),
            download_security: Some("high"),
            display_type: None,
            workspace_style: None,
            guest_chat_enabled: None,
            intelligence: Some(false),
            anonymous_uploads_enabled: None,
            accent_color: None,
            background_color1: None,
            background_color2: None,
            background_image: None,
            link_1: None,
            link_2: None,
            link_3: None,
            owner_defined: None,
            share_link_node_id: None,
        };
        let form = build_update_share_form(&params);
        assert_eq!(form.get("title").map(String::as_str), Some("New Title"));
        assert_eq!(form.get("share_type").map(String::as_str), Some("receive"));
        assert_eq!(form.get("expires").map(String::as_str), Some("null"));
        assert_eq!(
            form.get("comments_enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            form.get("download_security").map(String::as_str),
            Some("high")
        );
        assert_eq!(form.get("intelligence").map(String::as_str), Some("false"));
        // Untouched fields must be absent (partial update).
        assert!(!form.contains_key("name"));
        assert!(!form.contains_key("password"));
        assert!(!form.contains_key("access_options"));
        // share_link_node_id omitted when not supplied.
        assert!(!form.contains_key("share_link_node_id"));
    }

    /// Helper: an `UpdateShareParams` for `share_id` with every field unset.
    fn minimal_update(share_id: &str) -> UpdateShareParams<'_> {
        UpdateShareParams {
            share_id,
            name: None,
            title: None,
            custom_name: None,
            description: None,
            share_type: None,
            access_options: None,
            invite: None,
            password: None,
            expires: None,
            notify: None,
            download_enabled: None,
            comments_enabled: None,
            download_security: None,
            display_type: None,
            workspace_style: None,
            guest_chat_enabled: None,
            intelligence: None,
            anonymous_uploads_enabled: None,
            accent_color: None,
            background_color1: None,
            background_color2: None,
            background_image: None,
            link_1: None,
            link_2: None,
            link_3: None,
            owner_defined: None,
            share_link_node_id: None,
        }
    }

    #[test]
    fn update_form_sends_share_link_node_id_null_verbatim() {
        // shares.txt: `share_link_node_id` accepts ONLY `"null"` to remove the
        // workspace share-link node — the literal must be passed through, not
        // coerced into an empty/absent field.
        let mut params = minimal_update("123");
        params.share_link_node_id = Some("null");
        let form = build_update_share_form(&params);
        assert_eq!(
            form.get("share_link_node_id").map(String::as_str),
            Some("null")
        );
    }

    #[test]
    fn add_member_form_defaults_permissions_to_member() {
        let form = build_add_share_member_form(&AddShareMemberParams {
            share_id: "123",
            email_or_user_id: "user@example.com",
            permissions: None,
            notify_options: None,
            expires: None,
            force_notification: None,
            message: None,
            invitation_expires: None,
        });
        assert_eq!(form.get("permissions").map(String::as_str), Some("member"));
    }

    #[test]
    fn add_member_form_carries_view_role_and_invite_params() {
        let form = build_add_share_member_form(&AddShareMemberParams {
            share_id: "123",
            email_or_user_id: "user@example.com",
            permissions: Some("view"),
            notify_options: Some("Notify me in app and via email"),
            expires: Some("2030-01-01 00:00:00 UTC"),
            force_notification: Some(true),
            message: Some("Join us"),
            invitation_expires: Some("2030-02-01 00:00:00 UTC"),
        });
        assert_eq!(form.get("permissions").map(String::as_str), Some("view"));
        assert_eq!(
            form.get("notify_options").map(String::as_str),
            Some("Notify me in app and via email")
        );
        assert_eq!(
            form.get("force_notification").map(String::as_str),
            Some("true")
        );
        assert_eq!(form.get("message").map(String::as_str), Some("Join us"));
        assert_eq!(
            form.get("invitation_expires").map(String::as_str),
            Some("2030-02-01 00:00:00 UTC")
        );
    }
}
