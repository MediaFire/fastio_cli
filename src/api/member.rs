#![allow(clippy::missing_errors_doc)]

/// Member management API endpoints for the Fast.io REST API.
///
/// Handles member operations for both workspaces and shares.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List members of a workspace or share.
///
/// `GET /{entity_type}/{entity_id}/members/list/`
pub async fn list_members(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
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
    let path = format!(
        "/{}/{}/members/list/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add a member to a workspace or share.
///
/// `POST /{entity_type}/{entity_id}/members/{email_or_user_id}/`
pub async fn add_member(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    email_or_user_id: &str,
    role: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "permissions".to_owned(),
        role.unwrap_or("member").to_owned(),
    );
    let path = format!(
        "/{}/{}/members/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(email_or_user_id),
    );
    client.post(&path, &form).await
}

/// Remove a member from a workspace or share.
///
/// `DELETE /{entity_type}/{entity_id}/members/{member_id}/`
pub async fn remove_member(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    member_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/members/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(member_id),
    );
    client.delete(&path).await
}

/// Parameters for updating a member (workspace or share).
///
/// All fields are optional — only provided fields are sent. `permissions`
/// cannot be set to `owner` (use transfer ownership). For shares, `permissions`
/// also accepts `view`.
#[derive(Default)]
pub struct UpdateMemberParams<'a> {
    /// New permission level.
    pub permissions: Option<&'a str>,
    /// Notification preference.
    pub notify_options: Option<&'a str>,
    /// Membership expiration `YYYY-MM-DD HH:MM:SS`; `null`/`""` to clear.
    pub expires: Option<&'a str>,
}

/// Build the form body for [`update_member`] (pure; unit-tested).
fn build_member_update_form(params: &UpdateMemberParams<'_>) -> HashMap<String, String> {
    let mut form = HashMap::new();
    if let Some(v) = params.permissions {
        form.insert("permissions".to_owned(), v.to_owned());
    }
    if let Some(v) = params.notify_options {
        form.insert("notify_options".to_owned(), v.to_owned());
    }
    if let Some(v) = params.expires {
        form.insert("expires".to_owned(), v.to_owned());
    }
    form
}

/// Path for the member-update endpoint (pure; unit-tested).
fn member_update_path(entity_type: &str, entity_id: &str, member_id: &str) -> String {
    format!(
        "/{}/{}/member/{}/update/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(member_id),
    )
}

/// Update a member's permissions, notification preference, and/or expiration.
///
/// `POST /{entity_type}/{entity_id}/member/{member_id}/update/`
pub async fn update_member(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    member_id: &str,
    params: &UpdateMemberParams<'_>,
) -> Result<Value, CliError> {
    let form = build_member_update_form(params);
    let path = member_update_path(entity_type, entity_id, member_id);
    client.post(&path, &form).await
}

/// Update only a member's role (thin wrapper over [`update_member`]).
///
/// `POST /{entity_type}/{entity_id}/member/{member_id}/update/`
pub async fn update_member_role(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    member_id: &str,
    role: &str,
) -> Result<Value, CliError> {
    update_member(
        client,
        entity_type,
        entity_id,
        member_id,
        &UpdateMemberParams {
            permissions: Some(role),
            ..Default::default()
        },
    )
    .await
}

/// Get member details.
///
/// `GET /{entity_type}/{entity_id}/member/{member_id}/details/`
pub async fn get_member_details(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    member_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/member/{}/details/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(member_id),
    );
    client.get(&path).await
}

/// Transfer ownership of a workspace or share.
///
/// `POST /{entity_type}/{entity_id}/member/{member_id}/transfer_ownership/` —
/// POST is the canonical (mutating) verb; the body is empty and the target
/// member is a URL path part. (The server still accepts GET for backward
/// compatibility, but the CLI uses the canonical POST.)
pub async fn transfer_ownership(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    member_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/member/{}/transfer_ownership/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(member_id),
    );
    client.post(&path, &HashMap::new()).await
}

/// Leave a workspace or share.
///
/// `DELETE /{entity_type}/{entity_id}/member/`
pub async fn leave(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/member/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    client.delete(&path).await
}

/// Join a workspace or share.
///
/// `POST /{entity_type}/{entity_id}/members/join/`
pub async fn join(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/members/join/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

/// Accept or decline a workspace invitation.
///
/// `POST /workspace/{entity_id}/members/join/{key}/{action}/`
pub async fn join_invitation(
    client: &ApiClient,
    entity_id: &str,
    invitation_key: &str,
    invitation_action: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/members/join/{}/{}/",
        urlencoding::encode(entity_id),
        urlencoding::encode(invitation_key),
        urlencoding::encode(invitation_action),
    );
    client.post_json(&path, &serde_json::json!({})).await
}

#[cfg(test)]
mod tests {
    use super::{UpdateMemberParams, build_member_update_form, member_update_path};

    #[test]
    fn update_path_targets_member_update_and_url_encodes() {
        assert_eq!(
            member_update_path("share", "123", "456"),
            "/share/123/member/456/update/"
        );
        // Path segments must be percent-encoded so an id can't break out.
        let p = member_update_path("share", "a/b", "c d");
        assert!(p.contains("a%2Fb"), "{p}");
        assert!(p.contains("c%20d"), "{p}");
    }

    #[test]
    fn update_form_empty_when_no_fields() {
        let form = build_member_update_form(&UpdateMemberParams::default());
        assert!(form.is_empty());
    }

    #[test]
    fn update_form_carries_permissions_notify_expires() {
        let form = build_member_update_form(&UpdateMemberParams {
            permissions: Some("view"),
            notify_options: Some("Notify me in app"),
            expires: Some("2030-01-01 00:00:00"),
        });
        assert_eq!(form.get("permissions").map(String::as_str), Some("view"));
        assert_eq!(
            form.get("notify_options").map(String::as_str),
            Some("Notify me in app")
        );
        assert_eq!(
            form.get("expires").map(String::as_str),
            Some("2030-01-01 00:00:00")
        );
    }

    #[test]
    fn update_form_only_permissions_when_role_only() {
        // The `update_member_role` wrapper sends ONLY permissions.
        let form = build_member_update_form(&UpdateMemberParams {
            permissions: Some("admin"),
            ..Default::default()
        });
        assert_eq!(form.len(), 1);
        assert_eq!(form.get("permissions").map(String::as_str), Some("admin"));
    }
}
