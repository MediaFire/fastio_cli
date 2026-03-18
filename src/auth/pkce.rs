#![allow(clippy::missing_errors_doc)]

/// PKCE (Proof Key for Code Exchange) flow for the Fast.io CLI.
///
/// Implements RFC 7636 S256 challenge generation, authorization URL
/// construction, and a local HTTP server for receiving the callback.
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::error::CliError;

/// PKCE parameters generated for a single authorization flow.
///
/// `Debug` is manually implemented to redact the `code_verifier`,
/// which is a secret that must not appear in logs.
#[derive(Clone)]
pub struct PkceChallenge {
    /// The random code verifier (43-128 chars, base64url).
    pub code_verifier: String,
    /// The S256 challenge derived from the verifier.
    pub code_challenge: String,
    /// Random state parameter for CSRF protection.
    pub state: String,
}

impl std::fmt::Debug for PkceChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkceChallenge")
            .field("code_verifier", &"[REDACTED]")
            .field("code_challenge", &self.code_challenge)
            .field("state", &self.state)
            .finish()
    }
}

/// OAuth client ID registered for the CLI.
pub const PKCE_CLIENT_ID: &str = "fastio-cli";

/// Redirect URI for the CLI PKCE flow.
pub const PKCE_REDIRECT_URI: &str = "http://localhost:19836/callback";

/// Generate a new PKCE challenge with random verifier and state.
pub fn generate_challenge() -> Result<PkceChallenge, CliError> {
    let code_verifier = generate_code_verifier()?;
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_state()?;

    Ok(PkceChallenge {
        code_verifier,
        code_challenge,
        state,
    })
}

/// Generate a random code verifier (43 characters, base64url-encoded).
fn generate_code_verifier() -> Result<String, CliError> {
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes)?;
    Ok(base64url_encode(&bytes))
}

/// Derive the S256 code challenge from a verifier.
fn generate_code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url_encode(&digest)
}

/// Generate a random state parameter (UUID-like).
fn generate_state() -> Result<String, CliError> {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes)?;
    Ok(bytes.iter().fold(String::with_capacity(32), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    }))
}

/// Base64url-encode bytes (no padding, URL-safe alphabet).
fn base64url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Fill a buffer with cryptographically secure random bytes.
fn fill_random(buf: &mut [u8]) -> Result<(), CliError> {
    getrandom_crate::getrandom(buf)
        .map_err(|e| CliError::Auth(format!("failed to generate random bytes: {e}")))
}

/// Build the authorization URL for the PKCE flow.
///
/// The caller should open this URL in the user's browser.
#[allow(dead_code)]
#[must_use]
pub fn build_authorize_url(
    api_base: &str,
    challenge: &PkceChallenge,
    email_hint: Option<&str>,
) -> String {
    let mut params = vec![
        ("client_id", PKCE_CLIENT_ID.to_owned()),
        ("response_type", "code".to_owned()),
        ("code_challenge", challenge.code_challenge.clone()),
        ("code_challenge_method", "S256".to_owned()),
        ("state", challenge.state.clone()),
        ("redirect_uri", PKCE_REDIRECT_URI.to_owned()),
        ("response_format", "json".to_owned()),
    ];

    if let Some(email) = email_hint {
        params.push(("login_hint", email.to_owned()));
    }

    let query: String = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{api_base}/oauth/authorize?{query}")
}
