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

/// The wire name of the notification-preference field, which DIFFERS by entity.
///
/// Same concept, two spellings:
/// - workspace routes take **`notifications`**
/// - share routes take **`notify_options`**
///
/// `build_member_update_form` is shared between both, so it must select the
/// spelling per entity. Sending the wrong one makes
/// `fastio workspace member update --notify-options …` carry a field the
/// workspace route does not declare; the platform drops undeclared fields
/// silently, so the call returns 200 and changes nothing.
fn notify_field_for(entity_type: &str) -> &'static str {
    // Default to the share spelling for an unknown entity: that is the historical
    // behaviour, so an unrecognised value cannot make things worse than before.
    if entity_type == "workspace" {
        "notifications"
    } else {
        "notify_options"
    }
}

/// Build the form body for [`update_member`] (pure; unit-tested).
fn build_member_update_form(
    entity_type: &str,
    params: &UpdateMemberParams<'_>,
) -> HashMap<String, String> {
    let mut form = HashMap::new();
    if let Some(v) = params.permissions {
        form.insert("permissions".to_owned(), v.to_owned());
    }
    if let Some(v) = params.notify_options {
        form.insert(notify_field_for(entity_type).to_owned(), v.to_owned());
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
    let form = build_member_update_form(entity_type, params);
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

/// Path for accepting or declining a keyed invitation.
///
/// Extracted and tested because this literal must take `entity_type` like every
/// other builder in this file: hardcoding `/workspace/` sends a *share*
/// invitation down the workspace route with a share id in the workspace slot.
/// The published API docs document the route family per entity.
fn join_invitation_path(
    entity_type: &str,
    entity_id: &str,
    invitation_key: &str,
    invitation_action: &str,
) -> String {
    format!(
        "/{}/{}/members/join/{}/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(invitation_key),
        urlencoding::encode(invitation_action),
    )
}

/// Accept or decline a keyed workspace **or share** invitation.
///
/// `POST /{entity_type}/{entity_id}/members/join/{key}/{action}/`
///
/// This is the **invitation-key** route — the one an emailed invite link
/// carries. It is distinct from `fastio invitation accept|decline`, which works
/// off an invitation *id* on the `/user/invitations/` surface.
pub async fn join_invitation(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    invitation_key: &str,
    invitation_action: &str,
) -> Result<Value, CliError> {
    let path = join_invitation_path(entity_type, entity_id, invitation_key, invitation_action);
    client.post_json(&path, &serde_json::json!({})).await
}

#[cfg(test)]
mod tests {
    use super::{
        UpdateMemberParams, build_member_update_form, join_invitation_path, member_update_path,
    };

    /// Asserts the FULL literal, per entity type. A `contains("members/join")`
    /// check also passes on a `/workspace/`-hardcoded path — the route looks
    /// right in every substring — so only the full literal catches the mistake.
    #[test]
    fn join_invitation_path_honours_the_entity_type() {
        assert_eq!(
            join_invitation_path("workspace", "123", "abc", "accept"),
            "/workspace/123/members/join/abc/accept/"
        );
        assert_eq!(
            join_invitation_path("share", "456", "def", "decline"),
            "/share/456/members/join/def/decline/",
            "a SHARE invitation must not be sent down the workspace route"
        );
        // Every segment is percent-encoded so an id cannot break out of its slot.
        let p = join_invitation_path("share", "a/b", "c d", "accept");
        assert!(p.contains("a%2Fb") && p.contains("c%20d"), "{p}");
    }

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
        let form = build_member_update_form("share", &UpdateMemberParams::default());
        assert!(form.is_empty());
    }

    #[test]
    fn update_form_carries_permissions_notify_expires() {
        let form = build_member_update_form(
            "share",
            &UpdateMemberParams {
                permissions: Some("view"),
                notify_options: Some("Notify me in app"),
                expires: Some("2030-01-01 00:00:00"),
            },
        );
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

    /// The notification field is spelled DIFFERENTLY per entity, and the
    /// builder is shared between both.
    ///
    /// workspace → `notifications`; share → `notify_options`.
    ///
    /// The platform drops undeclared fields silently, so a workspace member
    /// update carrying the share spelling returns 200 and changes nothing.
    /// Asserting the wrong key is
    /// ABSENT is the load-bearing half — a test that only checks the right key
    /// is present would pass while both were sent.
    #[test]
    fn notification_field_is_spelled_per_entity_type() {
        let params = UpdateMemberParams {
            notify_options: Some("Notify me in app"),
            ..Default::default()
        };

        let ws = build_member_update_form("workspace", &params);
        assert_eq!(
            ws.get("notifications").map(String::as_str),
            Some("Notify me in app"),
            "a workspace takes `notifications`"
        );
        assert!(
            !ws.contains_key("notify_options"),
            "the share spelling must NOT be sent to a workspace route: {ws:?}"
        );

        let sh = build_member_update_form("share", &params);
        assert_eq!(
            sh.get("notify_options").map(String::as_str),
            Some("Notify me in app"),
            "a share takes `notify_options`"
        );
        assert!(
            !sh.contains_key("notifications"),
            "the workspace spelling must NOT be sent to a share route: {sh:?}"
        );

        // An unknown entity keeps the historical spelling rather than guessing.
        let other = build_member_update_form("fileshare", &params);
        assert!(other.contains_key("notify_options"));
    }

    #[test]
    fn update_form_only_permissions_when_role_only() {
        // The `update_member_role` wrapper sends ONLY permissions.
        let form = build_member_update_form(
            "share",
            &UpdateMemberParams {
                permissions: Some("admin"),
                ..Default::default()
            },
        );
        assert_eq!(form.len(), 1);
        assert_eq!(form.get("permissions").map(String::as_str), Some("admin"));
    }
}
