#![allow(clippy::missing_errors_doc)]

/// Authentication API endpoints for the Fast.io REST API.
///
/// Maps to the endpoints documented in `/current/user/auth/`,
/// `/current/oauth/`, and `/current/user/2fa/`.
use std::collections::HashMap;

use base64::Engine;
use serde_json::Value;

use crate::api::types::{
    ApiKeyCreateResponse, ApiKeyListResponse, AuthCheckResponse, EmptyResponse,
    PkceAuthorizeResponse, PkceTokenResponse, SignInResponse, SignUpResponse,
    TwoFactorEnableResponse, TwoFactorStatusResponse, TwoFactorVerifyResponse,
};
use crate::client::ApiClient;
use crate::error::CliError;

/// Sign in with email and password via HTTP Basic Auth.
///
/// `GET /user/auth/` with `Authorization: Basic base64(email:password)`.
pub async fn sign_in(
    client: &ApiClient,
    email: &str,
    password: &str,
) -> Result<SignInResponse, CliError> {
    let credentials =
        base64::engine::general_purpose::STANDARD.encode(format!("{email}:{password}"));
    let auth_header = format!("Basic {credentials}");
    client.get_with_auth("/user/auth/", &auth_header).await
}

/// Create a new user account.
///
/// `POST /user/` with form-encoded body.
pub async fn sign_up(
    client: &ApiClient,
    email: &str,
    password: &str,
    first_name: Option<&str>,
    last_name: Option<&str>,
) -> Result<SignUpResponse, CliError> {
    let mut form = HashMap::new();
    form.insert("email_address".to_owned(), email.to_owned());
    form.insert("password".to_owned(), password.to_owned());
    form.insert("tos_agree".to_owned(), "true".to_owned());

    if let Some(first) = first_name {
        form.insert("first_name".to_owned(), first.to_owned());
    }
    if let Some(last) = last_name {
        form.insert("last_name".to_owned(), last.to_owned());
    }

    client.post_no_auth("/user/", &form).await
}

/// Check whether a token is valid.
///
/// `GET /user/auth/check/`
pub async fn check_token(client: &ApiClient) -> Result<AuthCheckResponse, CliError> {
    client.get("/user/auth/check/").await
}

/// Send or confirm email verification.
///
/// `POST /user/email/validate/`
pub async fn email_verify(
    client: &ApiClient,
    email: &str,
    code: Option<&str>,
) -> Result<EmptyResponse, CliError> {
    let mut form = HashMap::new();
    form.insert("email".to_owned(), email.to_owned());
    if let Some(c) = code {
        form.insert("email_token".to_owned(), c.to_owned());
    }
    client.post("/user/email/validate/", &form).await
}

/// Request a password reset email.
///
/// `POST /user/email/reset/`
#[allow(dead_code)]
pub async fn password_reset_request(
    client: &ApiClient,
    email: &str,
) -> Result<EmptyResponse, CliError> {
    let mut form = HashMap::new();
    form.insert("email".to_owned(), email.to_owned());
    client.post_no_auth("/user/email/reset/", &form).await
}

/// Initiate PKCE authorization.
///
/// `GET /oauth/authorize/` with query parameters.
pub async fn pkce_authorize(
    client: &ApiClient,
    client_id: &str,
    code_challenge: &str,
    state: &str,
    redirect_uri: &str,
) -> Result<PkceAuthorizeResponse, CliError> {
    let mut params = HashMap::new();
    params.insert("client_id".to_owned(), client_id.to_owned());
    params.insert("response_type".to_owned(), "code".to_owned());
    params.insert("code_challenge".to_owned(), code_challenge.to_owned());
    params.insert("code_challenge_method".to_owned(), "S256".to_owned());
    params.insert("state".to_owned(), state.to_owned());
    params.insert("redirect_uri".to_owned(), redirect_uri.to_owned());
    params.insert("response_format".to_owned(), "json".to_owned());

    client
        .get_no_auth_with_params("/oauth/authorize/", &params)
        .await
}

/// Exchange a PKCE authorization code for tokens.
///
/// `POST /oauth/token/` with form-encoded body.
pub async fn pkce_token_exchange(
    client: &ApiClient,
    code: &str,
    code_verifier: &str,
    client_id: &str,
    redirect_uri: &str,
) -> Result<PkceTokenResponse, CliError> {
    let mut form = HashMap::new();
    form.insert("grant_type".to_owned(), "authorization_code".to_owned());
    form.insert("code".to_owned(), code.to_owned());
    form.insert("code_verifier".to_owned(), code_verifier.to_owned());
    form.insert("client_id".to_owned(), client_id.to_owned());
    form.insert("redirect_uri".to_owned(), redirect_uri.to_owned());
    form.insert("device_name".to_owned(), "fastio-cli".to_owned());
    form.insert("device_type".to_owned(), "cli".to_owned());

    client.post_no_auth_raw("/oauth/token/", &form).await
}

/// Refresh an OAuth access token using a refresh token.
///
/// `POST /oauth/token/` with `grant_type=refresh_token`.
#[allow(dead_code)]
pub async fn pkce_refresh(
    client: &ApiClient,
    refresh_token: &str,
    client_id: &str,
) -> Result<PkceTokenResponse, CliError> {
    let mut form = HashMap::new();
    form.insert("grant_type".to_owned(), "refresh_token".to_owned());
    form.insert("refresh_token".to_owned(), refresh_token.to_owned());
    form.insert("client_id".to_owned(), client_id.to_owned());

    client.post_no_auth_raw("/oauth/token/", &form).await
}

/// Verify a 2FA code after sign-in.
///
/// `POST /user/auth/2factor/auth/{code}/`
pub async fn two_factor_verify(
    client: &ApiClient,
    code: &str,
) -> Result<TwoFactorVerifyResponse, CliError> {
    let path = format!("/user/auth/2factor/auth/{}/", urlencoding::encode(code));
    let form = HashMap::new();
    client.post(&path, &form).await
}

/// Get 2FA status for the current user.
///
/// `GET /user/auth/2factor/`
#[allow(dead_code)]
pub async fn two_factor_status(client: &ApiClient) -> Result<TwoFactorStatusResponse, CliError> {
    client.get("/user/auth/2factor/").await
}

/// Enable 2FA on a channel (sms, totp, whatsapp).
///
/// `POST /user/auth/2factor/{channel}/`
pub async fn two_factor_enable(
    client: &ApiClient,
    channel: &str,
) -> Result<TwoFactorEnableResponse, CliError> {
    let path = format!("/user/auth/2factor/{}/", urlencoding::encode(channel));
    let form = HashMap::new();
    client.post(&path, &form).await
}

/// Disable 2FA using a verification token.
///
/// `DELETE /user/auth/2factor/{token}/`
pub async fn two_factor_disable(
    client: &ApiClient,
    token: &str,
) -> Result<EmptyResponse, CliError> {
    let path = format!("/user/auth/2factor/{}/", urlencoding::encode(token));
    client.delete(&path).await
}

/// Create an API key.
///
/// `POST /user/auth/key/`
pub async fn api_key_create(
    client: &ApiClient,
    name: Option<&str>,
    scopes: Option<&str>,
    agent_name: Option<&str>,
) -> Result<ApiKeyCreateResponse, CliError> {
    let mut form = HashMap::new();
    if let Some(n) = name {
        form.insert("memo".to_owned(), n.to_owned());
    }
    if let Some(s) = scopes {
        form.insert("scopes".to_owned(), s.to_owned());
    }
    if let Some(a) = agent_name {
        form.insert("agent_name".to_owned(), a.to_owned());
    }
    client.post("/user/auth/key/", &form).await
}

/// List all API keys for the current user.
///
/// `GET /user/auth/keys/`
pub async fn api_key_list(client: &ApiClient) -> Result<ApiKeyListResponse, CliError> {
    client.get("/user/auth/keys/").await
}

/// Delete an API key by ID.
///
/// `DELETE /user/auth/key/{key_id}/`
pub async fn api_key_delete(client: &ApiClient, key_id: &str) -> Result<EmptyResponse, CliError> {
    let path = format!("/user/auth/key/{}/", urlencoding::encode(key_id));
    client.delete(&path).await
}

/// Get details of a specific API key.
///
/// `GET /user/auth/key/{key_id}/`
pub async fn api_key_get(client: &ApiClient, key_id: &str) -> Result<Value, CliError> {
    let path = format!("/user/auth/key/{}/", urlencoding::encode(key_id));
    client.get(&path).await
}

/// Update an API key.
///
/// `POST /user/auth/key/{key_id}/`
pub async fn api_key_update(
    client: &ApiClient,
    key_id: &str,
    name: Option<&str>,
    scopes: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(n) = name {
        form.insert("memo".to_owned(), n.to_owned());
    }
    if let Some(s) = scopes {
        form.insert("scopes".to_owned(), s.to_owned());
    }
    let path = format!("/user/auth/key/{}/", urlencoding::encode(key_id));
    client.post(&path, &form).await
}

/// Check email availability.
///
/// `POST /user/email/` (unauthenticated)
pub async fn email_check(client: &ApiClient, email: &str) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("email".to_owned(), email.to_owned());
    client.post_no_auth("/user/email/", &form).await
}

/// Complete a password reset using a code.
///
/// `POST /user/password/{code}/`
pub async fn password_reset_complete(
    client: &ApiClient,
    code: &str,
    password1: &str,
    password2: &str,
) -> Result<EmptyResponse, CliError> {
    let mut form = HashMap::new();
    form.insert("password1".to_owned(), password1.to_owned());
    form.insert("password2".to_owned(), password2.to_owned());
    let path = format!("/user/password/{}/", urlencoding::encode(code));
    client.post_no_auth(&path, &form).await
}

/// Send a 2FA code on a channel (sms, call, whatsapp).
///
/// `GET /user/auth/2factor/send/{channel}/`
pub async fn two_factor_send(client: &ApiClient, channel: &str) -> Result<Value, CliError> {
    let path = format!("/user/auth/2factor/send/{}/", urlencoding::encode(channel),);
    client.get(&path).await
}

/// Verify TOTP setup with a token.
///
/// `POST /user/auth/2factor/verify/{token}/`
pub async fn two_factor_verify_setup(client: &ApiClient, token: &str) -> Result<Value, CliError> {
    let path = format!("/user/auth/2factor/verify/{}/", urlencoding::encode(token),);
    let form = HashMap::new();
    client.post(&path, &form).await
}

/// List OAuth sessions.
///
/// `GET /oauth/sessions/`
pub async fn oauth_list(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/oauth/sessions/").await
}

/// Get OAuth session details.
///
/// `GET /oauth/sessions/{session_id}/`
pub async fn oauth_details(client: &ApiClient, session_id: &str) -> Result<Value, CliError> {
    let path = format!("/oauth/sessions/{}/", urlencoding::encode(session_id));
    client.get(&path).await
}

/// Revoke a single OAuth session.
///
/// `DELETE /oauth/sessions/{session_id}/`
pub async fn oauth_revoke(client: &ApiClient, session_id: &str) -> Result<Value, CliError> {
    let path = format!("/oauth/sessions/{}/", urlencoding::encode(session_id));
    client.delete(&path).await
}

/// Revoke all OAuth sessions.
///
/// `DELETE /oauth/sessions/`
pub async fn oauth_revoke_all(client: &ApiClient) -> Result<Value, CliError> {
    client.delete("/oauth/sessions/").await
}

/// Get the current session info (alias for user details).
///
/// `GET /user/me/details/`
pub async fn session_info(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/me/details/").await
}
