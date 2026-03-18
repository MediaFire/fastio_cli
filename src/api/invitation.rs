#![allow(clippy::missing_errors_doc)]

/// Invitation management API endpoints for the Fast.io REST API.
///
/// Handles invitation operations for workspaces and shares.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List invitations for an entity (workspace or share).
///
/// `GET /{entity_type}/{entity_id}/members/invitations/list/`
#[allow(dead_code)]
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

/// Update an invitation.
///
/// `POST /{entity_type}/{entity_id}/members/invitation/{invitation_id}/`
pub async fn update_invitation(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    invitation_id: &str,
    new_state: Option<&str>,
    permissions: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(v) = new_state {
        form.insert("state".to_owned(), v.to_owned());
    }
    if let Some(v) = permissions {
        form.insert("permissions".to_owned(), v.to_owned());
    }
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
