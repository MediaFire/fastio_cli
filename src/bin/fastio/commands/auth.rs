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

use super::CommandContext;

/// Execute an auth subcommand.
pub async fn execute(
    command: &AuthCommand,
    config: &Config,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    match command {
        AuthCommand::Login { email, password } => {
            if let (Some(email), Some(password)) = (email.as_deref(), password.as_deref()) {
                login_basic(config, ctx, email, password).await
            } else if password.is_some() {
                anyhow::bail!(
                    "--password requires --email. Provide both for direct login, \
                     or omit both for browser login."
                )
            } else {
                login_pkce(config, ctx, email.as_deref()).await
            }
        }
        AuthCommand::Logout => logout(ctx),
        AuthCommand::Status => status(ctx).await,
        AuthCommand::Signup {
            email,
            password,
            first_name,
            last_name,
        } => {
            signup(
                config,
                ctx,
                email,
                password,
                first_name.as_deref(),
                last_name.as_deref(),
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
    },
    /// Clear stored credentials.
    Logout,
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
    /// Revoke a single session.
    Revoke {
        /// Session ID.
        session_id: String,
    },
    /// Revoke all sessions.
    RevokeAll,
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
            "message": "2FA verification required. Run: fastio auth 2fa verify <code>",
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

/// Login via PKCE browser flow.
async fn login_pkce(
    _config: &Config,
    ctx: &CommandContext<'_>,
    email_hint: Option<&str>,
) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;

    let challenge = pkce::generate_challenge().context("failed to generate PKCE challenge")?;

    // Initiate the PKCE flow
    let auth_resp = api::auth::pkce_authorize(
        &client,
        pkce::PKCE_CLIENT_ID,
        &challenge.code_challenge,
        &challenge.state,
        pkce::PKCE_REDIRECT_URI,
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
                // Distinguish "account not validated" from a truly invalid token.
                let (reason, message) = match e {
                    fastio_cli::error::CliError::Api(api_err) if api_err.code == 10587 => (
                        "account_not_validated",
                        Some(
                            "Account email has not been verified. Run `fastio auth verify` to resend the verification email.",
                        ),
                    ),
                    _ => ("token_invalid", None),
                };
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
) -> Result<()> {
    let client = ApiClient::new(ctx.api_base, None).context("failed to create API client")?;

    api::auth::sign_up(&client, email, password, first_name, last_name)
        .await
        .context("signup failed")?;

    // Auto-login after signup
    if let Ok(login) = api::auth::sign_in(&client, email, password).await {
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
        let value = json!({
            "status": "signed_up",
            "message": "Account created. Auto-login failed; run: fastio auth login",
            "email": email,
        });
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
        ApiKeyCommand::Create { name, scopes } => {
            let result =
                api::auth::api_key_create(&client, name.as_deref(), scopes.as_deref(), None)
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
        } => {
            if name.is_none() && scopes.is_none() {
                anyhow::bail!("at least one update field is required (--name, --scopes)");
            }
            let result =
                api::auth::api_key_update(&client, key_id, name.as_deref(), scopes.as_deref())
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
        OauthCommand::RevokeAll => {
            api::auth::oauth_revoke_all(&client)
                .await
                .context("failed to revoke all OAuth sessions")?;
            let value = json!({
                "status": "all_revoked",
            });
            ctx.output.render(&value)?;
        }
    }
    Ok(())
}
