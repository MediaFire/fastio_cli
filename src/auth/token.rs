#![allow(clippy::missing_errors_doc)]

/// Token resolution for the Fast.io CLI.
///
/// Implements the authentication precedence chain:
/// 1. `--token` flag
/// 2. `FASTIO_TOKEN` env var
/// 3. `FASTIO_API_KEY` env var
/// 4. `--profile` flag stored credentials
/// 5. Default profile credentials (fallback when a non-default profile
///    was requested but had no usable credentials)
use std::path::Path;

use crate::auth::credentials::CredentialsFile;
use crate::error::CliError;

/// The default profile name used when no `--profile` flag is supplied.
const DEFAULT_PROFILE: &str = "default";

/// Resolve the active bearer token using the precedence chain.
///
/// Returns `Ok(None)` if no credentials are available (not an error,
/// since some commands like `auth login` do not require auth).
///
/// When stored credentials are expired, a `CliError::Auth` is returned
/// so the caller can prompt the user to re-authenticate rather than
/// silently proceeding without auth.
pub fn resolve_token(
    flag_token: Option<&str>,
    profile_name: &str,
    config_dir: &Path,
) -> Result<Option<String>, CliError> {
    // 1. Explicit --token flag (filter empty strings like env var checks)
    if let Some(t) = flag_token
        && !t.is_empty()
    {
        return Ok(Some(t.to_owned()));
    }

    // 2. FASTIO_TOKEN env var
    if let Ok(t) = std::env::var("FASTIO_TOKEN")
        && !t.is_empty()
    {
        return Ok(Some(t));
    }

    // 3. FASTIO_API_KEY env var
    if let Ok(k) = std::env::var("FASTIO_API_KEY")
        && !k.is_empty()
    {
        return Ok(Some(k));
    }

    // 4. Profile stored credentials (specified profile)
    let creds_file = CredentialsFile::load(config_dir)?;

    if let Some(token) = resolve_from_profile(&creds_file, profile_name)? {
        return Ok(Some(token));
    }

    // 5. Fallback to default profile when a different profile was requested
    if profile_name != DEFAULT_PROFILE
        && let Some(token) = resolve_from_profile(&creds_file, DEFAULT_PROFILE)?
    {
        return Ok(Some(token));
    }

    Ok(None)
}

/// Attempt to extract a usable token from a stored profile.
///
/// Returns an error if the token is expired so the caller can
/// direct the user to re-login instead of silently proceeding
/// without authentication.
fn resolve_from_profile(
    creds_file: &CredentialsFile,
    profile: &str,
) -> Result<Option<String>, CliError> {
    let Some(creds) = creds_file.get(profile) else {
        return Ok(None);
    };

    // Prefer API key if present (API keys don't expire client-side)
    if let Some(key) = creds.expose_api_key() {
        return Ok(Some(key.to_owned()));
    }

    if let Some(token) = creds.expose_token() {
        // Check expiry — return an error instead of silently dropping
        if is_expired(creds.expires_at) {
            return Err(CliError::Auth(format!(
                "token for profile \"{profile}\" has expired — please run `fastio auth login` to refresh"
            )));
        }

        // Warn if token expires within 5 minutes
        if is_expiring_soon(creds.expires_at) {
            eprintln!("warning: token for profile \"{profile}\" expires within 5 minutes");
        }

        return Ok(Some(token.to_owned()));
    }

    Ok(None)
}

/// Check whether stored credentials are expired.
#[must_use]
pub fn is_expired(expires_at: Option<i64>) -> bool {
    let Some(exp) = expires_at else {
        return false;
    };
    chrono::Utc::now().timestamp() >= exp
}

/// Check whether stored credentials will expire within 5 minutes.
fn is_expiring_soon(expires_at: Option<i64>) -> bool {
    let Some(exp) = expires_at else {
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    let five_minutes: i64 = 300;
    now < exp && (exp - now) <= five_minutes
}
