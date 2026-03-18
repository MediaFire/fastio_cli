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

/// Update a member's role.
///
/// `POST /{entity_type}/{entity_id}/member/{member_id}/update/`
pub async fn update_member_role(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    member_id: &str,
    role: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("permissions".to_owned(), role.to_owned());
    let path = format!(
        "/{}/{}/member/{}/update/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
        urlencoding::encode(member_id),
    );
    client.post(&path, &form).await
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
/// `GET /{entity_type}/{entity_id}/member/{member_id}/transfer_ownership/`
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
    client.get(&path).await
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
