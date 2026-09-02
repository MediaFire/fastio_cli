/// Auth command implementations for `fastio auth *`.
///
/// Handles login (basic + PKCE), logout, status, signup, email
/// verification, 2FA management, and API key management.
use anyhow::{Context, Result};
use secrecy::SecretString;
use serde_json::{Value, json};

use fastio_cli::api;
use fastio_cli::auth::credentials::{CredentialsFile, StoredCredentials};
use fastio_cli::auth::pkce;
use fastio_cli::auth::token;
use fastio_cli::client::ApiClient;
use fastio_cli::config::Config;
use fastio_cli::error::CliError;

use super::CommandContext;

/// Execute an auth subcommand.
pub async fn execute(
    command: &AuthCommand,
    config: &Config,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    match command {
        AuthCommand::Login {
            email,
            password,
            agent_name,
        } => {
            if let (Some(email), Some(password)) = (email.as_deref(), password.as_deref()) {
                // Refuse rather than ignore: basic auth does not carry an agent
                // label, so accepting the flag here would silently produce a
                // credential the operator believes is named and is not.
                if agent_name.is_some() {
                    anyhow::bail!(
                        "--agent-name applies to browser (PKCE) login only; basic auth \
                         does not carry an agent label. Omit --email/--password to use \
                         browser login, or name an API key with \
                         `fastio auth api-key create --agent-name`."
                    )
                }
                login_basic(config, ctx, email, password).await
            } else if password.is_some() {
                anyhow::bail!(
                    "--password requires --email. Provide both for direct login, \
                     or omit both for browser login."
                )
            } else {
                login_pkce(config, ctx, email.as_deref(), agent_name.as_deref()).await
            }
        }
        AuthCommand::Logout => logout(ctx),
        AuthCommand::Signout => signout(config, ctx).await,
        AuthCommand::InvalidateAll => invalidate_all(ctx).await,
        AuthCommand::Status => status(ctx).await,
        AuthCommand::Signup {
            email,
            password,
            first_name,
            last_name,
            agent,
        } => {
            signup(
                config,
                ctx,
                email,
                password,
                first_name.as_deref(),
                last_name.as_deref(),
                *agent,
            )
            .await
        }
        AuthCommand::Verify { email, code } => verify(ctx, email, code.as_deref()).await,
        AuthCommand::TwoFa(cmd) => two_fa(cmd, ctx).await,
        AuthCommand::ApiKey(cmd) => api_key(cmd, ctx).await,
        AuthCommand::Check => check(ctx).await,
        AuthCommand::Session => session(ctx).await,
        AuthCommand::EmailCheck { email } => email_check(ctx, email).await,
        AuthCommand::PasswordResetRequest { email } => password_reset_request(ctx, email).await,
        AuthCommand::PasswordReset {
            code,
            password1,
            password2,
        } => password_reset(ctx, code, password1, password2).await,
        AuthCommand::Oauth(cmd) => oauth(cmd, ctx).await,
        AuthCommand::Scopes => scopes(ctx).await,
        AuthCommand::PasswordResetCheck { code } => password_reset_check(ctx, code).await,
    }
}

/// Auth subcommand variants.
#[derive(Clone)]
#[non_exhaustive]
pub enum AuthCommand {
    /// Log in with optional email/password or PKCE browser flow.
    Login {
        /// Email address for basic auth login.
        email: Option<String>,
        /// Password for basic auth login.
        password: Option<String>,
        /// Agent instance label for the resulting credential (PKCE only).
        agent_name: Option<String>,
    },
    /// Clear stored credentials (local only).
    Logout,
    /// Server-side sign-out (revoke revocable session tokens) + local clear.
    Signout,
    /// Invalidate all sessions everywhere + local clear.
    InvalidateAll,
    /// Show authentication status.
    Status,
    /// Create a new account.
    Signup {
        /// Email address.
        email: String,
        /// Password.
        password: String,
        /// First name.
        first_name: Option<String>,
        /// Last name.
        last_name: Option<String>,
        /// Create an AI-agent account.
        agent: bool,
    },
    /// Send or confirm email verification.
    Verify {
        /// Email address.
        email: String,
        /// Verification code (if confirming).
        code: Option<String>,
    },
    /// 2FA subcommands.
    TwoFa(TwoFaCommand),
    /// API key subcommands.
    ApiKey(ApiKeyCommand),
    /// Verify token validity.
    Check,
    /// Show session info from stored credentials.
    Session,
    /// Check email availability.
    EmailCheck {
        /// Email to check.
        email: String,
    },
    /// Request a password reset.
    PasswordResetRequest {
        /// Email address.
        email: String,
    },
    /// Complete a password reset.
    PasswordReset {
        /// Reset code.
        code: String,
        /// New password.
        password1: String,
        /// Confirm new password.
        password2: String,
    },
    /// OAuth session subcommands.
    Oauth(OauthCommand),
    /// Token scope introspection.
    Scopes,
    /// Check password reset code validity.
    PasswordResetCheck {
        /// Reset code to check.
        code: String,
    },
}

/// 2FA subcommand variants.
#[derive(Clone)]
#[non_exhaustive]
pub enum TwoFaCommand {
    /// Enable 2FA on a channel.
    Setup {
        /// Channel: sms, totp, or whatsapp.
        channel: String,
    },
    /// Verify a 2FA code.
    Verify {
        /// The 2FA verification code.
        code: String,
    },
    /// Disable 2FA.
    Disable {
        /// 2FA verification token.
        token: String,
    },
    /// Check 2FA status.
    Status,
    /// Send a 2FA code on a channel.
    Send {
        /// Channel: sms, totp, or whatsapp.
        channel: String,
    },
    /// Verify TOTP setup.
    VerifySetup {
        /// The TOTP verification token.
        token: String,
    },
}

/// API key subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ApiKeyCommand {
    /// Create a new API key.
    Create {
        /// Key label / memo.
        name: Option<String>,
        /// Scopes (JSON array string).
        scopes: Option<String>,
        /// Agent / application name for tracking.
        agent_name: Option<String>,
        /// Expiration datetime (strtotime-compatible).
        expires: Option<String>,
    },
    /// List all API keys.
    List,
    /// Delete an API key by ID.
    Delete {
        /// The API key ID.
        key_id: String,
    },
    /// Get API key details.
    Get {
        /// The API key ID.
        key_id: String,
    },
    /// Update an API key.
    Update {
        /// The API key ID.
        key_id: String,
        /// New label.
        name: Option<String>,
        /// New scopes.
        scopes: Option<String>,
        /// New agent / application name.
        agent_name: Option<String>,
        /// New expiration datetime (empty string clears).
        expires: Option<String>,
    },
}

/// OAuth session subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OauthCommand {
    /// List OAuth sessions.
    List,
    /// Get OAuth session details.
    Details {
        /// Session ID.
        session_id: String,
    },
    /// Rename a session's display labels.
    Rename {
        /// Session ID.
        session_id: String,
        /// New device name (empty string clears).
        device_name: Option<String>,
        /// New agent name (empty string clears).
        agent_name: Option<String>,
    },
    /// Revoke a single session.
    Revoke {
        /// Session ID.
        session_id: String,
    },
    /// Revoke all sessions.
    RevokeAll {
        /// Session ID to keep active while revoking all others.
        exclude_current: Option<String>,
    },
}

/// Login via email/password (HTTP Basic Auth).
async fn login_basic(
    _config: &Config,
    ctx: &CommandContext<'_>,
    email: &str,
    password: &str,
) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;

    let result = api::auth::sign_in(&client, email, password)
        .await
        .context("login failed")?;

    // Store credentials
    let now = chrono::Utc::now().timestamp();
    let creds = StoredCredentials {
        token: Some(SecretString::from(result.auth_token.clone())),
        refresh_token: None,
        api_key: None,
        expires_at: Some(now + result.expires_in),
        user_id: None,
        email: Some(email.to_owned()),
        auth_method: Some("basic".to_owned()),
    };

    let mut creds_file =
        CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
    creds_file
        .set(ctx.profile_name, creds, ctx.config_dir)
        .context("failed to save credentials")?;

    let value = if result.two_factor {
        json!({
            "status": "two_factor_required",
            // `--code` is REQUIRED — there is no positional form, and the
            // positional spelling exits with "unexpected argument". This message
            // is the SUCCESS path of every 2FA login, so it is the most likely
            // place a user meets this command: getting it wrong strands them
            // mid-2FA, and the stored token is `twofactor`-scoped, so their next
            // command hits `10175`. `HINT_SCOPE_INCORRECT` carries the same
            // wording and the two must stay in step.
            "message": "2FA verification required. Run: fastio auth 2fa verify --code <CODE>",
            "expires_in": result.expires_in,
        })
    } else {
        json!({
            "status": "authenticated",
            "email": email,
            "expires_in": result.expires_in,
            "profile": ctx.profile_name,
        })
    };

    ctx.output.render(&value)?;
    Ok(())
}

/// Environment variable naming the agent instance behind a sign-in.
const AGENT_NAME_ENV: &str = "FASTIO_AGENT_NAME";

/// Resolve the agent label for a sign-in: `--agent-name`, else
/// `FASTIO_AGENT_NAME`, else nothing.
///
/// The env var matters more than the flag here. A fleet is typically several
/// agent processes on one machine sharing one credential — the default
/// resolves them all to a single identity — and per-process environment is the
/// one place an operator can distinguish them without rewriting each command.
///
/// Blank is treated as absent, so `FASTIO_AGENT_NAME=""` does not send an empty
/// label the server would have to interpret.
fn resolve_agent_name(flag: Option<&str>) -> Option<String> {
    let from_flag = flag
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(ToOwned::to_owned);
    from_flag.or_else(|| {
        std::env::var(AGENT_NAME_ENV)
            .ok()
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
    })
}

/// Login via PKCE browser flow.
async fn login_pkce(
    _config: &Config,
    ctx: &CommandContext<'_>,
    email_hint: Option<&str>,
    agent_name: Option<&str>,
) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;

    let challenge = pkce::generate_challenge().context("failed to generate PKCE challenge")?;

    let agent_name = resolve_agent_name(agent_name);

    // Initiate the PKCE flow
    let auth_resp = api::auth::pkce_authorize(
        &client,
        pkce::PKCE_CLIENT_ID,
        &challenge.code_challenge,
        &challenge.state,
        pkce::PKCE_REDIRECT_URI,
        agent_name.as_deref(),
    )
    .await
    .context("failed to initiate PKCE authorization")?;

    // Build the browser URL
    let browser_url = format!(
        "https://go.fast.io/connect?auth_request_id={}&display_code=true",
        urlencoding::encode(&auth_resp.auth_request_id)
    );

    eprintln!("Opening browser for authentication...");
    eprintln!("If the browser does not open, visit:");
    eprintln!("  {browser_url}");
    eprintln!();

    // Try to open the browser
    let _ = open::that(&browser_url);

    // Prompt the user to paste the authorization code from the browser
    let (code, _state) = prompt_for_code(&challenge.state)?;

    // Exchange code for tokens
    let token_resp = api::auth::pkce_token_exchange(
        &client,
        &code,
        &challenge.code_verifier,
        pkce::PKCE_CLIENT_ID,
        pkce::PKCE_REDIRECT_URI,
    )
    .await
    .context("failed to exchange authorization code for tokens")?;

    // Store credentials
    let now = chrono::Utc::now().timestamp();
    let creds = StoredCredentials {
        token: Some(SecretString::from(token_resp.access_token.clone())),
        refresh_token: token_resp.refresh_token.map(SecretString::from),
        api_key: None,
        expires_at: Some(now + token_resp.expires_in),
        user_id: None,
        email: email_hint.map(String::from),
        auth_method: Some("pkce".to_owned()),
    };

    let mut creds_file =
        CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
    creds_file
        .set(ctx.profile_name, creds, ctx.config_dir)
        .context("failed to save credentials")?;

    let value = json!({
        "status": "authenticated",
        "auth_method": "pkce",
        "expires_in": token_resp.expires_in,
        "profile": ctx.profile_name,
    });

    ctx.output.render(&value)?;
    Ok(())
}

/// Prompt the user to manually paste an authorization code (sync wrapper).
fn prompt_for_code(state: &str) -> Result<(String, String)> {
    use std::io::{self, BufRead, Write};

    eprint!("Authorization code: ");
    io::stderr().flush()?;

    let mut code = String::new();
    io::stdin()
        .lock()
        .read_line(&mut code)
        .context("failed to read authorization code from stdin")?;

    let code = code.trim().to_owned();
    if code.is_empty() {
        anyhow::bail!("no authorization code provided");
    }

    // For manual entry, we trust the state since the user is pasting
    // the code from the same browser session we initiated.
    Ok((code, state.to_owned()))
}

/// Clear stored credentials for the active profile.
fn logout(ctx: &CommandContext<'_>) -> Result<()> {
    let mut creds_file =
        CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
    creds_file
        .remove(ctx.profile_name, ctx.config_dir)
        .context("failed to clear credentials")?;

    let value = json!({
        "status": "logged_out",
        "profile": ctx.profile_name,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Sign out server-side (invalidate the user's revocable sessions), then
/// clear local credentials for the active profile.
///
/// Best-effort when the stored credential is already dead: a 401 for a
/// profile-store bearer on the profile's own API base — or a stored token
/// that is already expired client-side — still clears local credentials, so
/// a dead profile never wedges in a signed-in state. The rendered
/// `server_signout_completed` field reports which path was taken. A 401 for a
/// `--token`/env bearer, or under an `--api-base` override that differs from
/// the profile's own base, stays fatal: the stored credentials were never
/// what that server rejected.
async fn signout(config: &Config, ctx: &CommandContext<'_>) -> Result<()> {
    // A stored token that is already expired client-side can never reach the
    // server (resolution refuses to send it) — the sign-out intent is still
    // satisfiable locally, so clear the profile instead of wedging it. The
    // `Auth` variant only ever originates from stored-profile expiry (flag
    // and env bearers return earlier in the precedence chain).
    let resolved = match token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir) {
        Ok(r) => r,
        Err(CliError::Auth(_)) => {
            let creds_file =
                CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
            if creds_file.get(ctx.profile_name).is_none() {
                anyhow::bail!("authentication required. Run: fastio auth login");
            }
            // The ACCESS token lapsed, which is precisely when the refresh token
            // matters most: it is the long-lived half and it is still live.
            // Revocation is unauthenticated, so an expired bearer cannot block
            // it — this path must revoke too, or the expiry case silently keeps
            // a decade-valid credential alive.
            let refresh_revoked = revoke_stored_refresh_token(ctx).await;
            return clear_and_render(ctx, false, refresh_revoked);
        }
        Err(e) => return Err(e).context("failed to resolve token"),
    };
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    let client = ApiClient::new(ctx.api_base, Some(t)).context("failed to create API client")?;

    // A dead-bearer 401 still clears local credentials — but ONLY when the
    // bearer came from the profile store (a dead `--token`/env token must
    // never wipe stored credentials it did not come from) AND the request
    // went to the profile's own API base (a foreign `--api-base` 401 says
    // nothing about the credential's validity on its real server). Any other
    // error (network, 5xx, 429) propagates without touching local state so
    // the user can retry. (`invalidate-all` stays strict on 401 entirely: a
    // dead caller key there means the account-wide invalidation did NOT
    // happen, and clearing local state would mask that failure.)
    // Revoke the OAuth refresh token BEFORE the session sign-out, and before
    // local credentials are cleared — once they are gone the token value is
    // unrecoverable and the server-side grant would outlive the "sign out"
    // silently. Per the published API docs, sign-out does not touch OAuth
    // tokens and callers must always revoke on logout; honouring only the first
    // half leaves a signed-out profile with a long-lived refresh token alive
    // (the doc's own example expires ten years out).
    //
    // Per RFC 7009 the endpoint always answers success and is unauthenticated,
    // so there is nothing to learn from the result and nothing to gate on it —
    // a failure here must NOT block the sign-out that follows (the published
    // API docs: clear local storage regardless of the response).
    let refresh_revoked = revoke_stored_refresh_token(ctx).await;

    let server_signout_completed = match api::auth::sign_out(&client).await {
        Ok(_) => true,
        Err(ref err)
            if dead_session_401(err)
                && bearer_from_store(
                    ctx.flag_token,
                    std::env::var("FASTIO_TOKEN").ok().as_deref(),
                    std::env::var("FASTIO_API_KEY").ok().as_deref(),
                )
                && config.api_base(None, Some(ctx.profile_name)) == ctx.api_base =>
        {
            false
        }
        Err(e) => return Err(e).context("sign-out failed"),
    };

    clear_and_render(ctx, server_signout_completed, refresh_revoked)
}

/// Revoke this profile's stored OAuth refresh token, best-effort.
///
/// Returns `None` when the profile has no refresh token (an API-key or
/// password profile — nothing to revoke), `Some(true)` on a completed call and
/// `Some(false)` when the call itself failed. The caller must NOT gate sign-out
/// on the result: RFC 7009 makes the response uninformative by design, and a
/// network failure here is no reason to leave the user signed in locally.
async fn revoke_stored_refresh_token(ctx: &CommandContext<'_>) -> Option<bool> {
    let creds_file = CredentialsFile::load(ctx.config_dir).ok()?;
    let creds = creds_file.get(ctx.profile_name)?;
    let refresh = creds.expose_refresh_token()?.to_owned();
    if refresh.trim().is_empty() {
        return None;
    }
    // Unauthenticated endpoint — build a bare client rather than reusing the
    // bearer one, so a dead access token cannot stop the revocation.
    let client = ApiClient::new(ctx.api_base, None).ok()?;
    Some(
        api::auth::oauth_revoke_token(&client, &refresh)
            .await
            .is_ok(),
    )
}

/// Shared tail of the `signout` paths: drop the profile's stored credentials
/// and render the outcome. `server_signout_completed: false` means local
/// credentials were cleared without a completed server-side sign-out (the
/// credential was already dead or expired); a stderr warning says so.
fn clear_and_render(
    ctx: &CommandContext<'_>,
    server_signout_completed: bool,
    refresh_revoked: Option<bool>,
) -> Result<()> {
    let mut creds_file =
        CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
    creds_file
        .remove(ctx.profile_name, ctx.config_dir)
        .context("failed to clear credentials")?;

    if !server_signout_completed {
        eprintln!(
            "warning: this profile's credentials were already invalid (revoked, lapsed, or \
             expired); local credentials were cleared without a server-side sign-out"
        );
    }

    if refresh_revoked == Some(false) {
        eprintln!(
            "warning: the OAuth refresh token could not be revoked server-side; local \
             credentials were still cleared. Revoke the session from `fastio auth oauth-sessions`."
        );
    }

    let mut value = json!({
        "status": "signed_out",
        "profile": ctx.profile_name,
        "server_signout_completed": server_signout_completed,
    });
    // Emitted only when there WAS a refresh token, so the common shape is
    // unchanged for API-key and password profiles.
    if let Some(revoked) = refresh_revoked {
        value["oauth_refresh_revoked"] = json!(revoked);
    }
    ctx.output.render(&value)?;
    Ok(())
}

/// Map a `check_token` failure to the `reason` (and optional `message`) that
/// `auth status` reports.
///
/// **Do NOT collapse everything that is not `10587` into `token_invalid`.**
/// Several 401s mean the credential is VALID and merely not permitted for this
/// call, and reporting those as invalid is a lie the user acts on. `10175` in
/// particular is exactly what a `twofactor`-scoped token receives before 2FA is
/// completed, so classifying it as invalid tells a mid-2FA user their token is
/// dead and steers them at a re-login that spends another counted attempt.
///
/// This is one of several paths that could classify on HTTP status (or on a
/// single code) *before* [`ApiError::suggestion`] is ever consulted. Extracted
/// from the command body so it is unit-testable — the mapping has no coverage
/// while it lives inline.
fn status_reason(err: &CliError) -> (&'static str, Option<&'static str>) {
    match err {
        CliError::Api(api) if api.code == 10_587 => (
            "account_not_validated",
            Some(
                "Account email has not been verified. Run `fastio auth verify` to resend the verification email.",
            ),
        ),
        CliError::Api(api) if api.code == 10_175 => (
            "token_scope_insufficient",
            Some(
                "This credential is valid but its scope does not cover this check. If you signed in to a 2FA-enabled account, finish verifying: `fastio auth 2fa verify --code <CODE>`.",
            ),
        ),
        _ => ("token_invalid", None),
    }
}

/// Codes that PROVE the presented bearer is dead — the only ones that may cause
/// [`clear_and_render`] to delete a stored credential.
///
/// **Measured 2026-08-24**, not assumed:
/// - `10011` — a genuinely REVOKED key. Minted a key, confirmed it worked
///   (HTTP 200), deleted it, called again: `401` / `10011`.
/// - `10001` — a malformed / unparseable bearer (garbage token), measured
///   earlier the same way.
///
/// Both mean "there is no live credential here", which is the only justification
/// for destroying local state.
const DEAD_BEARER_CODES: &[u32] = &[10_001, 10_011];

/// True when the API proved the bearer itself is dead — the gate on `signout`'s
/// destructive local clear.
///
/// # This is a POSITIVE allowlist, and that inversion is the point
///
/// It used to be a NEGATIVE exclusion list: *every* HTTP 401 meant "dead session,
/// wipe the stored credential" unless a code was explicitly excluded. Three codes
/// had been excluded one at a time, each after someone noticed a specific
/// misfire — `10545`, `115069`, then `10175`.
///
/// **The structural problem: that list can only ever be as complete as the
/// failures already discovered.** A concrete path — a stored agent profile
/// calling `POST /user/auth/sign-out/` gets `401` / `261844` from the
/// containment gate, sails past every exclusion, and the client **deletes a
/// still-live key while telling the user it was revoked or expired.** A fourth
/// exclusion would fix that one case and leave the class open.
///
/// Inverted, the failure direction flips: an unknown or bodyless 401 now
/// PRESERVES the credential and surfaces the error, instead of destroying state
/// on a code nobody has classified yet. For an irreversible local delete, being
/// wrong in the direction of "keep it and tell the user" costs a confused
/// message; being wrong the other way costs the credential.
///
/// Consequence worth stating: a genuinely dead session whose code is NOT in
/// [`DEAD_BEARER_CODES`] no longer auto-clears — the user is shown the real
/// error and can run `fastio auth logout`. That is the intended trade.
///
/// The status check stays: `10175` moves to HTTP 403, where this predicate
/// cannot match anyway. Stranding local state there is the DESIRED behaviour,
/// not a second failure mode.
fn dead_session_401(err: &CliError) -> bool {
    matches!(
        err,
        CliError::Api(e) if e.http_status == 401 && DEAD_BEARER_CODES.contains(&e.code)
    )
}

/// True when the active bearer was resolved from the profile store — i.e. NOT
/// supplied via `--token` or the `FASTIO_TOKEN` / `FASTIO_API_KEY` env vars.
/// Mirrors `token::resolve_token` precedence exactly (empty strings are
/// ignored, same as resolution). Gates `signout`'s clear-on-401 path: a dead
/// flag/env token must never wipe stored credentials it did not come from.
fn bearer_from_store(
    flag_token: Option<&str>,
    env_token: Option<&str>,
    env_key: Option<&str>,
) -> bool {
    let present = |v: Option<&str>| v.is_some_and(|s| !s.is_empty());
    !present(flag_token) && !present(env_token) && !present(env_key)
}

/// Invalidate every login session for the user (sign out everywhere), then
/// clear local credentials for the active profile.
async fn invalidate_all(ctx: &CommandContext<'_>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    let client = ApiClient::new(ctx.api_base, Some(t)).context("failed to create API client")?;

    api::auth::invalidate_all(&client)
        .await
        .context("session invalidation failed")?;

    let mut creds_file =
        CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
    creds_file
        .remove(ctx.profile_name, ctx.config_dir)
        .context("failed to clear credentials")?;

    let value = json!({
        "status": "all_sessions_invalidated",
        "profile": ctx.profile_name,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Show authentication status.
async fn status(ctx: &CommandContext<'_>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;

    let creds_file = CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
    let stored = creds_file.get(ctx.profile_name);

    let value = if let Some(t) = &resolved {
        // Try to validate the token
        let client =
            ApiClient::new(ctx.api_base, Some(t.clone())).context("failed to create API client")?;

        match api::auth::check_token(&client).await {
            Ok(check) => {
                json!({
                    "authenticated": true,
                    "user_id": check.id,
                    "email": stored.and_then(|s| s.email.clone()),
                    "auth_method": stored.and_then(|s| s.auth_method.clone()),
                    "expires_at": stored.and_then(|s| s.expires_at),
                    "expired": stored.and_then(|s| s.expires_at).map(|t| token::is_expired(Some(t))),
                    "profile": ctx.profile_name,
                })
            }
            Err(ref e) => {
                let (reason, message) = status_reason(e);
                let mut obj = json!({
                    "authenticated": false,
                    "reason": reason,
                    "profile": ctx.profile_name,
                });
                if let Some(msg) = message
                    && let Some(map) = obj.as_object_mut()
                {
                    map.insert("message".to_owned(), json!(msg));
                    // Still include stored email for convenience
                    if let Some(email) = stored.and_then(|s| s.email.clone()) {
                        map.insert("email".to_owned(), json!(email));
                    }
                }
                obj
            }
        }
    } else {
        json!({
            "authenticated": false,
            "reason": "no_credentials",
            "profile": ctx.profile_name,
        })
    };

    ctx.output.render(&value)?;
    Ok(())
}

/// Create a new user account.
async fn signup(
    _config: &Config,
    ctx: &CommandContext<'_>,
    email: &str,
    password: &str,
    first_name: Option<&str>,
    last_name: Option<&str>,
    agent: bool,
) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;

    api::auth::sign_up(&client, email, password, first_name, last_name, agent)
        .await
        .context("signup failed")?;

    // Auto-login after signup.
    //
    // The failure is CAPTURED, not discarded. Reporting only "Auto-login
    // failed; run: fastio auth login" would throw away the server's reason and,
    // if the failure was a wrong-credentials or lockout response, point the
    // user at the one action that spends another counted attempt. Same class as
    // the `10008` hint in `error.rs`.
    let auto_login = api::auth::sign_in(&client, email, password).await;
    let login_failure = match &auto_login {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    };
    if let Ok(login) = auto_login {
        let now = chrono::Utc::now().timestamp();
        let creds = StoredCredentials {
            token: Some(SecretString::from(login.auth_token)),
            refresh_token: None,
            api_key: None,
            expires_at: Some(now + login.expires_in),
            user_id: None,
            email: Some(email.to_owned()),
            auth_method: Some("basic".to_owned()),
        };

        let mut creds_file =
            CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
        creds_file
            .set(ctx.profile_name, creds, ctx.config_dir)
            .context("failed to save credentials")?;

        let value = json!({
            "status": "signed_up_and_authenticated",
            "email": email,
            "profile": ctx.profile_name,
        });
        ctx.output.render(&value)?;
    } else {
        let mut value = json!({
            "status": "signed_up",
            "message": "Account created, but the automatic sign-in did not succeed. \
                        Check the reason before retrying: repeating a failed sign-in \
                        counts toward a temporary account lockout.",
            "email": email,
        });
        // Surface the server's own reason rather than a generic "run auth login".
        if let Some(reason) = login_failure
            && let Some(map) = value.as_object_mut()
        {
            map.insert("auto_login_error".to_owned(), json!(reason));
        }
        ctx.output.render(&value)?;
    }

    Ok(())
}

/// Send or confirm email verification.
async fn verify(ctx: &CommandContext<'_>, email: &str, code: Option<&str>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let client = ApiClient::new(ctx.api_base, resolved).context("failed to create API client")?;

    api::auth::email_verify(&client, email, code)
        .await
        .context("email verification failed")?;

    let action = if code.is_some() { "verified" } else { "sent" };
    let value = json!({
        "status": action,
        "email": email,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Handle 2FA subcommands.
async fn two_fa(cmd: &TwoFaCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    let client = ApiClient::new(ctx.api_base, Some(t)).context("failed to create API client")?;

    match cmd {
        TwoFaCommand::Setup { channel } => {
            let result = api::auth::two_factor_enable(&client, channel)
                .await
                .context("2FA setup failed")?;

            let mut value = json!({
                "status": "2fa_setup_initiated",
                "channel": channel,
            });
            if let Some(uri) = result.binding_uri {
                value["binding_uri"] = json!(uri);
            }
            ctx.output.render(&value)?;
        }
        TwoFaCommand::Verify { code } => {
            let result = api::auth::two_factor_verify(&client, code)
                .await
                .context("2FA verification failed")?;

            // Update stored token with the full-scope one
            let now = chrono::Utc::now().timestamp();
            let mut creds_file =
                CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
            if let Some(existing) = creds_file.get(ctx.profile_name).cloned() {
                let updated = StoredCredentials {
                    token: Some(SecretString::from(result.auth_token)),
                    expires_at: Some(now + result.expires_in),
                    ..existing
                };
                creds_file
                    .set(ctx.profile_name, updated, ctx.config_dir)
                    .context("failed to save credentials")?;
            }

            let value = json!({
                "status": "authenticated",
                "message": "2FA verification successful",
                "expires_in": result.expires_in,
            });
            ctx.output.render(&value)?;
        }
        TwoFaCommand::Disable { token: tfa_token } => {
            api::auth::two_factor_disable(&client, tfa_token)
                .await
                .context("2FA disable failed")?;

            let value = json!({
                "status": "2fa_disabled",
            });
            ctx.output.render(&value)?;
        }
        TwoFaCommand::Status => {
            let result = api::auth::two_factor_status(&client)
                .await
                .context("2FA status check failed")?;

            let value = json!({
                "state": result.state,
                "totp": result.totp,
            });
            ctx.output.render(&value)?;
        }
        TwoFaCommand::Send { channel } => {
            let result = api::auth::two_factor_send(&client, channel)
                .await
                .context("2FA send failed")?;
            ctx.output.render(&result)?;
        }
        TwoFaCommand::VerifySetup { token: tfa_token } => {
            let result = api::auth::two_factor_verify_setup(&client, tfa_token)
                .await
                .context("2FA verify setup failed")?;
            ctx.output.render(&result)?;
        }
    }

    Ok(())
}

/// Handle API key subcommands.
async fn api_key(cmd: &ApiKeyCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    let client = ApiClient::new(ctx.api_base, Some(t)).context("failed to create API client")?;

    match cmd {
        ApiKeyCommand::Create {
            name,
            scopes,
            agent_name,
            expires,
        } => {
            let result = api::auth::api_key_create(
                &client,
                name.as_deref(),
                scopes.as_deref(),
                agent_name.as_deref(),
                expires.as_deref(),
            )
            .await
            .context("API key creation failed")?;

            let value = json!({
                "status": "created",
                "api_key": result.api_key,
            });
            ctx.output.render(&value)?;
        }
        ApiKeyCommand::List => {
            let result = api::auth::api_key_list(&client)
                .await
                .context("failed to list API keys")?;

            let keys = result.api_keys.unwrap_or_default();
            let value: Value = if keys.is_empty() {
                json!([])
            } else {
                Value::Array(keys)
            };
            ctx.output.render(&value)?;
        }
        ApiKeyCommand::Delete { key_id } => {
            api::auth::api_key_delete(&client, key_id)
                .await
                .context("API key deletion failed")?;

            let value = json!({
                "status": "deleted",
                "key_id": key_id,
            });
            ctx.output.render(&value)?;
        }
        ApiKeyCommand::Get { key_id } => {
            let result = api::auth::api_key_get(&client, key_id)
                .await
                .context("API key get failed")?;
            ctx.output.render(&result)?;
        }
        ApiKeyCommand::Update {
            key_id,
            name,
            scopes,
            agent_name,
            expires,
        } => {
            if name.is_none() && scopes.is_none() && agent_name.is_none() && expires.is_none() {
                anyhow::bail!(
                    "at least one update field is required \
                     (--name, --scopes, --agent-name, --expires)"
                );
            }
            let result = api::auth::api_key_update(
                &client,
                key_id,
                name.as_deref(),
                scopes.as_deref(),
                agent_name.as_deref(),
                expires.as_deref(),
            )
            .await
            .context("API key update failed")?;
            ctx.output.render(&result)?;
        }
    }

    Ok(())
}

/// Verify the current token is valid.
async fn check(ctx: &CommandContext<'_>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    let client = ApiClient::new(ctx.api_base, Some(t)).context("failed to create API client")?;

    let result = api::auth::check_token(&client)
        .await
        .context("token check failed")?;
    let value = json!({
        "valid": true,
        "user_id": result.id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Show session info.
async fn session(ctx: &CommandContext<'_>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    let client = ApiClient::new(ctx.api_base, Some(t)).context("failed to create API client")?;

    let value = api::auth::session_info(&client)
        .await
        .context("session info failed")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Check email availability.
async fn email_check(ctx: &CommandContext<'_>, email: &str) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let client = ApiClient::new(ctx.api_base, resolved).context("failed to create API client")?;

    let value = api::auth::email_check(&client, email)
        .await
        .context("email check failed")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Request a password reset.
async fn password_reset_request(ctx: &CommandContext<'_>, email: &str) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;
    api::auth::password_reset_request(&client, email)
        .await
        .context("password reset request failed")?;
    let value = json!({
        "status": "sent",
        "email": email,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Complete a password reset.
async fn password_reset(
    ctx: &CommandContext<'_>,
    code: &str,
    password1: &str,
    password2: &str,
) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;
    api::auth::password_reset_complete(&client, code, password1, password2)
        .await
        .context("password reset failed")?;
    let value = json!({
        "status": "reset_complete",
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Handle OAuth session subcommands.
async fn oauth(cmd: &OauthCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let resolved = token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    let client = ApiClient::new(ctx.api_base, Some(t)).context("failed to create API client")?;

    match cmd {
        OauthCommand::List => {
            let value = api::auth::oauth_list(&client)
                .await
                .context("failed to list OAuth sessions")?;
            ctx.output.render(&value)?;
        }
        OauthCommand::Details { session_id } => {
            let value = api::auth::oauth_details(&client, session_id)
                .await
                .context("failed to get OAuth session details")?;
            ctx.output.render(&value)?;
        }
        OauthCommand::Rename {
            session_id,
            device_name,
            agent_name,
        } => {
            if device_name.is_none() && agent_name.is_none() {
                anyhow::bail!("at least one of --device-name or --agent-name is required");
            }
            let value = api::auth::oauth_rename(
                &client,
                session_id,
                device_name.as_deref(),
                agent_name.as_deref(),
            )
            .await
            .context("failed to rename OAuth session")?;
            ctx.output.render(&value)?;
        }
        OauthCommand::Revoke { session_id } => {
            api::auth::oauth_revoke(&client, session_id)
                .await
                .context("failed to revoke OAuth session")?;
            let value = json!({
                "status": "revoked",
                "session_id": session_id,
            });
            ctx.output.render(&value)?;
        }
        OauthCommand::RevokeAll { exclude_current } => {
            api::auth::oauth_revoke_all(&client, exclude_current.as_deref())
                .await
                .context("failed to revoke all OAuth sessions")?;
            let value = json!({
                "status": "all_revoked",
                "excluded_session": exclude_current,
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}

/// Token scope introspection.
async fn scopes(ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::auth::scopes(&client)
        .await
        .context("token scope introspection failed")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Check password reset code validity.
async fn password_reset_check(ctx: &CommandContext<'_>, code: &str) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;
    let value = api::auth::password_reset_check(&client, code)
        .await
        .context("password reset check failed")?;
    ctx.output.render(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{bearer_from_store, dead_session_401, status_reason};
    use fastio_cli::error::{ApiError, CliError};

    fn api_err(code: u32, http_status: u16) -> CliError {
        CliError::Api(ApiError::new(code, None, "boom".to_owned(), http_status))
    }

    /// `auth status` must not report a VALID but scope-limited credential as
    /// invalid — the same misreport class as the sign-out one.
    #[test]
    fn status_does_not_report_a_valid_scoped_credential_as_invalid() {
        // `auth status` collapsed every non-`10587` failure into "token_invalid".
        // A `twofactor`-scoped token (pre-2FA) gets `10175` from `check_token`,
        // so a mid-2FA user was told their credential was dead — and the natural
        // response, re-running `auth login`, spends another counted attempt
        // toward the lockout.
        let (reason, message) = status_reason(&api_err(10_175, 401));
        assert_eq!(reason, "token_scope_insufficient");
        let msg = message.expect("a recovery message is the point of this arm");
        assert!(
            msg.contains("fastio auth 2fa verify --code <CODE>"),
            "must name the command that actually runs: {msg}"
        );
        // Unchanged behaviour for the two arms that were already correct.
        assert_eq!(
            status_reason(&api_err(10_587, 401)).0,
            "account_not_validated"
        );
        assert_eq!(status_reason(&api_err(10_011, 401)).0, "token_invalid");
        // And a genuinely dead credential still reads as invalid.
        assert_eq!(status_reason(&api_err(0, 401)).0, "token_invalid");
    }

    /// The clear-on-401 gate opens ONLY for codes measured to prove a dead
    /// bearer. Everything else — including an unknown or bodyless 401 —
    /// preserves the stored credential.
    #[test]
    fn dead_session_401_opens_only_for_measured_dead_bearer_codes() {
        // MEASURED 2026-08-24: a revoked key returns 10011; a malformed
        // bearer returns 10001. These are the only codes that may destroy local
        // state.
        assert!(dead_session_401(&api_err(10_011, 401)));
        assert!(dead_session_401(&api_err(10_001, 401)));

        // THE INVERSION — an allowlist, not a denylist. Clearing on EVERY 401
        // unless explicitly excluded means exclusions accumulate one at a time
        // after each misfire. The concrete path: a stored agent profile calling
        // sign-out gets 401/261844 from the containment gate, passes every
        // exclusion, and the client DELETES A STILL-LIVE KEY while reporting it
        // as revoked/lapsed/expired. One more exclusion would close that case
        // and leave the class open.
        assert!(!dead_session_401(&api_err(261_844, 401)));
        // A bodyless 401 is ambiguous, and ambiguity must not destroy a
        // credential. This previously cleared.
        assert!(!dead_session_401(&api_err(0, 401)));
        // `1650` is an App\Error CLASS constant; per `error.rs` the value that
        // reaches `error.code` is a per-call-site code, so this was pinning a
        // value that likely never arrives. It no longer clears either.
        assert!(!dead_session_401(&api_err(1650, 401)));

        // The previously-excluded codes stay non-clearing, now by construction
        // rather than by enumeration.
        for code in [10_545_u32, 115_069, 10_175, 10_560] {
            assert!(
                !dead_session_401(&api_err(code, 401)),
                "code {code} must never read as a dead bearer"
            );
        }

        // Status still gates: 10175 moves to 403, where this cannot match.
        assert!(!dead_session_401(&api_err(10_175, 403)));
        // Non-401 statuses and non-API errors never clear.
        assert!(!dead_session_401(&api_err(10_011, 500)));
        assert!(!dead_session_401(&CliError::RateLimit {
            retry_after_secs: 30
        }));
        assert!(!dead_session_401(&CliError::Parse("html 401".to_owned())));
    }

    /// The bearer counts as profile-store-sourced only when neither the flag
    /// nor either env var supplies one — empty strings are ignored exactly as
    /// `token::resolve_token` ignores them.
    #[test]
    fn bearer_from_store_mirrors_resolution_precedence() {
        assert!(bearer_from_store(None, None, None));
        assert!(bearer_from_store(Some(""), Some(""), Some("")));
        assert!(!bearer_from_store(Some("t"), None, None));
        assert!(!bearer_from_store(None, Some("t"), None));
        assert!(!bearer_from_store(None, None, Some("k")));
        assert!(!bearer_from_store(Some(""), Some("t"), None));
    }
}
