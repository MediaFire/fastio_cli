/// Auth command implementations for `fastio auth *`.
///
/// Handles login (basic + PKCE), logout, status, signup, email
/// verification, 2FA management, and API key management.
use anyhow::{Context, Result};
use colored::Colorize;
use secrecy::SecretString;
use serde_json::{Value, json};

use fastio_cli::api;
use fastio_cli::api::auth::ApiKeyScopeSpec;
use fastio_cli::auth::credentials::{CredentialsFile, StoredCredentials};
use fastio_cli::auth::pkce;
use fastio_cli::auth::token;
use fastio_cli::client::ApiClient;
use fastio_cli::config::Config;
use fastio_cli::error::CliError;
use fastio_cli::output::OutputFormat;

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
            access,
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
                // Same class of refusal: the access-mode ceiling and the
                // account-settings request are asked for at the consent page,
                // which basic auth never reaches. Accepting them here would
                // hand back a credential the operator believes is scoped and
                // is not.
                if let Some(flag) = access.first_unsupported_by_basic_auth() {
                    anyhow::bail!(
                        "{flag} applies to browser (PKCE) login only; basic auth cannot \
                         reach the consent page that grants it. Omit --email/--password \
                         to use browser login, or scope an API key with \
                         `fastio auth api-key create`."
                    )
                }
                login_basic(config, ctx, email, password).await
            } else if password.is_some() {
                anyhow::bail!(
                    "--password requires --email. Provide both for direct login, \
                     or omit both for browser login."
                )
            } else {
                login_pkce(
                    config,
                    ctx,
                    email.as_deref(),
                    agent_name.as_deref(),
                    *access,
                )
                .await
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

/// Access-mode ceiling requested at login, as asked for by `--admin`,
/// `--read-only` and `--account-settings`.
///
/// A ceiling, not a grant: the consent page may narrow what is actually
/// issued, which is why the PKCE path re-reads the granted set afterwards.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoginAccess {
    /// Ask for an admin (`rwa`) access mode.
    pub admin: bool,
    /// Ask for a read-only (`r`) access mode.
    pub read_only: bool,
    /// Also ask for account settings changes (`userdetails:*:rw`).
    pub account_settings: bool,
}

impl LoginAccess {
    /// The first flag set here that basic auth cannot honour, spelled as the
    /// user typed it — `None` when nothing was requested.
    #[must_use]
    pub fn first_unsupported_by_basic_auth(self) -> Option<&'static str> {
        if self.admin {
            Some("--admin")
        } else if self.read_only {
            Some("--read-only")
        } else if self.account_settings {
            Some("--account-settings")
        } else {
            None
        }
    }

    /// The wire form for the PKCE initiate call. `--admin` and `--read-only`
    /// are mutually exclusive at the clap layer, so at most one can be set.
    #[must_use]
    fn to_authorize_access(self) -> api::auth::AuthorizeAccess<'static> {
        let access_mode = if self.admin {
            Some("rwa")
        } else if self.read_only {
            Some("r")
        } else {
            None
        };
        api::auth::AuthorizeAccess {
            access_mode,
            account_settings: self.account_settings,
        }
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
        /// Access-mode ceiling requested at the consent page (PKCE only).
        access: LoginAccess,
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
        /// Structured scope selectors (mutually exclusive with `scopes`).
        scope_spec: ApiKeyScopeSpec,
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
        /// Structured scope selectors (mutually exclusive with `scopes`).
        scope_spec: ApiKeyScopeSpec,
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
    /// Narrow a session's granted scopes (narrower-or-equal only).
    Narrow {
        /// Session ID.
        session_id: String,
        /// Replacement scopes (JSON array string).
        scopes: Option<String>,
        /// Structured scope selectors (mutually exclusive with `scopes`).
        scope_spec: ApiKeyScopeSpec,
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
        // Basic auth never reaches the consent page, so there is no granted
        // entity list to record.
        scopes: None,
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
    access: LoginAccess,
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
        access.to_authorize_access(),
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
        // The granted entity array, normalized to the server's string form by
        // `deserialize_optional_scopes`: a string arrives as received, an array
        // is re-encoded to the same rendering, and a blank one reads as absent
        // rather than erasing a real grant.
        scopes: token_resp.scopes.clone(),
    };

    let mut creds_file =
        CredentialsFile::load(ctx.config_dir).context("failed to load credentials")?;
    creds_file
        .set(ctx.profile_name, creds, ctx.config_dir)
        .context("failed to save credentials")?;

    // `--admin` asks for a CEILING; the consent page decides. A login that
    // silently came back without admin would surface much later as a 403 on
    // the first admin operation, so confirm it once, here, while the user is
    // still looking. Never fatal — the login itself succeeded.
    // `--quiet` suppresses the WARNING, never the check. Quiet is an OUTPUT
    // flag: it must change what the command prints and nothing else, so the
    // introspection runs identically in both modes and only the printing is
    // conditional. It leaves no record either way — under `--quiet` the
    // refusal simply is not reported.
    if access.admin
        // Nothing in this block may fail the command: the credential is
        // already stored and works, so a client that will not build here is
        // one more reason to stay silent, never to exit non-zero.
        && let Ok(confirm_client) =
            ApiClient::new(ctx.api_base, Some(token_resp.access_token.clone()))
        && let Some(warning) = admin_grant_notice(&confirm_client, ctx.output.quiet).await
    {
        eprintln!("{} {warning}", "warning:".yellow().bold());
    }

    let value = json!({
        "status": "authenticated",
        "auth_method": "pkce",
        "expires_in": token_resp.expires_in,
        "profile": ctx.profile_name,
    });

    ctx.output.render(&value)?;
    Ok(())
}

/// The post-login admin confirmation, with `--quiet` applied to the OUTPUT
/// only.
///
/// The introspection call runs either way so that the login's side effects are
/// the SAME in both modes: `--quiet` then changes only what is printed, never
/// what the command does. Branching on it would make the quiet path a
/// different command that happens to share a name, and the one visible
/// difference — the warning — is suppressed here rather than upstream.
async fn admin_grant_notice(client: &ApiClient, quiet: bool) -> Option<String> {
    let warning = admin_grant_warning(client).await;
    if quiet { None } else { warning }
}

/// Read back what the consent page actually granted, returning the warning
/// text when `--admin` was asked for and not given.
///
/// Returns `None` when admin is confirmed. Never returns an error: the
/// credential is already stored and works, so a failed introspection is worth
/// a softer word, not a non-zero exit.
async fn admin_grant_warning(client: &ApiClient) -> Option<String> {
    match api::auth::scopes(client).await {
        // Only an explicit `admin: true` confirms the ceiling. A missing or
        // wrong-typed field is UNCONFIRMED, not confirmed: reading it as "fine"
        // would report success for a login that never got admin.
        Ok(body) => match body.get("admin").and_then(Value::as_bool) {
            Some(true) => None,
            Some(false) => Some(
                "admin access was requested but the consent page did not grant it. \
                 Run `fastio auth scopes` to see what this login actually holds."
                    .to_owned(),
            ),
            None => Some(
                "admin access was requested but could not be confirmed (the scope \
                 introspection did not report it). \
                 Run `fastio auth scopes` to see what this login actually holds."
                    .to_owned(),
            ),
        },
        Err(e) => Some(format!(
            "admin access was requested but could not be confirmed ({e}). \
             Run `fastio auth scopes` to see what this login actually holds."
        )),
    }
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

/// Does the profile's stored grant describe the credential `auth status`
/// actually reported on?
///
/// [`bearer_from_store`] rules out a `--token` / env bearer, but two cases
/// survive it and both would attribute one credential's authority to another:
///
/// - **Profile fallback.** `--profile x` where `x` holds metadata but no usable
///   token falls back to the DEFAULT profile's token
///   ([`token::resolve_token`]), while `status` reads profile `x`. Reporting
///   `x`'s scopes next to the default profile's bearer describes a credential
///   that was never used.
/// - **No bearer at all.** With nothing to resolve, the payload is
///   `authenticated: false`; a `scopes` key there claims a grant for a
///   credential the command could not even find.
///
/// So the grant is reported only when the profile itself supplied the bearer.
fn stored_scopes_describe_bearer(
    from_store: bool,
    resolved: Option<&str>,
    stored: Option<&StoredCredentials>,
) -> bool {
    from_store
        && resolved.is_some()
        && stored.is_some_and(|s| s.expose_api_key().is_some() || s.expose_token().is_some())
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

    let mut value = value;
    apply_stored_scopes(
        &mut value,
        stored_scopes_describe_bearer(
            bearer_from_store(
                ctx.flag_token,
                std::env::var("FASTIO_TOKEN").ok().as_deref(),
                std::env::var("FASTIO_API_KEY").ok().as_deref(),
            ),
            resolved.as_deref(),
            stored,
        ),
        stored,
    );

    ctx.output.render(&value)?;
    Ok(())
}

/// Attach the stored profile's granted scopes to an `auth status` payload —
/// but ONLY when the bearer actually came from that profile.
///
/// A `--token` / `FASTIO_TOKEN` / `FASTIO_API_KEY` bearer is a different
/// credential from whatever the profile happens to hold, so reporting the
/// profile's scopes alongside it would attribute one credential's authority to
/// another. Those calls omit the key entirely. When the profile IS the bearer
/// and it has no recorded grant (a basic-auth or pre-upgrade profile), the key
/// is present as `null` — absent means "not this credential's", `null` means
/// "this credential, nothing recorded".
fn apply_stored_scopes(value: &mut Value, from_store: bool, stored: Option<&StoredCredentials>) {
    let Some(stored) = stored.filter(|_| from_store) else {
        return;
    };
    if let Some(map) = value.as_object_mut() {
        map.insert("scopes".to_owned(), json!(stored.scopes));
    }
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
            // Auto-login after signup is basic auth: no consent page, no
            // granted entity list.
            scopes: None,
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
            scope_spec,
        } => {
            // Raw `--scopes` and the structured selectors are mutually
            // exclusive at the clap layer, so at most one of these is `Some`.
            let resolved = api::auth::resolve_key_scopes(scope_spec)
                .context("could not build the requested scopes")?;
            let scopes = scopes.as_deref().or(resolved.as_deref());
            let result = api::auth::api_key_create(
                &client,
                name.as_deref(),
                scopes,
                agent_name.as_deref(),
                expires.as_deref(),
            )
            .await
            .context("API key creation failed")?;

            ctx.output.render(&api_key_create_value(&result))?;
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
            scope_spec,
        } => {
            // An update REPLACES the scope set wholesale, so a half-specified
            // request must not reach the server — and the recovery is always
            // the same: look at what the key holds now, then send the whole
            // intended set.
            let resolved = api::auth::resolve_key_scopes(scope_spec).with_context(|| {
                format!(
                    "could not build the requested scopes. An update replaces the key's \
                     scopes wholesale — run `fastio auth api-key get {key_id}` to see the \
                     current set before replacing it"
                )
            })?;
            // `resolved.is_none()` is exactly "no structured selector was
            // given", so it doubles as the emptiness check for the guard.
            if name.is_none()
                && scopes.is_none()
                && agent_name.is_none()
                && expires.is_none()
                && resolved.is_none()
            {
                anyhow::bail!(
                    "at least one update field is required \
                     (--name, --scopes, --agent-name, --expires, or \
                     --org/--workspace/--share/--all/--account-settings)"
                );
            }
            let result = api::auth::api_key_update(
                &client,
                key_id,
                name.as_deref(),
                scopes.as_deref().or(resolved.as_deref()),
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

/// Render the whole key object the server returned on creation.
///
/// `POST /user/auth/key/` has no `response` sub-object, so the envelope's own
/// `result` flag arrives alongside the key fields — it is bookkeeping, not part
/// of the key, and is dropped here. The raw `api_key` is shown ONCE: this is
/// the only time the server ever returns it in full.
fn api_key_create_value(result: &api::types::ApiKeyCreateResponse) -> Value {
    let mut obj = serde_json::Map::new();
    for (key, val) in &result.extra {
        if key == "result" {
            continue;
        }
        obj.insert(key.clone(), val.clone());
    }
    // Last so the CLI's own fields always win over a same-named server field.
    obj.insert("status".to_owned(), json!("created"));
    obj.insert("api_key".to_owned(), json!(result.api_key));
    Value::Object(obj)
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
        OauthCommand::Narrow {
            session_id,
            scopes,
            scope_spec,
        } => {
            let resolved = api::auth::resolve_key_scopes(scope_spec)
                .context("could not build the requested scopes")?;
            // A blank `--scopes ""` is not a narrowing request: forwarding it
            // would send `scopes=` and ask the server to interpret an empty
            // grant. Treat it as absent so the bail below explains what to
            // supply.
            let raw = scopes.as_deref().filter(|s| !s.trim().is_empty());
            let Some(scopes_json) = raw.or(resolved.as_deref()) else {
                anyhow::bail!(
                    "nothing to narrow to: supply --scopes <json>, or the structured \
                     selectors (--org/--workspace/--share/--all/--account-settings)"
                );
            };
            let value = api::auth::oauth_narrow(&client, session_id, scopes_json)
                .await
                .context("failed to narrow OAuth session scopes")?;
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
    // Row-shaping is a RENDERING concern for the two tabular formats only.
    // `json` and `markdown` stay byte-identical to the server's own payload.
    let value = match ctx.output.format {
        OutputFormat::Table | OutputFormat::Csv => scopes_rows_for_table(&value),
        // Any format added later renders the server payload untouched, which
        // is the safe default: only the tabular renderers need rows.
        _ => value,
    };
    ctx.output.render(&value)?;
    Ok(())
}

/// Reshape an `/auth/scopes/` payload into one row per scope for table/CSV.
///
/// The tabular renderers flatten an envelope to its first non-empty array, so
/// an introspection response renders as a bare list of scope strings with none
/// of the surrounding context — and, for a credential whose only array is
/// empty, as nothing at all. Rows come from `scopes_detail` when it carries
/// OBJECT entries (it is the hydrated form, with entity names), plus a
/// `{"scope": …}` row for every plain `scopes` string those objects do not
/// account for — so a mixed or partial `scopes_detail` cannot make a granted
/// scope vanish from the output. Each row then carries every credential-level
/// scalar so a single row is self-describing.
///
/// A payload with no scopes at all is returned unchanged — an unscoped
/// credential has no rows to show, and the top-level object is the answer.
/// The `entity_type:entity_id:access_mode` string one hydrated `scopes_detail`
/// entry stands for, so it can be matched against the plain `scopes` list.
///
/// Prefers an explicit `scope` field when the entry carries one; otherwise
/// rebuilds it from the three hydrated fields. `None` means the entry names no
/// recognizable scope, so it can account for nothing.
fn detail_scope_string(entry: &Value) -> Option<String> {
    if let Some(scope) = entry.get("scope").and_then(Value::as_str) {
        return Some(scope.to_owned());
    }
    let entity_type = entry.get("entity_type").and_then(Value::as_str)?;
    let entity_id = entry.get("entity_id").and_then(Value::as_str)?;
    let access_mode = entry.get("access_mode").and_then(Value::as_str)?;
    Some(format!("{entity_type}:{entity_id}:{access_mode}"))
}

fn scopes_rows_for_table(body: &Value) -> Value {
    let Some(map) = body.as_object() else {
        return body.clone();
    };

    // `scopes_detail` is a richer view only where it carries OBJECTS. A
    // non-empty array of plain strings (or the string half of a mixed one)
    // yields nothing a row can be built from, so the object entries are
    // selected first and the plain-`scopes` path covers the rest.
    let detail: Vec<Value> = map
        .get("scopes_detail")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter(|e| e.is_object()).cloned().collect())
        .unwrap_or_default();

    let scope_strings: Vec<&str> = map
        .get("scopes")
        .and_then(Value::as_array)
        .map(|scopes| scopes.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    // A string in `scopes` that no detail object accounts for still gets a row:
    // filtering to objects must never make a scope disappear from the very
    // command the docs tell agents to run to confirm what they got. A detail
    // entry accounts for a string either by naming it in its own `scope` field
    // or by rebuilding it from `entity_type:entity_id:access_mode`, the shape
    // the hydrated form uses. When no entry yields either — an unrecognized
    // detail shape — the entries are taken to account for the first
    // `detail.len()` strings by position.
    let named: Vec<String> = detail.iter().filter_map(detail_scope_string).collect();
    let uncovered: Vec<&str> = if named.is_empty() {
        scope_strings.iter().skip(detail.len()).copied().collect()
    } else {
        scope_strings
            .iter()
            .filter(|s| !named.iter().any(|n| n == *s))
            .copied()
            .collect()
    };

    let mut rows: Vec<Value> = detail;
    rows.extend(uncovered.into_iter().map(|s| json!({ "scope": s })));

    if rows.is_empty() {
        return body.clone();
    }

    for row in &mut rows {
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        // EVERY top-level scalar is a credential-level fact, carried onto the
        // row generically: a fixed list silently drops whatever the server adds
        // next (`expires`, `is_agent`, `agent_name` were all lost that way).
        // Arrays and objects are the rows' own source, and `result` is envelope
        // bookkeeping.
        for (key, val) in map {
            if key == "result" || val.is_array() || val.is_object() {
                continue;
            }
            if obj.contains_key(key.as_str()) {
                // A per-row key describes THAT scope; the credential-level
                // field of the same name is a different fact. Both are kept,
                // under distinct keys, so neither is shadowed. The rule is
                // generic for the same reason the copy above is: naming the
                // colliding keys would silently drop whatever the server adds
                // next.
                obj.insert(format!("credential_{key}"), val.clone());
            } else {
                obj.insert(key.clone(), val.clone());
            }
        }
    }
    Value::Array(rows)
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

#[cfg(test)]
mod admin_scope_tests {
    use super::{
        AuthCommand, CommandContext, LoginAccess, admin_grant_warning, api_key_create_value,
        apply_stored_scopes, execute, scopes_rows_for_table,
    };
    use fastio_cli::api::types::ApiKeyCreateResponse;
    use fastio_cli::auth::credentials::StoredCredentials;
    use fastio_cli::client::ApiClient;
    use fastio_cli::config::Config;
    use fastio_cli::output::OutputConfig;
    use fastio_cli::output::format::filter_fields;
    use serde_json::{Value, json};

    /// A loopback address with nothing listening on it: bound to claim a free
    /// port, then released.
    fn closed_loopback_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);
        addr
    }

    /// Serve exactly one canned JSON response on a loopback port.
    async fn spawn_json_server(status_line: &'static str, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let header = format!(
                    "{status_line}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        addr
    }

    fn client_for(addr: &str) -> ApiClient {
        ApiClient::new(&format!("http://{addr}"), Some("token".to_owned())).expect("client builds")
    }

    // ─── the basic-auth refusal ────────────────────────────────────────────

    /// The access-mode ceiling and the account-settings request are granted at
    /// the consent page, which basic auth never reaches. Accepting them there
    /// would hand back a credential the operator believes is scoped and is not
    /// — the same failure the `--agent-name` refusal exists to prevent.
    #[tokio::test]
    async fn basic_auth_refuses_each_consent_page_flag() {
        let dir = std::env::temp_dir().join(format!("fastio-admin-basic-{}", std::process::id()));
        let output = OutputConfig::from_flags(Some("json"), None, true, true);
        let base = format!("http://{}", closed_loopback_addr());
        let ctx = CommandContext {
            output: &output,
            profile_name: "default",
            api_base: &base,
            flag_token: None,
            config_dir: &dir,
        };
        let config = Config::default();

        for (flag, access) in [
            (
                "--admin",
                LoginAccess {
                    admin: true,
                    ..LoginAccess::default()
                },
            ),
            (
                "--read-only",
                LoginAccess {
                    read_only: true,
                    ..LoginAccess::default()
                },
            ),
            (
                "--account-settings",
                LoginAccess {
                    account_settings: true,
                    ..LoginAccess::default()
                },
            ),
        ] {
            let cmd = AuthCommand::Login {
                email: Some("someone@example.com".to_owned()),
                password: Some("pw".to_owned()),
                agent_name: None,
                access,
            };
            let err = execute(&cmd, &config, &ctx)
                .await
                .expect_err("basic auth must refuse a consent-page flag");
            let msg = format!("{err:#}");
            assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
            assert!(
                msg.contains("browser (PKCE) login only"),
                "the refusal must point at browser login: {msg}"
            );
        }
    }

    /// POSITIVE CONTROL for the refusal above: `--email` WITHOUT `--password`
    /// is still browser login, so the same flags must sail through to the
    /// initiate call. Reaching the network at all is the proof — the refusal
    /// short-circuits before any client is built.
    #[tokio::test]
    async fn email_only_login_accepts_the_consent_page_flags() {
        let dir = std::env::temp_dir().join(format!("fastio-admin-pkce-{}", std::process::id()));
        let output = OutputConfig::from_flags(Some("json"), None, true, true);
        let addr = spawn_json_server(
            "HTTP/1.1 500 Internal Server Error",
            r#"{"result":false,"error":{"code":1,"text":"nope"}}"#,
        )
        .await;
        let base = format!("http://{addr}");
        let ctx = CommandContext {
            output: &output,
            profile_name: "default",
            api_base: &base,
            flag_token: None,
            config_dir: &dir,
        };
        let cmd = AuthCommand::Login {
            email: Some("someone@example.com".to_owned()),
            password: None,
            agent_name: None,
            access: LoginAccess {
                admin: true,
                account_settings: true,
                ..LoginAccess::default()
            },
        };
        let err = execute(&cmd, &Config::default(), &ctx)
            .await
            .expect_err("the canned 500 fails the initiate call");
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("applies to browser (PKCE) login only"),
            "browser login must not refuse its own flags: {msg}"
        );
        assert!(
            msg.contains("failed to initiate PKCE authorization"),
            "the flags must have been carried into the initiate call: {msg}"
        );
    }

    /// The ceiling is a request for an access MODE, and the two mode flags map
    /// to the two non-default modes. `--account-settings` is orthogonal: it
    /// never sets a mode of its own.
    #[test]
    fn login_access_maps_to_the_wire_access_mode() {
        let none = LoginAccess::default().to_authorize_access();
        assert_eq!(none.access_mode, None);
        assert!(!none.account_settings);

        let admin = LoginAccess {
            admin: true,
            ..LoginAccess::default()
        }
        .to_authorize_access();
        assert_eq!(admin.access_mode, Some("rwa"));

        let read_only = LoginAccess {
            read_only: true,
            ..LoginAccess::default()
        }
        .to_authorize_access();
        assert_eq!(read_only.access_mode, Some("r"));

        let settings = LoginAccess {
            account_settings: true,
            ..LoginAccess::default()
        }
        .to_authorize_access();
        assert_eq!(settings.access_mode, None);
        assert!(settings.account_settings);
    }

    /// Nothing requested means nothing to refuse — otherwise every basic-auth
    /// login would start failing.
    #[test]
    fn nothing_requested_is_never_refused() {
        assert_eq!(
            LoginAccess::default().first_unsupported_by_basic_auth(),
            None
        );
        for (expected, access) in [
            (
                "--admin",
                LoginAccess {
                    admin: true,
                    ..LoginAccess::default()
                },
            ),
            (
                "--read-only",
                LoginAccess {
                    read_only: true,
                    ..LoginAccess::default()
                },
            ),
            (
                "--account-settings",
                LoginAccess {
                    account_settings: true,
                    ..LoginAccess::default()
                },
            ),
        ] {
            assert_eq!(
                access.first_unsupported_by_basic_auth(),
                Some(expected),
                "{expected} must be named back to the user"
            );
        }
    }

    // ─── the post-login admin confirmation ─────────────────────────────────

    /// `--admin` asks for a ceiling the consent page may decline. A login that
    /// silently came back without admin surfaces much later as a 403 on the
    /// first admin operation, so the mismatch is reported while the user is
    /// still looking.
    #[tokio::test]
    async fn admin_grant_warning_fires_when_admin_was_not_granted() {
        let addr = spawn_json_server(
            "HTTP/1.1 200 OK",
            r#"{"result":true,"auth_type":"jwt_v2","admin":false,"full_access":false,
                "scopes":["org:1:rw"]}"#,
        )
        .await;
        let warning = admin_grant_warning(&client_for(&addr))
            .await
            .expect("a declined admin request must warn");
        assert!(warning.contains("did not grant"), "{warning}");
        assert!(
            warning.contains("fastio auth scopes"),
            "the warning must name the inspection command: {warning}"
        );
    }

    /// POSITIVE CONTROL: a granted request is silent. Warning on every
    /// `--admin` login would train the user to ignore the line.
    #[tokio::test]
    async fn admin_grant_warning_is_silent_when_admin_was_granted() {
        let addr = spawn_json_server(
            "HTTP/1.1 200 OK",
            r#"{"result":true,"auth_type":"jwt_v2","admin":true,"full_access":false,
                "scopes":["org:1:rwa"]}"#,
        )
        .await;
        assert!(
            admin_grant_warning(&client_for(&addr)).await.is_none(),
            "a granted admin request must stay quiet"
        );
    }

    /// A failed introspection is not a failed login: the credential is already
    /// stored and works. The softer wording says admin could not be CONFIRMED,
    /// never that it was refused — and the function cannot return an error at
    /// all, so the login's exit code stays 0 by construction.
    #[tokio::test]
    async fn admin_grant_warning_softens_when_introspection_fails() {
        let addr = spawn_json_server(
            "HTTP/1.1 500 Internal Server Error",
            r#"{"result":false,"error":{"code":1,"text":"boom"}}"#,
        )
        .await;
        let warning = admin_grant_warning(&client_for(&addr))
            .await
            .expect("an unreadable grant is still worth a word");
        assert!(warning.contains("could not be confirmed"), "{warning}");
        assert!(!warning.contains("did not grant"), "{warning}");
        assert!(warning.contains("fastio auth scopes"), "{warning}");
    }

    /// `--quiet` suppresses the WARNING, never the check.
    ///
    /// The introspection is the one call that makes the requested ceiling
    /// verifiable; skipping it under `--quiet` would leave a scripted
    /// `--admin` login with no record that admin was refused. So the request
    /// must still reach the server, and only the printed line disappears.
    #[tokio::test]
    async fn admin_grant_notice_still_introspects_under_quiet() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 4096];
                if sock.read(&mut buf).await.is_ok() {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                let body = br#"{"result":true,"auth_type":"jwt_v2","admin":false,"scopes":[]}"#;
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

        let quiet = super::admin_grant_notice(&client_for(&addr), true).await;
        assert!(quiet.is_none(), "quiet mode prints nothing: {quiet:?}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the scope introspection must still run under --quiet"
        );
    }

    /// The negative control: without `--quiet` the same refusal is reported.
    #[tokio::test]
    async fn admin_grant_notice_reports_the_refusal_when_not_quiet() {
        let addr = spawn_json_server(
            "HTTP/1.1 200 OK",
            r#"{"result":true,"auth_type":"jwt_v2","admin":false,"scopes":[]}"#,
        )
        .await;
        let warning = super::admin_grant_notice(&client_for(&addr), false)
            .await
            .expect("a declined admin request must warn");
        assert!(warning.contains("did not grant"), "{warning}");
    }

    /// Only an explicit `admin: true` confirms the ceiling. A response with no
    /// top-level `admin` field has not confirmed anything, and reading that
    /// silence as confirmation is how a login that never got admin reports
    /// success.
    #[tokio::test]
    async fn admin_grant_warning_fires_when_the_grant_is_not_reported() {
        let addr = spawn_json_server(
            "HTTP/1.1 200 OK",
            r#"{"result":true,"auth_type":"jwt_v2","full_access":false,
                "scopes":["org:1:rw"]}"#,
        )
        .await;
        let warning = admin_grant_warning(&client_for(&addr))
            .await
            .expect("an unreported grant must warn");
        assert!(warning.contains("could not be confirmed"), "{warning}");
        assert!(
            !warning.contains("did not grant"),
            "silence is not a refusal: {warning}"
        );
    }

    // ─── `auth oauth narrow` input guards ──────────────────────────────────

    /// `--scopes ""` is not a narrowing request. Forwarding it would send
    /// `scopes=` and leave the server to interpret an empty grant, so it must
    /// fall into the same bail as supplying nothing at all.
    #[tokio::test]
    async fn oauth_narrow_treats_blank_scopes_as_nothing_to_narrow_to() {
        use fastio_cli::auth::credentials::CredentialsFile;
        use secrecy::SecretString;

        let dir = std::env::temp_dir().join(format!(
            "fastio-cli-narrow-blank-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("create config dir");
        let mut file = CredentialsFile::load(&dir).expect("load credentials");
        file.set(
            "default",
            StoredCredentials {
                token: Some(SecretString::from("stored-token")),
                ..StoredCredentials::default()
            },
            &dir,
        )
        .expect("write default profile");

        let output = OutputConfig::from_flags(Some("json"), None, true, true);
        let base = format!("http://{}", closed_loopback_addr());
        let ctx = CommandContext {
            output: &output,
            profile_name: "default",
            api_base: &base,
            flag_token: None,
            config_dir: &dir,
        };
        let cmd = AuthCommand::Oauth(super::OauthCommand::Narrow {
            session_id: "s1".to_owned(),
            scopes: Some("   ".to_owned()),
            scope_spec: fastio_cli::api::auth::ApiKeyScopeSpec::default(),
        });
        let err = execute(&cmd, &Config::default(), &ctx)
            .await
            .expect_err("a blank --scopes must not reach the server");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nothing to narrow to"),
            "a blank value must bail like an absent one, got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── `auth status` scope attribution ───────────────────────────────────

    /// The profile's grant describes the PROFILE'S credential. Reporting it
    /// next to a `--token` / env bearer would attribute one credential's
    /// authority to another, so those calls omit the key entirely — while a
    /// profile bearer with nothing recorded reports `null`, which is a
    /// different statement from silence.
    #[test]
    fn status_scopes_are_attributed_to_the_bearer() {
        let granted = StoredCredentials {
            scopes: Some(r#"["org:1:rwa"]"#.to_owned()),
            ..StoredCredentials::default()
        };
        let ungranted = StoredCredentials::default();

        let mut value = json!({"authenticated": true});
        apply_stored_scopes(&mut value, true, Some(&granted));
        assert_eq!(value["scopes"], json!(r#"["org:1:rwa"]"#));

        // Present-as-null: this credential, nothing recorded.
        let mut value = json!({"authenticated": true});
        apply_stored_scopes(&mut value, true, Some(&ungranted));
        assert_eq!(
            value.get("scopes"),
            Some(&Value::Null),
            "a stored profile with no grant reports null, not silence"
        );

        // A flag/env bearer omits the key, even though the profile has one.
        let mut value = json!({"authenticated": true});
        apply_stored_scopes(&mut value, false, Some(&granted));
        assert!(
            value.get("scopes").is_none(),
            "a flag/env bearer must never borrow the profile's grant: {value}"
        );

        // No stored profile at all: nothing to attribute.
        let mut value = json!({"authenticated": false});
        apply_stored_scopes(&mut value, true, None);
        assert!(value.get("scopes").is_none(), "{value}");
    }

    /// A credentials directory holding the two profiles the fallback test
    /// needs: a metadata-only `x` (a recorded grant, no usable credential) and
    /// a `default` that actually holds the token.
    fn fallback_profile_dir(tag: &str) -> std::path::PathBuf {
        use fastio_cli::auth::credentials::CredentialsFile;
        use secrecy::SecretString;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "fastio-cli-status-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create config dir");
        let mut file = CredentialsFile::load(&dir).expect("load credentials");
        file.set(
            "x",
            StoredCredentials {
                scopes: Some(r#"["org:1:rwa"]"#.to_owned()),
                ..StoredCredentials::default()
            },
            &dir,
        )
        .expect("write profile x");
        file.set(
            "default",
            StoredCredentials {
                token: Some(SecretString::from("default-token")),
                ..StoredCredentials::default()
            },
            &dir,
        )
        .expect("write default profile");
        dir
    }

    /// `--profile x` where `x` holds a recorded grant but NO usable credential
    /// falls back to the default profile's token, so the bearer belongs to
    /// `default` while `status` reads `x`. Reporting `x`'s scopes there would
    /// describe a credential the command never used — a full-access default
    /// bearer would appear narrowly scoped.
    #[test]
    fn status_omits_scopes_when_the_bearer_came_from_the_fallback_profile() {
        use fastio_cli::auth::credentials::CredentialsFile;
        use fastio_cli::auth::token;

        let dir = fallback_profile_dir("fallback");
        // Resolution takes its environment INJECTED, never from the ambient
        // process environment: a `FASTIO_API_KEY` in the developer's shell
        // would otherwise win precedence 3 and the fallback under test would
        // never run, while the assertions below still passed.
        let resolved = token::resolve_token_with_env(None, None, None, "x", &dir)
            .expect("resolution succeeds");
        // POSITIVE CONTROL: the fallback must actually have fired. Without
        // this, a `resolve_token` returning `None` for any reason would satisfy
        // every assertion below for the wrong reason.
        assert_eq!(
            resolved.as_deref(),
            Some("default-token"),
            "profile x has no usable credential, so the default profile's token must be the bearer"
        );
        let creds_file = CredentialsFile::load(&dir).expect("load credentials");
        let stored = creds_file.get("x");
        assert!(
            stored.is_some_and(|s| s.scopes.is_some()),
            "fixture: profile x must carry a recorded grant"
        );

        // The bearer-origin inputs are injected for the same reason.
        // `bearer_from_store` has its own test for the env arms.
        let describes = super::stored_scopes_describe_bearer(
            super::bearer_from_store(None, None, None),
            resolved.as_deref(),
            stored,
        );
        assert!(
            !describes,
            "profile x supplied no bearer, so its grant must not be reported"
        );

        let mut value = json!({"authenticated": true});
        apply_stored_scopes(&mut value, describes, stored);
        assert!(
            value.get("scopes").is_none(),
            "a fallback bearer must not borrow the requested profile's grant: {value}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no bearer at all the payload is `authenticated: false`. A `scopes`
    /// key there claims a grant for a credential the command could not even
    /// find.
    #[test]
    fn status_omits_scopes_when_there_is_no_bearer() {
        let stored = StoredCredentials {
            scopes: Some(r#"["org:1:rwa"]"#.to_owned()),
            ..StoredCredentials::default()
        };
        let describes = super::stored_scopes_describe_bearer(true, None, Some(&stored));
        assert!(
            !describes,
            "no bearer means nothing to attribute a grant to"
        );

        let mut value = json!({"authenticated": false, "reason": "no_credentials"});
        apply_stored_scopes(&mut value, describes, Some(&stored));
        assert!(
            value.get("scopes").is_none(),
            "an unauthenticated payload must carry no grant: {value}"
        );
    }

    /// POSITIVE CONTROL: a profile that DID supply the bearer still reports its
    /// grant. Without this, a guard that dropped `scopes` unconditionally would
    /// pass both tests above.
    #[test]
    fn status_reports_scopes_when_the_profile_supplied_the_bearer() {
        use secrecy::SecretString;

        let stored = StoredCredentials {
            token: Some(SecretString::from("profile-token")),
            scopes: Some(r#"["org:1:rwa"]"#.to_owned()),
            ..StoredCredentials::default()
        };
        assert!(super::stored_scopes_describe_bearer(
            true,
            Some("profile-token"),
            Some(&stored)
        ));

        let mut value = json!({"authenticated": true});
        apply_stored_scopes(&mut value, true, Some(&stored));
        assert_eq!(value["scopes"], json!(r#"["org:1:rwa"]"#));
    }

    // ─── `api-key create` rendering ────────────────────────────────────────

    /// The create endpoint has no `response` sub-object, so the envelope's own
    /// `result` flag arrives alongside the key fields. It is bookkeeping, not
    /// part of the key. Everything else the server sent is shown, because this
    /// is the only time the raw key is ever returned and the user needs the id
    /// and scopes beside it.
    #[test]
    fn api_key_create_renders_the_whole_key_without_the_envelope_flag() {
        let mut extra = serde_json::Map::new();
        extra.insert("result".to_owned(), json!(true));
        extra.insert("id".to_owned(), json!("key-1"));
        extra.insert("memo".to_owned(), json!("ci"));
        extra.insert("scopes".to_owned(), json!(["org:1:rwa"]));
        extra.insert("admin".to_owned(), json!(true));
        extra.insert("legacy".to_owned(), json!(false));
        let response = ApiKeyCreateResponse {
            api_key: "SECRET-KEY-VALUE".to_owned(),
            extra,
        };

        let rendered = api_key_create_value(&response);
        assert_eq!(rendered["status"], json!("created"));
        assert_eq!(rendered["id"], json!("key-1"));
        assert_eq!(rendered["memo"], json!("ci"));
        assert_eq!(rendered["scopes"], json!(["org:1:rwa"]));
        assert_eq!(rendered["admin"], json!(true));
        assert_eq!(rendered["legacy"], json!(false));
        assert!(
            rendered.get("result").is_none(),
            "the envelope flag is not part of the key: {rendered}"
        );

        let text = rendered.to_string();
        assert_eq!(
            text.matches("SECRET-KEY-VALUE").count(),
            1,
            "the raw key is shown exactly once: {text}"
        );
    }

    /// A server field named like one of the CLI's own must not displace it —
    /// `status` and `api_key` are written last for exactly that reason.
    #[test]
    fn api_key_create_keeps_its_own_fields_authoritative() {
        let mut extra = serde_json::Map::new();
        extra.insert("status".to_owned(), json!("something-else"));
        let response = ApiKeyCreateResponse {
            api_key: "K".to_owned(),
            extra,
        };
        let rendered = api_key_create_value(&response);
        assert_eq!(rendered["status"], json!("created"));
        assert_eq!(rendered["api_key"], json!("K"));
    }

    // ─── `auth scopes` row shaping ─────────────────────────────────────────

    fn detail_body() -> Value {
        json!({
            "result": true,
            "auth_type": "jwt_v2",
            "full_access": false,
            "admin": true,
            "legacy": false,
            "scopes": ["org:12345:rwa", "userdetails:*:rw"],
            "scopes_detail": [
                {"entity_type": "org", "entity_id": "12345", "access_mode": "rwa",
                 "name": "Example Org"},
                {"entity_type": "userdetails", "entity_id": "*", "access_mode": "rw"},
            ],
        })
    }

    /// The hydrated entries are the better rows — they carry entity names — and
    /// each is stamped with the credential-level facts so a single row answers
    /// "what is this credential, and what can it reach?" on its own.
    #[test]
    fn scopes_rows_use_the_hydrated_detail_when_present() {
        let rows = scopes_rows_for_table(&detail_body());
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["entity_type"], json!("org"));
        assert_eq!(rows[0]["name"], json!("Example Org"));
        for row in rows {
            assert_eq!(row["auth_type"], json!("jwt_v2"));
            assert_eq!(row["full_access"], json!(false));
            assert_eq!(row["admin"], json!(true));
            assert_eq!(row["legacy"], json!(false));
        }
    }

    /// With no hydrated detail — a legacy credential, or one the server did not
    /// expand — the plain scope strings still become rows rather than a bare
    /// list of strings with no context.
    #[test]
    fn scopes_rows_fall_back_to_the_plain_strings() {
        let body = json!({
            "result": true,
            "auth_type": "api_key_scoped",
            "full_access": false,
            "admin": false,
            "legacy": true,
            "scopes": ["org:1:rw", "workspace:2:r"],
            "scopes_detail": [],
        });
        let rows = scopes_rows_for_table(&body);
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["scope"], json!("org:1:rw"));
        assert_eq!(rows[1]["scope"], json!("workspace:2:r"));
        for row in rows {
            assert_eq!(row["auth_type"], json!("api_key_scoped"));
            assert_eq!(row["full_access"], json!(false));
            assert_eq!(row["admin"], json!(false));
            assert_eq!(row["legacy"], json!(true));
        }
    }

    /// An unscoped credential has no rows to show, so the top-level object IS
    /// the answer and is returned untouched.
    #[test]
    fn scopes_rows_leave_a_scopeless_payload_alone() {
        let body = json!({
            "result": true,
            "auth_type": "jwt_v1",
            "full_access": true,
            "scopes": [],
            "scopes_detail": [],
        });
        assert_eq!(scopes_rows_for_table(&body), body);
        // And a non-object payload is never reshaped either.
        let scalar = json!("nope");
        assert_eq!(scopes_rows_for_table(&scalar), scalar);
    }

    /// The reshape is a TABLE/CSV rendering concern. `json` and `markdown`
    /// render the server's payload, so the reshape must be a pure function the
    /// caller can decline to apply — never a mutation of the response.
    #[test]
    fn scopes_reshape_never_touches_the_source_payload() {
        let body = detail_body();
        let before = body.clone();
        let _ = scopes_rows_for_table(&body);
        assert_eq!(body, before, "the json path must see the original bytes");
    }

    /// The credential-level facts are carried GENERICALLY, so a field the
    /// server adds later reaches the table instead of being dropped by a fixed
    /// list. `expires`, `is_agent` and `agent_name` are exactly the fields an
    /// enumerated list had already lost.
    #[test]
    fn scopes_rows_carry_every_credential_level_scalar() {
        let body = json!({
            "result": true,
            "auth_type": "api_key_scoped",
            "full_access": false,
            "admin": false,
            "legacy": false,
            "is_agent": true,
            "agent_name": "ci-runner",
            "expires": "2026-12-31 23:59:59",
            "scopes": ["org:12345:rw"],
            "scopes_detail": [
                {"entity_type": "org", "entity_id": "12345", "access_mode": "rw"},
            ],
        });
        let rows = scopes_rows_for_table(&body);
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 1);
        for row in rows {
            for key in [
                "auth_type",
                "full_access",
                "admin",
                "legacy",
                "is_agent",
                "agent_name",
                "expires",
            ] {
                assert!(row.get(key).is_some(), "{key} must reach the row: {row}");
            }
            assert_eq!(row["is_agent"], json!(true));
            assert_eq!(row["agent_name"], json!("ci-runner"));
            assert_eq!(row["expires"], json!("2026-12-31 23:59:59"));
            // The envelope flag is bookkeeping, not a credential fact.
            assert!(row.get("result").is_none(), "{row}");
            // The arrays are the rows' own source, never a cell.
            assert!(row.get("scopes").is_none(), "{row}");
            assert!(row.get("scopes_detail").is_none(), "{row}");
        }
    }

    /// A non-empty `scopes_detail` carrying no objects is not a hydrated view —
    /// there is nothing to build a row from, so the plain strings must still
    /// produce rows rather than a table of unusable entries.
    #[test]
    fn scopes_rows_fall_back_when_detail_holds_no_objects() {
        let body = json!({
            "result": true,
            "auth_type": "api_key_scoped",
            "scopes": ["org:1:rw", "workspace:2:r"],
            "scopes_detail": ["org:1:rw", "workspace:2:r"],
        });
        let rows = scopes_rows_for_table(&body);
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["scope"], json!("org:1:rw"));
        assert_eq!(rows[1]["scope"], json!("workspace:2:r"));
        assert_eq!(rows[0]["auth_type"], json!("api_key_scoped"));
    }

    /// A MIXED array uses the hydrated object for the scope it describes, and
    /// falls back to a plain `{scope}` row for the one it does not — dropping
    /// the uncovered string would hide a granted scope on the very command the
    /// docs tell agents to run to confirm what they got.
    #[test]
    fn scopes_rows_keep_only_the_object_entries_of_a_mixed_detail() {
        let body = json!({
            "result": true,
            "auth_type": "jwt_v2",
            "scopes": ["org:1:rw", "workspace:2:r"],
            "scopes_detail": [
                "org:1:rw",
                {"entity_type": "workspace", "entity_id": "2", "access_mode": "r"},
            ],
        });
        let rows = scopes_rows_for_table(&body);
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 2, "no granted scope may be dropped: {rows:?}");
        assert_eq!(rows[0]["entity_type"], json!("workspace"));
        assert!(
            rows[0].get("scope").is_none(),
            "the hydrated entry is used as-is: {}",
            rows[0]
        );
        assert_eq!(
            rows[1]["scope"],
            json!("org:1:rw"),
            "the uncovered string still gets a row: {}",
            rows[1]
        );
        for row in rows {
            assert_eq!(row["auth_type"], json!("jwt_v2"));
        }
    }

    /// A detail entry that names its scope with a `scope` field covers by that
    /// field rather than by position — a positional rule would credit the
    /// wrong string whenever a mixed array reorders or omits entries.
    #[test]
    fn a_detail_entry_covers_the_scope_string_it_names() {
        let body = json!({
            "result": true,
            "auth_type": "jwt_v2",
            "scopes": ["org:1:rw", "workspace:2:r", "share:3:r"],
            "scopes_detail": [
                {"scope": "share:3:r", "name": "Public share"},
            ],
        });
        let rows = scopes_rows_for_table(&body);
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["name"], json!("Public share"));
        assert_eq!(rows[1]["scope"], json!("org:1:rw"));
        assert_eq!(rows[2]["scope"], json!("workspace:2:r"));
    }

    /// An unrecognizable detail shape names no scope at all, so it can only be
    /// taken to account for the entries it sits in front of. The remainder
    /// still reaches the table.
    #[test]
    fn an_unrecognizable_detail_entry_covers_by_position() {
        let body = json!({
            "result": true,
            "auth_type": "jwt_v2",
            "scopes": ["org:1:rw", "workspace:2:r"],
            "scopes_detail": [{"label": "something new"}],
        });
        let rows = scopes_rows_for_table(&body);
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["label"], json!("something new"));
        assert_eq!(rows[1]["scope"], json!("workspace:2:r"));
    }

    /// A per-row key and the credential-level field of the same name are
    /// DIFFERENT facts. Letting the row value suppress the credential one hid
    /// whether the key itself is an admin credential, so both are carried —
    /// always, so the column set does not depend on the values. The rule is
    /// GENERIC: `expires` here collides on a name nobody enumerated, and must
    /// survive exactly as `admin` and `legacy` do.
    #[test]
    fn scopes_rows_keep_both_the_row_and_credential_admin() {
        let body = json!({
            "result": true,
            "auth_type": "jwt_v2",
            "admin": true,
            "legacy": false,
            "expires": "2026-12-31 23:59:59",
            "scopes": ["org:12345:rwa"],
            "scopes_detail": [
                {"entity_type": "org", "entity_id": "12345", "access_mode": "rwa",
                 "admin": false, "legacy": true, "expires": "2026-06-30 00:00:00"},
            ],
        });
        let rows = scopes_rows_for_table(&body);
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows[0]["admin"], json!(false), "the row's own value stands");
        assert_eq!(rows[0]["credential_admin"], json!(true));
        assert_eq!(rows[0]["legacy"], json!(true));
        assert_eq!(rows[0]["credential_legacy"], json!(false));
        assert_eq!(rows[0]["expires"], json!("2026-06-30 00:00:00"));
        assert_eq!(
            rows[0]["credential_expires"],
            json!("2026-12-31 23:59:59"),
            "an unenumerated collision must be kept too: {}",
            rows[0]
        );
    }

    /// `--fields` projects over whatever rows the renderer is given, so the
    /// reshaped rows must be projectable the same way any listing is.
    #[test]
    fn field_projection_works_over_the_reshaped_rows() {
        let rows = scopes_rows_for_table(&detail_body());
        let fields = ["entity_id".to_owned(), "access_mode".to_owned()];
        let projected = filter_fields(&rows, Some(&fields));
        let projected = projected.as_array().expect("an array of rows");
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0]["entity_id"], json!("12345"));
        assert_eq!(projected[0]["access_mode"], json!("rwa"));
        assert!(
            projected[0].get("auth_type").is_none(),
            "the projection must drop what was not asked for: {:?}",
            projected[0]
        );
    }
}
