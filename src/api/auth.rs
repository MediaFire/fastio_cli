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
///
/// A GET that ACTS: it MINTS a JWT. Routed through
/// [`ApiClient::get_with_auth_side_effecting`] so a lost response is never
/// recovered by re-sending — a replay would mint a second live token (audit
/// noise, two valid sessions), and on a failed sign-in it would burn a second
/// attempt against lockout and rate-limit accounting.
pub async fn sign_in(
    client: &ApiClient,
    email: &str,
    password: &str,
) -> Result<SignInResponse, CliError> {
    let credentials =
        base64::engine::general_purpose::STANDARD.encode(format!("{email}:{password}"));
    let auth_header = format!("Basic {credentials}");
    client
        .get_with_auth_side_effecting("/user/auth/", &auth_header)
        .await
}

/// Create a new user account.
///
/// `POST /user/` with form-encoded body.
///
/// When `agent` is `true` the `agent` form field is sent as `"true"`, tagging
/// the account as an AI-agent account (`account_type` becomes `"agent"`
/// permanently per `auth.txt`). When `false` the field is omitted (default
/// non-agent account).
pub async fn sign_up(
    client: &ApiClient,
    email: &str,
    password: &str,
    first_name: Option<&str>,
    last_name: Option<&str>,
    agent: bool,
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
    if agent {
        form.insert("agent".to_owned(), "true".to_owned());
    }

    client.post_no_auth("/user/", &form).await
}

/// Check whether a token is valid.
///
/// `GET /user/auth/check/`
pub async fn check_token(client: &ApiClient) -> Result<AuthCheckResponse, CliError> {
    client.get("/user/auth/check/").await
}

/// Sign out — invalidate every revocable JWT issued to the calling user.
///
/// `POST /user/auth/sign-out/` (no body).
///
/// This is the server-side "logout" for interactive (revocable) browser
/// sessions; per `auth.txt` it does NOT affect API keys, OAuth/PKCE access
/// tokens, agent tokens, or non-revocable JWTs. For a strict superset that also
/// kills account-session tokens, use [`invalidate_all`].
pub async fn sign_out(client: &ApiClient) -> Result<EmptyResponse, CliError> {
    client.post_empty("/user/auth/sign-out/").await
}

/// Invalidate EVERY login session for the calling user ("sign out everywhere").
///
/// `POST /user/auth/invalidate-all/` (no body).
///
/// A strict superset of [`sign_out`]: per `auth.txt` it bumps both the user's
/// `session_version` (revocable browser tokens) and `global_session_version`
/// (all account-session tokens — interactive logins regardless of the revocable
/// opt-in, Ripley AI-chat tokens, and workflow-agent tokens). It does NOT affect
/// OAuth/PKCE access tokens or API keys, which have separate revocation paths.
pub async fn invalidate_all(client: &ApiClient) -> Result<EmptyResponse, CliError> {
    client.post_empty("/user/auth/invalidate-all/").await
}

/// Confirm a pending email-address change.
///
/// `POST /user/email/change/` with the one-time `token` from the confirmation
/// link emailed to the new address. The change is requested via
/// `POST /user/update/` (`email_address` + `current_password`) and only takes
/// effect once confirmed here (per `auth.txt`).
pub async fn email_change_confirm(
    client: &ApiClient,
    token: &str,
) -> Result<EmptyResponse, CliError> {
    let mut form = HashMap::new();
    form.insert("token".to_owned(), token.to_owned());
    client.post("/user/email/change/", &form).await
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
///
/// A GET that ACTS: it CREATES a pending authorization request (the returned
/// `auth_request_id`). Routed through the non-replaying path so a lost response
/// body is never recovered by re-sending — that would create a second one.
///
/// `agent_name` labels the *instance* behind this credential. [`PKCE_CLIENT_ID`]
/// is a fixed constant, so without it every agent signing in through the CLI is
/// indistinguishable from every other one — the OAuth identity is a vendor
/// label, not an instance. It is optional and omitted when absent: the server
/// echoes it back in the authorize response when supplied.
///
/// [`PKCE_CLIENT_ID`]: crate::auth::pkce::PKCE_CLIENT_ID
pub async fn pkce_authorize(
    client: &ApiClient,
    client_id: &str,
    code_challenge: &str,
    state: &str,
    redirect_uri: &str,
    agent_name: Option<&str>,
) -> Result<PkceAuthorizeResponse, CliError> {
    let mut params = HashMap::new();
    params.insert("client_id".to_owned(), client_id.to_owned());
    params.insert("response_type".to_owned(), "code".to_owned());
    params.insert("code_challenge".to_owned(), code_challenge.to_owned());
    params.insert("code_challenge_method".to_owned(), "S256".to_owned());
    params.insert("state".to_owned(), state.to_owned());
    params.insert("redirect_uri".to_owned(), redirect_uri.to_owned());
    params.insert("response_format".to_owned(), "json".to_owned());
    if let Some(agent_name) = agent_name.map(str::trim).filter(|n| !n.is_empty()) {
        params.insert("agent_name".to_owned(), agent_name.to_owned());
    }

    client
        .get_no_auth_with_params_side_effecting("/oauth/authorize/", &params)
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
///
/// `expires` is an optional expiration datetime — any `strtotime`-compatible
/// value, canonical form `Y-m-d H:i:s UTC` (e.g. `2026-12-31 23:59:59 UTC`),
/// and must be in the future (per `auth.txt`). Omit for a non-expiring key.
pub async fn api_key_create(
    client: &ApiClient,
    name: Option<&str>,
    scopes: Option<&str>,
    agent_name: Option<&str>,
    expires: Option<&str>,
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
    if let Some(e) = expires {
        form.insert("expires".to_owned(), e.to_owned());
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
///
/// `agent_name` and `expires` mirror [`api_key_create`]; an empty string for
/// `expires` clears an existing expiration (per `auth.txt`).
pub async fn api_key_update(
    client: &ApiClient,
    key_id: &str,
    name: Option<&str>,
    scopes: Option<&str>,
    agent_name: Option<&str>,
    expires: Option<&str>,
) -> Result<Value, CliError> {
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
    if let Some(e) = expires {
        form.insert("expires".to_owned(), e.to_owned());
    }
    let path = format!("/user/auth/key/{}/", urlencoding::encode(key_id));
    client.post(&path, &form).await
}

/// Revoke an OAuth refresh TOKEN (RFC 7009).
///
/// `POST /oauth/revoke/` — unauthenticated, form-encoded, single required
/// `token` field (see the published API docs).
///
/// NOT the same endpoint as [`oauth_revoke`], despite the near-identical
/// name: that one is `DELETE /oauth/sessions/{session_id}/` and revokes a
/// SESSION by id, authenticated. This one revokes a refresh TOKEN by value,
/// unauthenticated. Two contracts, two routes — do not merge them.
///
/// Sign-out does not touch OAuth tokens, and the published API docs direct
/// callers to "always call this endpoint on user logout" — the two halves of
/// one contract. Skipping this call leaves a live refresh token on the server;
/// the tokens are long-lived (the doc's own example expires ten years out), so
/// a "signed out" client would leave a decade-valid credential behind.
///
/// Per RFC 7009 the endpoint **always answers success** — found, already
/// revoked, or never existed are indistinguishable, deliberately, to prevent
/// token enumeration. So a caller learns nothing from the response and must
/// clear local storage regardless (see the published API docs).
pub async fn oauth_revoke_token(
    client: &ApiClient,
    refresh_token: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("token".to_owned(), refresh_token.to_owned());
    client.post_no_auth("/oauth/revoke/", &form).await
}

/// DEPRECATED — this is NOT an availability check.
///
/// `POST /user/email/` (unauthenticated, IP-throttled). Per the published API
/// docs it "no longer does any account lookup" and returns a uniform `202` /
/// `result: true` for ANY well-formed address, because reporting registration
/// let anyone enumerate accounts. A success tells the caller nothing. Retained
/// only so existing callers keep receiving a success response.
///
/// To handle an already-registered email, call signup (`POST /user/`) — it
/// notifies the existing account and returns the same success as a new signup.
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
///
/// A GET that ACTS: it delivers a message to the user's phone. Routed through
/// [`ApiClient::get_side_effecting`] so a lost response body is never recovered
/// by re-sending — that would send the user a SECOND code.
pub async fn two_factor_send(client: &ApiClient, channel: &str) -> Result<Value, CliError> {
    let path = format!("/user/auth/2factor/send/{}/", urlencoding::encode(channel),);
    client.get_side_effecting(&path).await
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

/// Rename an OAuth session's display labels.
///
/// `PATCH /oauth/sessions/{session_id}/` with a form-encoded body. Updates the
/// session's `device_name` (human-readable device label) and/or `agent_name`
/// (connection/agent label) per `oauth.txt`; an empty string clears a label to
/// null. At least one of the two should be provided.
pub async fn oauth_rename(
    client: &ApiClient,
    session_id: &str,
    device_name: Option<&str>,
    agent_name: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(d) = device_name {
        form.insert("device_name".to_owned(), d.to_owned());
    }
    if let Some(a) = agent_name {
        form.insert("agent_name".to_owned(), a.to_owned());
    }
    let path = format!("/oauth/sessions/{}/", urlencoding::encode(session_id));
    client.patch_form(&path, &form).await
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
///
/// When `keep_session_id` is `Some`, the documented `exclude_current=true` +
/// `current_session_id=<id>` query pair is sent so that one session (e.g. the
/// caller's own) is preserved while all others are revoked (per `oauth.txt`).
pub async fn oauth_revoke_all(
    client: &ApiClient,
    keep_session_id: Option<&str>,
) -> Result<Value, CliError> {
    if let Some(session_id) = keep_session_id {
        let mut params = HashMap::new();
        params.insert("exclude_current".to_owned(), "true".to_owned());
        params.insert("current_session_id".to_owned(), session_id.to_owned());
        client.delete_with_params("/oauth/sessions/", &params).await
    } else {
        client.delete("/oauth/sessions/").await
    }
}

/// Get the current session info (alias for user details).
///
/// `GET /user/me/details/`
pub async fn session_info(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/me/details/").await
}

/// Token scope introspection.
///
/// `GET /auth/scopes/`
///
/// Returns the current token's auth type, scopes, agent status,
/// and whether the token has full access.
pub async fn scopes(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/auth/scopes/").await
}

/// Check whether a password reset code is valid.
///
/// `GET /user/password/{code}/details/`
///
/// Returns the email associated with the reset code, or an error
/// if the code is invalid, expired, or mismatched.
pub async fn password_reset_check(client: &ApiClient, code: &str) -> Result<Value, CliError> {
    let path = format!("/user/password/{}/details/", urlencoding::encode(code),);
    client.get(&path).await
}

#[cfg(test)]
mod tests {
    use super::pkce_authorize;
    use crate::client::ApiClient;
    use std::sync::{Arc, Mutex};

    /// Serve one canned authorize envelope and capture the raw request.
    async fn spawn_capturing_server() -> (String, Arc<Mutex<String>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let seen = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&seen);
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = sock.read(&mut buf).await {
                    *sink.lock().expect("capture lock") =
                        String::from_utf8_lossy(&buf[..n]).into_owned();
                }
                let body =
                    br#"{"result":"yes","response":{"auth_request_id":"ar1","expires_in":600}}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
            }
        });
        (addr, seen)
    }

    async fn authorize_request_line(agent_name: Option<&str>) -> String {
        let (addr, seen) = spawn_capturing_server().await;
        let client = ApiClient::new(&format!("http://{addr}"), None).expect("client builds");
        let _ = pkce_authorize(
            &client,
            "fastio-cli",
            "challenge",
            "state-1",
            "http://localhost:19836/callback",
            agent_name,
        )
        .await;
        let req = seen.lock().expect("capture lock").clone();
        req.lines().next().unwrap_or_default().to_owned()
    }

    /// A supplied `agent_name` must reach the authorize request. Verified live:
    /// the server echoes `agent_name` back only when it is sent.
    #[tokio::test]
    async fn pkce_authorize_sends_agent_name_when_supplied() {
        let line = authorize_request_line(Some("claude-2")).await;
        assert!(
            line.contains("agent_name=claude-2"),
            "agent_name must reach the authorize request, got: {line}"
        );
    }

    /// Absent or blank must OMIT the parameter rather than sending an empty
    /// label the server would have to interpret.
    #[tokio::test]
    async fn pkce_authorize_omits_absent_or_blank_agent_name() {
        for supplied in [None, Some(""), Some("   ")] {
            let line = authorize_request_line(supplied).await;
            assert!(
                !line.contains("agent_name"),
                "blank/absent agent_name must be omitted, got: {line} (supplied {supplied:?})"
            );
        }
    }
}
