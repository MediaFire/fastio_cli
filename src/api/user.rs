#![allow(clippy::missing_errors_doc)]

/// User profile API endpoints for the Fast.io REST API.
///
/// Maps to the endpoints documented in `/current/user/`.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Get the current user's profile details.
///
/// `GET /user/me/details/`
pub async fn get_me(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/me/details/").await
}

/// Get a user's details by ID.
///
/// `GET /user/{user_id}/details/`
pub async fn get_user_by_id(client: &ApiClient, user_id: &str) -> Result<Value, CliError> {
    let path = format!("/user/{}/details/", urlencoding::encode(user_id));
    client.get(&path).await
}

/// Update the current user's profile.
///
/// `POST /user/update/` with form-encoded body.
pub async fn update_user(
    client: &ApiClient,
    first_name: Option<&str>,
    last_name: Option<&str>,
    email_address: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(v) = first_name {
        form.insert("first_name".to_owned(), v.to_owned());
    }
    if let Some(v) = last_name {
        form.insert("last_name".to_owned(), v.to_owned());
    }
    if let Some(v) = email_address {
        form.insert("email_address".to_owned(), v.to_owned());
    }
    client.post("/user/update/", &form).await
}

/// Upload a user asset (e.g. avatar).
///
/// `POST /user/{user_id}/assets/{asset_name}/`
///
/// Note: This endpoint requires multipart form data, which is not yet
/// supported by the basic client. Returns an error directing the user
/// to use the web interface for avatar uploads.
pub fn upload_user_asset(
    client: &ApiClient,
    user_id: &str,
    asset_name: &str,
    _file_path: &str,
) -> Result<Value, CliError> {
    let _ = client;
    let _ = user_id;
    let _ = asset_name;
    Err(CliError::Config(
        "Avatar upload is not available in this version of the CLI.".to_owned(),
    ))
}

/// Delete a user asset (e.g. avatar).
///
/// `DELETE /user/{user_id}/assets/{asset_name}/`
pub async fn delete_user_asset(
    client: &ApiClient,
    user_id: &str,
    asset_name: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/user/{}/assets/{}/",
        urlencoding::encode(user_id),
        urlencoding::encode(asset_name),
    );
    client.delete(&path).await
}

/// Get available user asset types.
///
/// `GET /user/assets/`
pub async fn get_asset_types(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/assets/").await
}

/// List a user's assets.
///
/// `GET /user/{user_id}/assets/`
pub async fn list_user_assets(client: &ApiClient, user_id: &str) -> Result<Value, CliError> {
    let path = format!("/user/{}/assets/", urlencoding::encode(user_id));
    client.get(&path).await
}

/// Get the current user's available profiles.
///
/// `GET /user/available_profiles/`
pub async fn get_profiles(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/available_profiles/").await
}

/// List user invitations.
///
/// `GET /user/invitations/list/`
pub async fn list_invitations(
    client: &ApiClient,
    invitation_key: Option<&str>,
) -> Result<Value, CliError> {
    if let Some(key) = invitation_key {
        let mut params = HashMap::new();
        params.insert("invitation_key".to_owned(), key.to_owned());
        client
            .get_with_params("/user/invitations/list/", &params)
            .await
    } else {
        client.get("/user/invitations/list/").await
    }
}

/// Get details of a specific invitation.
///
/// `GET /user/invitation/{invitation_id}/details/`
pub async fn get_invitation_details(
    client: &ApiClient,
    invitation_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/user/invitation/{}/details/",
        urlencoding::encode(invitation_id),
    );
    client.get(&path).await
}

/// Accept all pending invitations.
///
/// `POST /user/invitations/acceptall/`
pub async fn accept_all_invitations(
    client: &ApiClient,
    invitation_key: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(key) = invitation_key {
        form.insert("invitation_key".to_owned(), key.to_owned());
    }
    client.post("/user/invitations/acceptall/", &form).await
}

/// Search users by query.
///
/// `GET /users/search/`
pub async fn search_users(client: &ApiClient, query: &str) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("query".to_owned(), query.to_owned());
    client.get_with_params("/users/search/", &params).await
}

/// Close (delete) the current user's account.
///
/// `POST /user/close/`
pub async fn close_account(client: &ApiClient, confirmation: &str) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("confirmation".to_owned(), confirmation.to_owned());
    client.post("/user/close/", &form).await
}

/// Check whether the user is authorized in their country.
///
/// `GET /user/me/allowed/`
pub async fn user_allowed(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/me/allowed/").await
}

/// Check org creation eligibility / limits.
///
/// `GET /user/me/limits/orgs/`
pub async fn user_org_limits(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/me/limits/orgs/").await
}

/// List the user's shares.
///
/// `GET /shares/all/`
pub async fn list_user_shares(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/shares/all/").await
}
