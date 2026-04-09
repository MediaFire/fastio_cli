#![allow(clippy::missing_errors_doc)]

/// User profile API endpoints for the Fast.io REST API.
///
/// Maps to the endpoints documented in `/current/user/`.
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, USER_AGENT};
use reqwest::multipart;
use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// User-Agent string for user asset requests.
const USER_ASSET_USER_AGENT: &str = concat!("fastio-cli/", env!("CARGO_PKG_VERSION"));

/// Timeout for user asset upload requests.
const ASSET_UPLOAD_TIMEOUT_SECS: u64 = 120;

/// Connect timeout for user asset requests.
const ASSET_CONNECT_TIMEOUT_SECS: u64 = 30;

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

/// Upload a user asset (e.g. avatar) via multipart form data.
///
/// `POST /user/{user_id}/assets/{asset_name}/`
///
/// This uses a raw reqwest client because the API expects `multipart/form-data`
/// with a binary file field.
pub async fn upload_user_asset(
    client: &ApiClient,
    user_id: &str,
    asset_name: &str,
    file_path: &str,
) -> Result<Value, CliError> {
    let token = client
        .get_token()
        .ok_or_else(|| CliError::Config("authentication required for asset upload".to_owned()))?
        .to_owned();
    let base = client.base_url();
    let url = format!(
        "{}/user/{}/assets/{}/",
        base.trim_end_matches('/'),
        urlencoding::encode(user_id),
        urlencoding::encode(asset_name),
    );

    let path = Path::new(file_path);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("asset")
        .to_owned();
    let file_bytes = tokio::fs::read(path)
        .await
        .map_err(|e| CliError::Config(format!("failed to read file {file_path}: {e}")))?;

    let part = multipart::Part::bytes(file_bytes).file_name(file_name);
    let form = multipart::Form::new().part("file", part);

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(ASSET_UPLOAD_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(ASSET_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    let resp = http_client
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(USER_AGENT, USER_ASSET_USER_AGENT)
        .multipart(form)
        .send()
        .await?;

    let status = resp.status();
    let body: Value = resp.json().await.map_err(CliError::Http)?;

    if !status.is_success() {
        let msg = body
            .get("error")
            .and_then(|e| e.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("asset upload failed");
        return Err(CliError::Api(crate::error::ApiError {
            code: 0,
            error_code: None,
            message: msg.to_owned(),
            http_status: status.as_u16(),
        }));
    }

    // Unwrap the API response envelope per convention.
    let result = body.get("result").and_then(|v| v.as_str());
    if result == Some("yes") {
        if let Some(inner) = body.get("response") {
            Ok(inner.clone())
        } else {
            Ok(body)
        }
    } else {
        let msg = body
            .get("error")
            .and_then(|e| e.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("asset upload failed");
        Err(CliError::Api(crate::error::ApiError {
            code: 0,
            error_code: None,
            message: msg.to_owned(),
            http_status: status.as_u16(),
        }))
    }
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

/// Enable or disable photo auto-sync from SSO providers.
///
/// `GET /user/me/autosync/{state}/`
///
/// State must be `"enable"` or `"disable"`.
pub async fn autosync(client: &ApiClient, state: &str) -> Result<Value, CliError> {
    let path = format!("/user/me/autosync/{}/", urlencoding::encode(state));
    client.get(&path).await
}

/// Read the binary content of a user asset.
///
/// `GET /user/{user_id}/assets/{asset_name}/read/`
///
/// Downloads the asset to the given output path. Returns the number of bytes written.
pub async fn read_user_asset(
    client: &ApiClient,
    user_id: &str,
    asset_name: &str,
    output_path: &Path,
) -> Result<u64, CliError> {
    let token = client
        .get_token()
        .ok_or_else(|| CliError::Config("authentication required for asset read".to_owned()))?
        .to_owned();
    let base = client.base_url();
    let url = format!(
        "{}/user/{}/assets/{}/read/",
        base.trim_end_matches('/'),
        urlencoding::encode(user_id),
        urlencoding::encode(asset_name),
    );

    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(ASSET_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(CliError::Http)?;

    let resp = http_client
        .get(&url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(USER_AGENT, USER_ASSET_USER_AGENT)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(CliError::Api(crate::error::ApiError {
            code: 0,
            error_code: None,
            message: format!("asset read failed with HTTP {status}: {body}"),
            http_status: status,
        }));
    }

    let bytes = resp.bytes().await.map_err(CliError::Http)?;
    tokio::fs::write(output_path, &bytes)
        .await
        .map_err(|e| CliError::Config(format!("failed to write file: {e}")))?;

    Ok(bytes.len() as u64)
}

/// Get the user's support PIN and identity verification hash.
///
/// `GET /user/pin/`
pub async fn get_pin(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/pin/").await
}

/// Validate a phone number and country code combination.
///
/// `GET /user/phone/{country_code}-{phone_number}/`
pub async fn validate_phone(
    client: &ApiClient,
    country_code: &str,
    phone_number: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/user/phone/{}-{}/",
        urlencoding::encode(country_code),
        urlencoding::encode(phone_number),
    );
    client.get(&path).await
}
