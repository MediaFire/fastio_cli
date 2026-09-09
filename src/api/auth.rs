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

/// The access ceiling requested at PKCE initiate.
///
/// Bundled into a single argument because [`pkce_authorize`] would otherwise
/// carry more parameters than the lint budget allows. `Default` reproduces the
/// pre-existing request byte-for-byte: neither parameter is sent.
///
/// Both are a CEILING, not a grant — the consent page may narrow what is
/// actually approved.
#[derive(Debug, Default, Clone, Copy)]
pub struct AuthorizeAccess<'a> {
    /// Requested access mode (`r`, `rw`, or `rwa`). Sent as the `access_mode`
    /// query parameter only when `Some` and non-blank after trimming; omitted
    /// otherwise so the server applies its own default.
    pub access_mode: Option<&'a str>,
    /// Whether to request the account-settings (`userdetails:*:rw`) scope.
    /// Sends `account_settings=1` when `true`; the parameter is omitted when
    /// `false` — a literal `0` is never sent.
    pub account_settings: bool,
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
/// `access` carries the requested access ceiling; see [`AuthorizeAccess`].
///
/// [`PKCE_CLIENT_ID`]: crate::auth::pkce::PKCE_CLIENT_ID
pub async fn pkce_authorize(
    client: &ApiClient,
    client_id: &str,
    code_challenge: &str,
    state: &str,
    redirect_uri: &str,
    agent_name: Option<&str>,
    access: AuthorizeAccess<'_>,
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
    if let Some(mode) = access.access_mode.map(str::trim).filter(|m| !m.is_empty()) {
        params.insert("access_mode".to_owned(), mode.to_owned());
    }
    if access.account_settings {
        params.insert("account_settings".to_owned(), "1".to_owned());
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

/// Structured scope selectors for an API key or an OAuth session, as supplied
/// by CLI flags or MCP parameters.
///
/// The default value means "nothing was selected"; [`resolve_key_scopes`] maps
/// it to `Ok(None)` so the caller omits the `scopes` field entirely.
///
/// The four flags mirror four independent user-facing switches one-for-one, so
/// the lint's suggested state machine would add a translation layer between the
/// flags and this type without removing a single state. The illegal
/// combinations are rejected by [`resolve_key_scopes`] instead, which is the
/// one place both the CLI and the MCP parameters go through.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone)]
pub struct ApiKeyScopeSpec {
    /// Organization ids to scope to (`org:<id>:<mode>`).
    pub org: Vec<String>,
    /// Workspace ids to scope to (`workspace:<id>:<mode>`).
    pub workspace: Vec<String>,
    /// Share ids to scope to (`share:<id>:<mode>`).
    pub share: Vec<String>,
    /// Scope to the whole account (`user:*:<mode>`). Mutually exclusive with
    /// the three id lists.
    pub all: bool,
    /// Request the admin access mode (`rwa`). Mutually exclusive with
    /// [`ApiKeyScopeSpec::read_only`].
    pub admin: bool,
    /// Request the read-only access mode (`r`).
    pub read_only: bool,
    /// Append the account-settings scope, which is always `userdetails:*:rw`
    /// regardless of the access mode chosen for the other entities.
    pub account_settings: bool,
}

/// Append `entry` to `out` unless it is already present, preserving first-seen
/// order.
fn push_unique(out: &mut Vec<String>, entry: String) {
    if !out.contains(&entry) {
        out.push(entry);
    }
}

/// Append one `entity_type:id:mode` string per id, rejecting blank ids and ids
/// that are neither all-numeric nor the bare wildcard `*`.
///
/// `kind` is both the scope prefix and the flag name, so the error text names
/// the flag the user actually typed.
///
/// Org, workspace and share ids are numeric id strings, and the scope grammar
/// is `type:id:mode` — so an id carrying a separator (`1:rwa`, `1,2`) would be
/// pasted straight into the scope string and produce a grant the caller did not
/// ask for, silently and often a wider one. Validating the id against the
/// grammar is the only place that can be caught, since the resulting string is
/// well-formed by construction.
///
/// The bare wildcard `*` is accepted because it is a real grant the platform
/// issues and reports — `org:*:rw` means every organization, and the scope
/// detail contract gives `entity_id` as a numeric id or `*`. It is a distinct
/// grant from `--all`, which emits `user:*:<mode>` for a different entity type.
/// Anything containing a wildcard alongside other characters (`*1`, `**`) is
/// still refused: only the exact single-character form is a grammar-legal id.
fn push_entity(
    out: &mut Vec<String>,
    kind: &str,
    ids: &[String],
    mode: &str,
) -> Result<(), CliError> {
    for id in ids {
        let id = id.trim();
        if id.is_empty() {
            return Err(CliError::Parse(format!("--{kind} id must not be blank")));
        }
        if id != "*" && !id.chars().all(|c| c.is_ascii_digit()) {
            return Err(CliError::Parse(format!("--{kind} id must be numeric or *")));
        }
        push_unique(out, format!("{kind}:{id}:{mode}"));
    }
    Ok(())
}

/// Turn structured scope selectors into the JSON-encoded `scopes` string the
/// API expects, or `None` when no scoping was requested at all.
///
/// Shared by the CLI flags and the MCP tool parameters so both produce byte-
/// identical scope strings. `None` means "send no `scopes` field", which is the
/// unscoped, full-access default; it is returned ONLY when the caller supplied
/// no selector and no access-mode flag. Every other outcome is either a
/// non-empty array or an error — an empty array grants no authority and is
/// never produced.
///
/// Access mode: `--admin` gives `rwa`, `--read-only` gives `r`, neither gives
/// `rw`. The account-settings entry is always `userdetails:*:rw`.
///
/// # Errors
///
/// Returns [`CliError::Parse`] — the crate's input-validation variant — when
/// `admin` and `read_only` are both set, when `all` is combined with an id
/// list, when an access-mode flag is supplied with nothing the mode can apply
/// to, or when an id is blank, or is after trimming neither all-numeric nor the
/// bare wildcard `*`.
pub fn resolve_key_scopes(spec: &ApiKeyScopeSpec) -> Result<Option<String>, CliError> {
    let has_entities = !spec.org.is_empty() || !spec.workspace.is_empty() || !spec.share.is_empty();
    // The account-settings scope is ALWAYS `userdetails:*:rw`, so it is not a
    // target an access mode can be applied to: only the entity selectors and
    // `--all` carry the mode.
    let has_mode_target = has_entities || spec.all;
    let has_selector = has_mode_target || spec.account_settings;

    // No structured input whatsoever — the caller omits `scopes` and the
    // credential keeps today's full access.
    if !has_selector && !spec.admin && !spec.read_only {
        return Ok(None);
    }

    if spec.admin && spec.read_only {
        return Err(CliError::Parse(
            "--admin and --read-only are mutually exclusive; supply at most one".to_owned(),
        ));
    }
    if spec.all && has_entities {
        return Err(CliError::Parse(
            "--all cannot be combined with --org, --workspace, or --share".to_owned(),
        ));
    }
    if (spec.admin || spec.read_only) && !has_mode_target {
        // An access mode with nothing to apply it to is a half-typed command,
        // not a request for account-wide access — fail rather than guess.
        // `--account-settings` does not count: pairing it with `--admin` would
        // otherwise drop the requested ceiling silently and issue a credential
        // narrower than the one asked for.
        let flag = if spec.admin { "--admin" } else { "--read-only" };
        return Err(CliError::Parse(format!(
            "{flag} needs at least one of --org/--workspace/--share/--all \
             (the account-settings scope is always rw)"
        )));
    }

    let mode = if spec.admin {
        "rwa"
    } else if spec.read_only {
        "r"
    } else {
        "rw"
    };

    let mut entries: Vec<String> = Vec::new();
    if spec.all {
        entries.push(format!("user:*:{mode}"));
    }
    push_entity(&mut entries, "org", &spec.org, mode)?;
    push_entity(&mut entries, "workspace", &spec.workspace, mode)?;
    push_entity(&mut entries, "share", &spec.share, mode)?;
    if spec.account_settings {
        // Always `rw`: `userdetails:*:rw` is the only valid userdetails scope.
        push_unique(&mut entries, "userdetails:*:rw".to_owned());
    }

    if entries.is_empty() {
        // Unreachable — every surviving path pushes at least one entry. Kept as
        // a fail-closed guard so an empty array can never be sent.
        return Err(CliError::Parse(
            "no scopes to apply; supply --org/--workspace/--share/--all/--account-settings"
                .to_owned(),
        ));
    }

    Ok(Some(
        Value::Array(entries.into_iter().map(Value::String).collect()).to_string(),
    ))
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

/// Narrow an OAuth session's granted scopes.
///
/// `PATCH /oauth/sessions/{session_id}/` with a single form-encoded `scopes`
/// field carrying the JSON-encoded array of `entity_type:entity_id:access_mode`
/// strings (the shape [`resolve_key_scopes`] produces).
///
/// The new set must be narrower than or equal to what the session already
/// holds; the server enforces that and refuses a widening request, so this is a
/// give-up-authority operation only.
pub async fn oauth_narrow(
    client: &ApiClient,
    session_id: &str,
    scopes_json: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("scopes".to_owned(), scopes_json.to_owned());
    let path = format!("/oauth/sessions/{}/", urlencoding::encode(session_id));
    client.patch_form(&path, &form).await
}

/// Recovery hint for a lost compare-and-swap race on an API key's scope set
/// ([`api_key_update`]).
///
/// The server updates a key's scopes with a conditional write against the set
/// it read, so a concurrent update by another credential loses the race and
/// comes back HTTP `409`. The generic conflict wording ("wait a moment and
/// retry") is wrong here twice over: retrying the SAME request would re-apply a
/// set built from a now-stale read, and an update REPLACES the whole scope set,
/// so the loser would silently delete whatever the winner just added. The
/// recovery is therefore to re-read the key and re-state the full intended set.
pub const HINT_KEY_SCOPES_CHANGED: &str = "The credential's scope set changed underneath this request (someone else updated it). \
     Re-read it with `fastio auth api-key get <key-id>` and retry with the full intended set — an update REPLACES the whole set, so a retry built on the stale read would drop the change that won the race.";

/// Recovery hint for a lost compare-and-swap race on an OAuth session's scope
/// set ([`oauth_narrow`]).
///
/// Same mechanism as [`HINT_KEY_SCOPES_CHANGED`], read back through the session
/// endpoint. Narrowing is give-up-authority only, so a retry built on a stale
/// read can also be REFUSED as a widening request rather than merely losing
/// data — re-reading first is the only way to know what is still narrowable.
pub const HINT_SESSION_SCOPES_CHANGED: &str = "The credential's scope set changed underneath this request (someone else updated it). \
     Re-read it with `fastio auth oauth details <session-id>` and retry with the full intended set — narrowing is measured against what the session holds now, so a retry built on the stale read can be refused as a widening request.";

/// Re-hint an HTTP `409` from one of the two scope compare-and-swap surfaces.
///
/// [`api_key_update`] and [`oauth_narrow`] both write scopes conditionally
/// against the stored set, so both can lose a race. Keyed on the STATUS, never
/// on the per-call-site numeric codes: those are per-route fuses that change
/// independently of the contract, and a code list assembled from the two known
/// today would silently stop matching the moment a third site is added or a
/// fuse is renumbered.
///
/// Every other error — including a 409 that is not one of these calls, because
/// this is only ever applied at those two call sites — passes through
/// untouched. Nothing is retried: the caller must re-read before deciding what
/// to send, so an automatic retry would re-apply the stale set.
#[must_use]
pub fn map_scope_update_conflict(err: CliError, surface_hint: &'static str) -> CliError {
    match err {
        CliError::Api(api) if api.http_status == 409 => CliError::MappedApi {
            api,
            hint: Some(surface_hint),
        },
        other => other,
    }
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
    use super::{
        ApiKeyScopeSpec, AuthorizeAccess, HINT_KEY_SCOPES_CHANGED, HINT_SESSION_SCOPES_CHANGED,
        map_scope_update_conflict, oauth_narrow, pkce_authorize, resolve_key_scopes,
    };
    use crate::client::ApiClient;
    use crate::error::{ApiError, CliError};
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

    /// Index just past the `\r\n\r\n` that ends the request headers, or `None`
    /// while the headers are still incomplete.
    fn header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
    }

    /// The declared body length of a captured request, or `0` when the request
    /// carries no `Content-Length` (a GET, or a header still in flight).
    fn content_length(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0)
    }

    /// Serve one canned session envelope and capture the WHOLE request —
    /// request line, headers and body — so a form field can be asserted.
    async fn spawn_body_capturing_server() -> (String, Arc<Mutex<String>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let seen = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&seen);
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut acc: Vec<u8> = Vec::new();
                let mut buf = vec![0u8; 4096];
                // Headers and a small form body normally arrive in one segment,
                // but that is not guaranteed. Stopping at the first byte AFTER
                // the header terminator would capture a truncated body and turn
                // a split TCP write into a flaky wire assertion, so read until
                // `Content-Length` is satisfied.
                for _ in 0..8 {
                    let Ok(n) = sock.read(&mut buf).await else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    acc.extend_from_slice(&buf[..n]);
                    if let Some(pos) = header_end(&acc)
                        && acc.len() >= pos + content_length(&acc[..pos])
                    {
                        break;
                    }
                }
                *sink.lock().expect("capture lock") = String::from_utf8_lossy(&acc).into_owned();
                let body = br#"{"result":"yes","response":{"session":{"session_id":"s1"}}}"#;
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

    async fn authorize_request_line(
        agent_name: Option<&str>,
        access: AuthorizeAccess<'_>,
    ) -> String {
        let (addr, seen) = spawn_capturing_server().await;
        let client = ApiClient::new(&format!("http://{addr}"), None).expect("client builds");
        let _ = pkce_authorize(
            &client,
            "fastio-cli",
            "challenge",
            "state-1",
            "http://localhost:19836/callback",
            agent_name,
            access,
        )
        .await;
        let req = seen.lock().expect("capture lock").clone();
        req.lines().next().unwrap_or_default().to_owned()
    }

    /// Read one query-parameter value out of a captured request line.
    ///
    /// Substring matching is not good enough here: `access_mode=r` is a prefix
    /// of `access_mode=rwa`, so a `contains` assertion for the read-only case
    /// would pass on an admin request.
    fn query_param(line: &str, key: &str) -> Option<String> {
        let target = line.split_whitespace().nth(1)?;
        let query = target.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| v.to_owned())
        })
    }

    /// A supplied `agent_name` must reach the authorize request. Verified live:
    /// the server echoes `agent_name` back only when it is sent.
    #[tokio::test]
    async fn pkce_authorize_sends_agent_name_when_supplied() {
        let line = authorize_request_line(Some("cli-agent-2"), AuthorizeAccess::default()).await;
        assert!(
            line.contains("agent_name=cli-agent-2"),
            "agent_name must reach the authorize request, got: {line}"
        );
    }

    /// Absent or blank must OMIT the parameter rather than sending an empty
    /// label the server would have to interpret.
    #[tokio::test]
    async fn pkce_authorize_omits_absent_or_blank_agent_name() {
        for supplied in [None, Some(""), Some("   ")] {
            let line = authorize_request_line(supplied, AuthorizeAccess::default()).await;
            assert!(
                !line.contains("agent_name"),
                "blank/absent agent_name must be omitted, got: {line} (supplied {supplied:?})"
            );
        }
    }

    // ─── Access ceiling on the initiate GET ──────────────────────────────────

    /// The admin path asks for the `rwa` ceiling on the initiate itself.
    #[tokio::test]
    async fn pkce_authorize_sends_admin_access_mode() {
        let access = AuthorizeAccess {
            access_mode: Some("rwa"),
            account_settings: false,
        };
        let line = authorize_request_line(None, access).await;
        assert_eq!(
            query_param(&line, "access_mode").as_deref(),
            Some("rwa"),
            "got: {line}"
        );
    }

    /// Read-only asks for `r` — and must not be confusable with `rwa`.
    #[tokio::test]
    async fn pkce_authorize_sends_read_only_access_mode() {
        let access = AuthorizeAccess {
            access_mode: Some("r"),
            account_settings: false,
        };
        let line = authorize_request_line(None, access).await;
        assert_eq!(
            query_param(&line, "access_mode").as_deref(),
            Some("r"),
            "got: {line}"
        );
    }

    /// Neither flag sends NO `access_mode` at all — the absence is the
    /// contract, not some client-side default value.
    #[tokio::test]
    async fn pkce_authorize_omits_access_mode_when_unset() {
        let line = authorize_request_line(None, AuthorizeAccess::default()).await;
        assert_eq!(query_param(&line, "access_mode"), None, "got: {line}");
    }

    /// A blank or whitespace-only ceiling is treated as absent rather than
    /// sent as an empty value the server would have to interpret.
    #[tokio::test]
    async fn pkce_authorize_treats_blank_access_mode_as_absent() {
        for supplied in [Some(""), Some("   ")] {
            let access = AuthorizeAccess {
                access_mode: supplied,
                account_settings: false,
            };
            let line = authorize_request_line(None, access).await;
            assert_eq!(
                query_param(&line, "access_mode"),
                None,
                "blank ceiling must be omitted (supplied {supplied:?}), got: {line}"
            );
        }
    }

    /// `account_settings` is sent as the literal `1` when requested.
    #[tokio::test]
    async fn pkce_authorize_sends_account_settings_when_requested() {
        let access = AuthorizeAccess {
            access_mode: None,
            account_settings: true,
        };
        let line = authorize_request_line(None, access).await;
        assert_eq!(
            query_param(&line, "account_settings").as_deref(),
            Some("1"),
            "got: {line}"
        );
    }

    /// …and OMITTED when not — never sent as `0`, which would still be a
    /// request the consent page has to answer.
    #[tokio::test]
    async fn pkce_authorize_omits_account_settings_when_not_requested() {
        let line = authorize_request_line(None, AuthorizeAccess::default()).await;
        assert_eq!(query_param(&line, "account_settings"), None, "got: {line}");
        assert!(
            !line.contains("account_settings"),
            "a literal 0 must never be sent, got: {line}"
        );
    }

    // ─── Scope resolution ────────────────────────────────────────────────────

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    fn resolved(spec: &ApiKeyScopeSpec) -> String {
        resolve_key_scopes(spec)
            .expect("spec resolves")
            .expect("spec produces scopes")
    }

    fn error_message(spec: &ApiKeyScopeSpec) -> String {
        match resolve_key_scopes(spec) {
            Err(CliError::Parse(msg)) => msg,
            Err(other) => panic!("expected CliError::Parse, got {other:?}"),
            Ok(value) => panic!("expected a validation error, got {value:?}"),
        }
    }

    fn entries(spec: &ApiKeyScopeSpec) -> Vec<String> {
        serde_json::from_str(&resolved(spec)).expect("scopes is a JSON array of strings")
    }

    #[test]
    fn org_with_admin_yields_rwa() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["1"]),
            admin: true,
            ..Default::default()
        };
        assert_eq!(resolved(&spec), "[\"org:1:rwa\"]");
    }

    #[test]
    fn all_with_read_only_yields_the_user_wildcard() {
        let spec = ApiKeyScopeSpec {
            all: true,
            read_only: true,
            ..Default::default()
        };
        assert_eq!(resolved(&spec), "[\"user:*:r\"]");
    }

    /// No structured input at all is the ONLY route to `None` — the caller then
    /// omits `scopes` and the credential keeps full access.
    #[test]
    fn no_flags_resolves_to_none() {
        assert_eq!(
            resolve_key_scopes(&ApiKeyScopeSpec::default()).expect("empty spec resolves"),
            None
        );
    }

    /// The account-settings entry is `rw` regardless of the access mode chosen
    /// for everything else, and appears exactly once.
    #[test]
    fn account_settings_is_always_rw_and_appears_once() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["5"]),
            read_only: true,
            account_settings: true,
            ..Default::default()
        };
        let out = entries(&spec);
        let userdetails: Vec<&String> = out
            .iter()
            .filter(|e| e.starts_with("userdetails:"))
            .collect();
        assert_eq!(
            userdetails.len(),
            1,
            "exactly one userdetails entry: {out:?}"
        );
        assert_eq!(userdetails[0], "userdetails:*:rw", "got: {out:?}");
        assert!(
            out.contains(&"org:5:r".to_owned()),
            "the read-only mode still applies to the org: {out:?}"
        );
    }

    /// `--account-settings` on its own is a complete request, not a partial one.
    #[test]
    fn account_settings_alone_is_valid() {
        let spec = ApiKeyScopeSpec {
            account_settings: true,
            ..Default::default()
        };
        assert_eq!(resolved(&spec), "[\"userdetails:*:rw\"]");
    }

    /// An access mode with nothing to apply it to is a half-typed command.
    #[test]
    fn admin_alone_is_an_error() {
        let spec = ApiKeyScopeSpec {
            admin: true,
            ..Default::default()
        };
        assert_eq!(
            error_message(&spec),
            "--admin needs at least one of --org/--workspace/--share/--all \
             (the account-settings scope is always rw)"
        );
    }

    #[test]
    fn read_only_alone_is_an_error() {
        let spec = ApiKeyScopeSpec {
            read_only: true,
            ..Default::default()
        };
        assert_eq!(
            error_message(&spec),
            "--read-only needs at least one of --org/--workspace/--share/--all \
             (the account-settings scope is always rw)"
        );
    }

    /// `--admin --account-settings` names a ceiling with nothing to apply it
    /// to: the account-settings scope is always `userdetails:*:rw`, so
    /// accepting the pair would issue a key WITHOUT the admin access the
    /// caller asked for and report success.
    #[test]
    fn admin_with_only_account_settings_is_an_error() {
        let spec = ApiKeyScopeSpec {
            admin: true,
            account_settings: true,
            ..Default::default()
        };
        assert_eq!(
            error_message(&spec),
            "--admin needs at least one of --org/--workspace/--share/--all \
             (the account-settings scope is always rw)"
        );

        let read_only = ApiKeyScopeSpec {
            read_only: true,
            account_settings: true,
            ..Default::default()
        };
        assert_eq!(
            error_message(&read_only),
            "--read-only needs at least one of --org/--workspace/--share/--all \
             (the account-settings scope is always rw)"
        );
    }

    /// POSITIVE CONTROL for the rule above: a mode flag WITH a real target and
    /// `--account-settings` alongside is still valid, and the mode still
    /// reaches the entity.
    #[test]
    fn admin_with_a_target_and_account_settings_is_valid() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["7"]),
            admin: true,
            account_settings: true,
            ..Default::default()
        };
        assert_eq!(
            resolved(&spec),
            "[\"org:7:rwa\",\"userdetails:*:rw\"]",
            "the ceiling applies to the org; account settings stay rw"
        );
    }

    /// A blank id would build the scope string `org::rw`, which names nothing.
    #[test]
    fn blank_id_is_an_error() {
        let spec = ApiKeyScopeSpec {
            org: ids(&[""]),
            ..Default::default()
        };
        assert_eq!(error_message(&spec), "--org id must not be blank");
    }

    #[test]
    fn whitespace_only_id_is_an_error() {
        let spec = ApiKeyScopeSpec {
            workspace: ids(&["   "]),
            ..Default::default()
        };
        assert_eq!(error_message(&spec), "--workspace id must not be blank");
    }

    /// An id carrying a scope-grammar separator, a partial wildcard, or any
    /// other non-digit would be pasted verbatim into `type:id:mode` and produce
    /// a grant nobody asked for — `--org "1:rwa"` alone would smuggle an admin
    /// entry past the `--admin`/`--read-only` checks.
    #[test]
    fn non_numeric_ids_are_an_error() {
        for bad in ["1:rwa", "1,2", "abc", "1 2", "**", "*1", "1*"] {
            let spec = ApiKeyScopeSpec {
                org: ids(&[bad]),
                ..Default::default()
            };
            assert_eq!(
                error_message(&spec),
                "--org id must be numeric or *",
                "id {bad:?} must be rejected"
            );
        }
    }

    /// `org:*:rw` is a real grant the platform issues and reports ("All
    /// Organizations"), and `--all` is not a substitute — it emits
    /// `user:*:<mode>`, a different entity type. The numeric rule exists for
    /// separators, so the bare wildcard has to survive it.
    #[test]
    fn a_bare_wildcard_entity_id_resolves() {
        for kind in ["org", "workspace", "share"] {
            let mut spec = ApiKeyScopeSpec::default();
            match kind {
                "workspace" => spec.workspace = ids(&["*"]),
                "share" => spec.share = ids(&["*"]),
                _ => spec.org = ids(&["*"]),
            }
            assert_eq!(resolved(&spec), format!("[\"{kind}:*:rw\"]"));
        }
    }

    /// The access mode applies to a wildcard entity exactly as it does to a
    /// numeric one — this is the workflow the admin hints send readers to.
    #[test]
    fn a_wildcard_entity_id_takes_the_admin_ceiling() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["*"]),
            admin: true,
            ..Default::default()
        };
        assert_eq!(resolved(&spec), "[\"org:*:rwa\"]");
    }

    /// The blank case keeps its own wording — the MCP surface matches on it.
    #[test]
    fn blank_id_keeps_the_blank_message_not_the_numeric_one() {
        let spec = ApiKeyScopeSpec {
            share: ids(&["  "]),
            ..Default::default()
        };
        assert_eq!(error_message(&spec), "--share id must not be blank");
    }

    /// POSITIVE CONTROL: real ids are 19-digit numeric strings, so the check
    /// must not be a length or `u32`-parse test in disguise.
    #[test]
    fn a_long_all_digit_id_resolves() {
        let spec = ApiKeyScopeSpec {
            workspace: ids(&["1234567890123456789"]),
            ..Default::default()
        };
        assert_eq!(resolved(&spec), "[\"workspace:1234567890123456789:rw\"]");
    }

    #[test]
    fn all_combined_with_an_entity_id_is_an_error() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["1"]),
            all: true,
            ..Default::default()
        };
        assert_eq!(
            error_message(&spec),
            "--all cannot be combined with --org, --workspace, or --share"
        );
    }

    /// The flags contradict each other; the CLI blocks this but the MCP
    /// parameters do not, so the resolver has to.
    #[test]
    fn admin_with_read_only_is_an_error() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["1"]),
            admin: true,
            read_only: true,
            ..Default::default()
        };
        assert_eq!(
            error_message(&spec),
            "--admin and --read-only are mutually exclusive; supply at most one"
        );
    }

    #[test]
    fn duplicate_ids_are_deduplicated() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["1", "1", "2", "1"]),
            ..Default::default()
        };
        assert_eq!(resolved(&spec), "[\"org:1:rw\",\"org:2:rw\"]");
    }

    /// Trimming happens BEFORE the scope string is built, and before the
    /// duplicate check — so a padded repeat is still a duplicate.
    #[test]
    fn ids_are_trimmed() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["  7  ", "7"]),
            ..Default::default()
        };
        assert_eq!(resolved(&spec), "[\"org:7:rw\"]");
    }

    /// Entity types compose in the documented order: `user`, then org,
    /// workspace, share in that order, then the account-settings entry last.
    #[test]
    fn entity_types_compose_in_the_documented_order() {
        let spec = ApiKeyScopeSpec {
            org: ids(&["1"]),
            workspace: ids(&["2"]),
            share: ids(&["3"]),
            account_settings: true,
            ..Default::default()
        };
        assert_eq!(
            entries(&spec),
            vec![
                "org:1:rw".to_owned(),
                "workspace:2:rw".to_owned(),
                "share:3:rw".to_owned(),
                "userdetails:*:rw".to_owned(),
            ]
        );
    }

    /// An empty array is NOT unconstrained — it grants no authority and the
    /// credential is refused everywhere. The resolver must never produce one.
    #[test]
    fn a_successful_resolution_is_never_an_empty_array() {
        let specs = [
            ApiKeyScopeSpec {
                all: true,
                ..Default::default()
            },
            ApiKeyScopeSpec {
                account_settings: true,
                ..Default::default()
            },
            ApiKeyScopeSpec {
                org: ids(&["1"]),
                admin: true,
                ..Default::default()
            },
            ApiKeyScopeSpec {
                share: ids(&["9"]),
                read_only: true,
                account_settings: true,
                ..Default::default()
            },
        ];
        for spec in &specs {
            let out = resolved(spec);
            assert_ne!(out, "[]", "spec produced an empty grant: {spec:?}");
            assert!(
                !entries(spec).is_empty(),
                "spec produced no entries: {spec:?}"
            );
        }
    }

    // ─── Session narrowing ───────────────────────────────────────────────────

    /// The narrow call is a PATCH on the session path carrying one `scopes`
    /// form field with the JSON array verbatim.
    #[tokio::test]
    async fn oauth_narrow_patches_the_session_with_a_scopes_form_field() {
        let (addr, seen) = spawn_body_capturing_server().await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let _ = oauth_narrow(&client, "a1b2c3", "[\"org:1:r\"]").await;

        let req = seen.lock().expect("capture lock").clone();
        let line = req.lines().next().unwrap_or_default();
        assert!(
            line.starts_with("PATCH /oauth/sessions/a1b2c3/ "),
            "got: {line}"
        );

        let body = req.split("\r\n\r\n").nth(1).unwrap_or_default();
        let value = body
            .strip_prefix("scopes=")
            .unwrap_or_else(|| panic!("the body must be the single scopes field, got: {body}"));
        let decoded = urlencoding::decode(value).expect("form value decodes");
        assert_eq!(decoded, "[\"org:1:r\"]");
    }

    // ─── the scope compare-and-swap conflict mapper ────────────────────────

    /// A `409` from either scope-writing surface must be re-hinted with THAT
    /// surface's re-read command.
    ///
    /// The generic conflict advice is "wait a moment and retry", which is the
    /// one thing a caller must not do here: the request they would retry was
    /// built from a read that is now stale, and a key update REPLACES the whole
    /// scope set — so the blind retry deletes exactly the change that won the
    /// race.
    #[test]
    fn a_409_is_re_hinted_per_surface() {
        for (hint, needle) in [
            (HINT_KEY_SCOPES_CHANGED, "api-key get <key-id>"),
            (HINT_SESSION_SCOPES_CHANGED, "oauth details <session-id>"),
        ] {
            let err = CliError::Api(ApiError::new(181_408, None, "conflict".to_owned(), 409));
            let mapped = map_scope_update_conflict(err, hint);
            assert!(
                matches!(mapped, CliError::MappedApi { .. }),
                "a 409 on a scope write must be re-hinted"
            );
            assert_eq!(mapped.suggestion(), Some(hint));
            let rendered = mapped.suggestion().unwrap_or_default();
            assert!(
                rendered.contains(needle),
                "the hint must name the re-read command for its own surface: {rendered}"
            );
            assert!(
                rendered.contains("changed underneath this request"),
                "the hint must say WHY the write was refused: {rendered}"
            );
        }
    }

    /// Keyed on the STATUS, not the per-call-site code.
    ///
    /// The backend numbers these fuses per route (a different one for the key
    /// update than for the session PATCH), and a client-side code list
    /// assembled from the two known today stops matching the moment a third
    /// surface appears or a fuse is renumbered — silently, with the caller sent
    /// back to "wait and retry".
    #[test]
    fn any_409_code_maps_including_an_unknown_one() {
        for code in [181_408_u32, 172_160, 0, 999_999] {
            let err = CliError::Api(ApiError::new(code, None, "conflict".to_owned(), 409));
            let mapped = map_scope_update_conflict(err, HINT_KEY_SCOPES_CHANGED);
            assert_eq!(
                mapped.suggestion(),
                Some(HINT_KEY_SCOPES_CHANGED),
                "code {code} rides HTTP 409 and must be re-hinted"
            );
        }
    }

    /// The negative control. Everything that is NOT a 409 passes through
    /// untouched — including the scope refusals, whose own reason-keyed hints
    /// would be destroyed by an over-broad wrap.
    #[test]
    fn non_conflict_errors_pass_through_unchanged() {
        let forbidden = CliError::Api(ApiError::new(10_770, None, "forbidden".to_owned(), 403));
        let before = forbidden.suggestion();
        let after = map_scope_update_conflict(forbidden, HINT_KEY_SCOPES_CHANGED);
        assert!(
            matches!(after, CliError::Api(_)),
            "a non-409 must not be re-hinted"
        );
        assert_eq!(after.suggestion(), before, "its own hint must survive");
        assert_ne!(after.suggestion(), Some(HINT_KEY_SCOPES_CHANGED));

        // And a non-API error (transport, parse) is not an API conflict at all.
        let parse = CliError::Parse("html".to_owned());
        let mapped = map_scope_update_conflict(parse, HINT_KEY_SCOPES_CHANGED);
        assert!(matches!(mapped, CliError::Parse(_)));
    }

    /// A session id is a path segment: anything needing encoding is encoded,
    /// so it can never open a second path segment.
    #[tokio::test]
    async fn oauth_narrow_percent_encodes_the_session_id() {
        let (addr, seen) = spawn_body_capturing_server().await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");
        let _ = oauth_narrow(&client, "a/b", "[]").await;

        let req = seen.lock().expect("capture lock").clone();
        let line = req.lines().next().unwrap_or_default();
        assert!(
            line.starts_with("PATCH /oauth/sessions/a%2Fb/ "),
            "the session id must be percent-encoded, got: {line}"
        );
    }
}
