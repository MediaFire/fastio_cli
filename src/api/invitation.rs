#![allow(clippy::missing_errors_doc)]

/// Invitation management API endpoints for the Fast.io REST API.
///
/// Handles invitation operations for workspaces and shares.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List invitations for an entity (workspace or share), optionally by state.
///
/// `GET /{entity_type}/{entity_id}/members/invitations/list/[{state}/]`
pub async fn list_invitations(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    state: Option<&str>,
) -> Result<Value, CliError> {
    let path = if let Some(s) = state {
        format!(
            "/{}/{}/members/invitations/list/{}/",
            urlencoding::encode(entity_type),
            urlencoding::encode(entity_id),
            urlencoding::encode(s),
        )
    } else {
        format!(
            "/{}/{}/members/invitations/list/",
            urlencoding::encode(entity_type),
            urlencoding::encode(entity_id),
        )
    };
    client.get(&path).await
}

/// Parameters for updating an invitation (workspace or share).
///
/// All fields are optional — only provided fields are sent.
#[derive(Default)]
pub struct UpdateInvitationParams<'a> {
    /// New invitation state: `pending`, `accepted`, `declined`.
    pub new_state: Option<&'a str>,
    /// Updated permission level: `admin`, `member`, `guest`, `view`.
    pub permissions: Option<&'a str>,
    /// Updated notification preference.
    pub notify_options: Option<&'a str>,
    /// Updated membership expiration datetime.
    pub expires: Option<&'a str>,
}

/// Build the form body for [`update_invitation`] (pure; unit-tested).
fn build_update_invitation_form(params: &UpdateInvitationParams<'_>) -> HashMap<String, String> {
    let mut form = HashMap::new();
    if let Some(v) = params.new_state {
        form.insert("state".to_owned(), v.to_owned());
    }
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

/// Update an invitation.
///
/// `POST /{entity_type}/{entity_id}/members/invitation/{invitation_id}/`
pub async fn update_invitation(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    invitation_id: &str,
    params: &UpdateInvitationParams<'_>,
) -> Result<Value, CliError> {
    let form = build_update_invitation_form(params);
    let path = format!(
        "/{}/{}/members/invitation/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(invitation_id),
    );
    client.post(&path, &form).await
}

/// Delete an invitation.
///
/// `DELETE /{entity_type}/{entity_id}/members/invitation/{invitation_id}/`
pub async fn delete_invitation(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    invitation_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/members/invitation/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(invitation_id),
    );
    client.delete(&path).await
}

/// List user-level invitations (pending invitations for the current user).
///
/// `GET /user/invitations/list/`
pub async fn list_user_invitations(
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
        client.get("/user/invitations/list/").await
    } else {
        client
            .get_with_params("/user/invitations/list/", &params)
            .await
    }
}

/// Accept all pending user invitations.
///
/// `POST /user/invitations/acceptall/`
pub async fn accept_all_user_invitations(client: &ApiClient) -> Result<Value, CliError> {
    let form = HashMap::new();
    client.post("/user/invitations/acceptall/", &form).await
}

#[cfg(test)]
mod tests {
    use super::{UpdateInvitationParams, build_update_invitation_form};

    #[test]
    fn update_invitation_form_empty_when_no_fields() {
        let form = build_update_invitation_form(&UpdateInvitationParams::default());
        assert!(form.is_empty());
    }

    #[test]
    fn update_invitation_form_maps_new_state_to_state_key() {
        // The request field is `state`, not `new_state`.
        let form = build_update_invitation_form(&UpdateInvitationParams {
            new_state: Some("declined"),
            ..Default::default()
        });
        assert_eq!(form.get("state").map(String::as_str), Some("declined"));
        assert!(!form.contains_key("new_state"));
    }

    #[test]
    fn update_invitation_form_carries_all_fields() {
        let form = build_update_invitation_form(&UpdateInvitationParams {
            new_state: Some("pending"),
            permissions: Some("view"),
            notify_options: Some("Notify me in app"),
            expires: Some("2030-01-01 00:00:00"),
        });
        assert_eq!(form.get("state").map(String::as_str), Some("pending"));
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
}
