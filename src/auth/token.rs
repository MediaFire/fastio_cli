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

// ─── Refresh ─────────────────────────────────────────────────────────────────

/// Outcome of an attempted proactive token refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefreshOutcome {
    /// Nothing to do — no stored PKCE credential needed refreshing.
    NotNeeded,
    /// The access token was refreshed and re-persisted.
    Refreshed,
    /// A refresh was attempted and failed; stored credentials are untouched.
    Failed,
}

/// Refresh a stored PKCE access token that has expired (or is about to), using
/// the long-lived refresh token already on disk.
///
/// **This exists because the CLI previously never refreshed at all.** It stored
/// `refresh_token` at login and then, on expiry, told the user to run
/// `fastio auth login` again — while the server's refresh tokens are valid for
/// ~10 years. A once-per-invocation refresh turns an hourly re-login into a
/// credential that lasts until it is revoked.
///
/// Deliberately **best-effort and non-fatal**: on any failure the stored
/// credentials are left untouched and [`RefreshOutcome::Failed`] is returned, so
/// the caller proceeds exactly as it did before and the existing "expired —
/// please run `fastio auth login`" error still surfaces from [`resolve_token`].
/// A refresh that cannot happen must never be worse than not trying.
///
/// Skips entirely when a higher-precedence credential is in play (`--token`,
/// `FASTIO_TOKEN`, `FASTIO_API_KEY`) or when the profile holds an API key —
/// those never expire client-side, so there is nothing to refresh and no reason
/// to spend a network round-trip. Picks its target via [`select_refresh_target`],
/// which mirrors [`resolve_token`]'s fallback so it only ever refreshes the
/// profile the invocation would actually use.
///
/// # Errors
/// Never returns `Err` for a refresh failure — that is reported as
/// [`RefreshOutcome::Failed`]. Only a credentials-file read error propagates.
pub async fn refresh_if_needed(
    api_base: &str,
    profile_name: &str,
    config_dir: &Path,
    flag_token: Option<&str>,
) -> Result<RefreshOutcome, CliError> {
    // Higher-precedence credentials win in `resolve_token`, so refreshing the
    // profile would be wasted work that the caller would not even use.
    if flag_token.is_some_and(|t| !t.is_empty())
        || std::env::var("FASTIO_TOKEN").is_ok_and(|v| !v.is_empty())
        || std::env::var("FASTIO_API_KEY").is_ok_and(|v| !v.is_empty())
    {
        return Ok(RefreshOutcome::NotNeeded);
    }

    let creds_file = CredentialsFile::load(config_dir)?;

    let Some(target) = select_refresh_target(&creds_file, profile_name) else {
        return Ok(RefreshOutcome::NotNeeded);
    };

    // Clone the whole record: it must outlive the borrow across the `await`,
    // and carrying it wholesale is what lets the write below use struct-update
    // syntax instead of re-listing fields.
    let Some(existing) = creds_file.get(&target).cloned() else {
        return Ok(RefreshOutcome::NotNeeded);
    };
    let Some(refresh) = existing.expose_refresh_token().map(ToOwned::to_owned) else {
        return Ok(RefreshOutcome::NotNeeded);
    };

    // Unauthenticated client: the refresh grant carries its own credential.
    let Ok(client) = crate::client::ApiClient::new(api_base, None) else {
        return Ok(RefreshOutcome::Failed);
    };
    let resp =
        match crate::api::auth::pkce_refresh(&client, &refresh, crate::auth::pkce::PKCE_CLIENT_ID)
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                // The reason is worth a trace line — "revoked" and "offline" need
                // different user action. Safe to render: an OAuth failure body is
                // `{"error":"invalid_grant",…}` and carries no token material.
                tracing::debug!(error = %e, profile = %target, "token refresh failed");
                return Ok(RefreshOutcome::Failed);
            }
        };

    // Re-load immediately before writing. `set` re-serializes the WHOLE profile
    // map, so the copy read before the network round-trip is stale by seconds —
    // long enough for a concurrent `auth login` / `set-api-key` / `logout` to be
    // silently reverted by this write. Re-reading shrinks that window to the
    // write itself. (`write_secure_file` already makes the write atomic, so the
    // file can never be torn — this is about lost updates, not corruption.)
    let mut creds_file = CredentialsFile::load(config_dir)?;

    // Bail if the profile stopped being refreshable while we were on the
    // network — a `logout` in that window must not be undone by resurrecting
    // the credential it just removed.
    if !profile_needs_refresh(&creds_file, &target) {
        return Ok(RefreshOutcome::NotNeeded);
    }

    let now = chrono::Utc::now().timestamp();
    if creds_file
        .set(
            &target,
            crate::auth::credentials::StoredCredentials {
                token: Some(secrecy::SecretString::from(resp.access_token)),
                // A refresh response MAY omit a new refresh token; keep the old
                // one rather than dropping the credential that makes this work
                // at all. (This server does not rotate — the published API docs
                // say the refresh token is "returned unchanged (no per-refresh
                // rotation)" — and RFC 6749 §6 says the same, so this stays
                // correct if rotation is ever added.)
                refresh_token: Some(secrecy::SecretString::from(
                    resp.refresh_token.unwrap_or(refresh),
                )),
                expires_at: Some(now + resp.expires_in),
                auth_method: Some("pkce".to_owned()),
                // Every other field is carried over rather than re-listed, so a
                // field added to `StoredCredentials` later is not silently
                // dropped on every refresh. Matches `commands/auth.rs:740`.
                ..existing
            },
            config_dir,
        )
        .is_err()
    {
        // A refresh that succeeded on the wire but could not be persisted is a
        // failure from the caller's point of view — it must produce the warning,
        // not silently look like success.
        return Ok(RefreshOutcome::Failed);
    }

    Ok(RefreshOutcome::Refreshed)
}

/// Whether `resolve_token` would STOP at this profile rather than fall back to
/// the default — i.e. it holds a credential of any kind.
///
/// Mirrors [`resolve_from_profile`]'s `Ok(None)` condition: an API key or a
/// token (expired or not) makes the profile terminal. An expired token is
/// deliberately terminal — `resolve_token` errors there and never reaches the
/// default.
fn profile_is_terminal(creds_file: &CredentialsFile, profile: &str) -> bool {
    creds_file
        .get(profile)
        .is_some_and(|c| c.expose_api_key().is_some() || c.expose_token().is_some())
}

/// Pick the profile to refresh, mirroring [`resolve_token`]'s fallback exactly.
///
/// The subtlety: falling back merely because the named profile does not *need*
/// refreshing is wrong. A healthy or API-key-holding named profile is the one
/// the command will use, so refreshing the default in that case spends a
/// network round-trip and rewrites a credential the invocation never touches.
fn select_refresh_target(creds_file: &CredentialsFile, profile_name: &str) -> Option<String> {
    if profile_needs_refresh(creds_file, profile_name) {
        return Some(profile_name.to_owned());
    }
    if profile_name != DEFAULT_PROFILE
        && !profile_is_terminal(creds_file, profile_name)
        && profile_needs_refresh(creds_file, DEFAULT_PROFILE)
    {
        return Some(DEFAULT_PROFILE.to_owned());
    }
    None
}

/// Whether a profile holds a PKCE credential that is expired or expiring soon
/// and carries a refresh token to renew it with.
fn profile_needs_refresh(creds_file: &CredentialsFile, profile: &str) -> bool {
    let Some(creds) = creds_file.get(profile) else {
        return false;
    };
    // An API key takes precedence in `resolve_token` and never expires.
    if creds.expose_api_key().is_some() {
        return false;
    }
    if creds.expose_token().is_none() || creds.expose_refresh_token().is_none() {
        return false;
    }
    is_expired(creds.expires_at) || is_expiring_soon(creds.expires_at)
}

#[cfg(test)]
mod refresh_tests {
    use super::{DEFAULT_PROFILE, profile_needs_refresh, select_refresh_target};
    use crate::auth::credentials::{CredentialsFile, StoredCredentials};
    use secrecy::SecretString;

    fn creds(
        token: Option<&str>,
        refresh: Option<&str>,
        api_key: Option<&str>,
        expires_at: Option<i64>,
    ) -> StoredCredentials {
        StoredCredentials {
            token: token.map(SecretString::from),
            refresh_token: refresh.map(SecretString::from),
            api_key: api_key.map(SecretString::from),
            expires_at,
            user_id: None,
            email: None,
            auth_method: Some("pkce".to_owned()),
        }
    }

    fn file_with(profile: &str, c: StoredCredentials) -> CredentialsFile {
        let mut f = CredentialsFile::default();
        f.profiles.insert(profile.to_owned(), c);
        f
    }

    fn past() -> i64 {
        chrono::Utc::now().timestamp() - 60
    }
    fn soon() -> i64 {
        chrono::Utc::now().timestamp() + 60
    }
    fn far() -> i64 {
        chrono::Utc::now().timestamp() + 86_400
    }

    #[test]
    fn refreshes_an_expired_pkce_credential() {
        let f = file_with(
            DEFAULT_PROFILE,
            creds(Some("t"), Some("r"), None, Some(past())),
        );
        assert!(profile_needs_refresh(&f, DEFAULT_PROFILE));
    }

    #[test]
    fn refreshes_proactively_inside_the_expiry_window() {
        // Renewing BEFORE expiry is what removes the mid-command failure: a
        // token valid when the command starts can die during a long upload.
        let f = file_with(
            DEFAULT_PROFILE,
            creds(Some("t"), Some("r"), None, Some(soon())),
        );
        assert!(profile_needs_refresh(&f, DEFAULT_PROFILE));
    }

    #[test]
    fn leaves_a_healthy_token_alone() {
        let f = file_with(
            DEFAULT_PROFILE,
            creds(Some("t"), Some("r"), None, Some(far())),
        );
        assert!(!profile_needs_refresh(&f, DEFAULT_PROFILE));
    }

    #[test]
    fn never_refreshes_when_an_api_key_is_stored() {
        // An API key wins in `resolve_token` and never expires client-side, so
        // a refresh here would be a pointless round-trip on every invocation —
        // even alongside a long-expired token.
        let f = file_with(
            DEFAULT_PROFILE,
            creds(Some("t"), Some("r"), Some("k"), Some(past())),
        );
        assert!(!profile_needs_refresh(&f, DEFAULT_PROFILE));
    }

    #[test]
    fn cannot_refresh_without_a_refresh_token() {
        // The basic-auth login path stores no refresh token; there is nothing
        // to renew with, and the user must log in again as before.
        let f = file_with(DEFAULT_PROFILE, creds(Some("t"), None, None, Some(past())));
        assert!(!profile_needs_refresh(&f, DEFAULT_PROFILE));
    }

    #[test]
    fn no_expiry_recorded_means_nothing_to_do() {
        let f = file_with(DEFAULT_PROFILE, creds(Some("t"), Some("r"), None, None));
        assert!(!profile_needs_refresh(&f, DEFAULT_PROFILE));
    }

    #[test]
    fn unknown_profile_is_not_a_refresh_candidate() {
        let f = file_with(
            DEFAULT_PROFILE,
            creds(Some("t"), Some("r"), None, Some(past())),
        );
        assert!(!profile_needs_refresh(&f, "no-such-profile"));
    }

    // ─── Target selection (mirrors `resolve_token`'s fallback) ───────────────
    //
    // The bug these pin: falling back to `default` merely because the NAMED
    // profile did not need refreshing. `resolve_token` stops at any profile
    // holding a credential, so refreshing `default` in that case rewrites a
    // credential the invocation never uses — and widens the concurrent-write
    // window on the shared credentials file for no benefit.

    fn expiring() -> i64 {
        chrono::Utc::now().timestamp() - 60
    }
    fn healthy() -> i64 {
        chrono::Utc::now().timestamp() + 86_400
    }

    #[test]
    fn healthy_named_profile_stops_fallback_to_default() {
        let mut f = CredentialsFile::default();
        f.profiles.insert(
            "work".to_owned(),
            creds(Some("t"), Some("r"), None, Some(healthy())),
        );
        f.profiles.insert(
            DEFAULT_PROFILE.to_owned(),
            creds(Some("t2"), Some("r2"), None, Some(expiring())),
        );
        assert_eq!(
            select_refresh_target(&f, "work"),
            None,
            "a healthy named profile is the one resolve_token uses; default must not be refreshed"
        );
    }

    #[test]
    fn api_key_named_profile_stops_fallback_to_default() {
        let mut f = CredentialsFile::default();
        f.profiles
            .insert("work".to_owned(), creds(None, None, Some("k"), None));
        f.profiles.insert(
            DEFAULT_PROFILE.to_owned(),
            creds(Some("t2"), Some("r2"), None, Some(expiring())),
        );
        assert_eq!(select_refresh_target(&f, "work"), None);
    }

    /// An EXPIRED but non-refreshable named profile is still terminal:
    /// `resolve_token` errors there and never reaches the default.
    #[test]
    fn expired_unrefreshable_named_profile_stops_fallback() {
        let mut f = CredentialsFile::default();
        f.profiles.insert(
            "work".to_owned(),
            creds(Some("t"), None, None, Some(expiring())),
        );
        f.profiles.insert(
            DEFAULT_PROFILE.to_owned(),
            creds(Some("t2"), Some("r2"), None, Some(expiring())),
        );
        assert_eq!(select_refresh_target(&f, "work"), None);
    }

    /// The fallback that IS legitimate: the named profile holds nothing at all.
    #[test]
    fn absent_named_profile_falls_back_to_default() {
        let mut f = CredentialsFile::default();
        f.profiles.insert(
            DEFAULT_PROFILE.to_owned(),
            creds(Some("t"), Some("r"), None, Some(expiring())),
        );
        assert_eq!(
            select_refresh_target(&f, "work"),
            Some(DEFAULT_PROFILE.to_owned())
        );
    }

    #[test]
    fn named_profile_needing_refresh_is_chosen_over_default() {
        let mut f = CredentialsFile::default();
        f.profiles.insert(
            "work".to_owned(),
            creds(Some("t"), Some("r"), None, Some(expiring())),
        );
        f.profiles.insert(
            DEFAULT_PROFILE.to_owned(),
            creds(Some("t2"), Some("r2"), None, Some(expiring())),
        );
        assert_eq!(select_refresh_target(&f, "work"), Some("work".to_owned()));
    }
}
