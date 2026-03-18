/// Shared API types for the Fast.io REST API.
///
/// Defines the standard response envelope and common data structures
/// used across multiple API domains.
use std::fmt;

use serde::{Deserialize, Serialize};

/// Sign-in response from `GET /user/auth/`.
#[derive(Deserialize)]
pub struct SignInResponse {
    /// Token lifetime in seconds.
    pub expires_in: i64,
    /// JWT access token.
    pub auth_token: String,
    /// Whether 2FA verification is required.
    #[serde(rename = "2factor", default)]
    pub two_factor: bool,
}

impl fmt::Debug for SignInResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInResponse")
            .field("expires_in", &self.expires_in)
            .field("auth_token", &"[REDACTED]")
            .field("two_factor", &self.two_factor)
            .finish()
    }
}

/// Token check response from `GET /user/auth/check/`.
#[derive(Debug, Deserialize)]
pub struct AuthCheckResponse {
    /// The user ID associated with the token.
    ///
    /// The API may return this as either a string or an integer,
    /// so we accept both and normalise to `String`.
    #[serde(deserialize_with = "deserialize_string_or_number")]
    pub id: String,
}

/// Accept a JSON string **or** a JSON number and always produce a `String`.
fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected string or number for id, got {other}"
        ))),
    }
}

/// Sign-up response (the account creation itself returns minimal data).
#[derive(Debug, Deserialize)]
pub struct SignUpResponse {
    /// Placeholder for any fields returned on account creation.
    #[allow(dead_code)]
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// 2FA verification response.
#[derive(Deserialize)]
pub struct TwoFactorVerifyResponse {
    /// Full-scope token after successful 2FA.
    pub auth_token: String,
    /// Token lifetime in seconds.
    pub expires_in: i64,
}

impl fmt::Debug for TwoFactorVerifyResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TwoFactorVerifyResponse")
            .field("auth_token", &"[REDACTED]")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// 2FA status response.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct TwoFactorStatusResponse {
    /// Current 2FA state (e.g., "enabled", "disabled").
    pub state: String,
    /// Whether TOTP is configured.
    #[serde(default)]
    pub totp: bool,
}

/// 2FA enable response.
#[derive(Debug, Deserialize)]
pub struct TwoFactorEnableResponse {
    /// TOTP binding URI (for QR code generation).
    pub binding_uri: Option<String>,
    /// Any extra fields.
    #[allow(dead_code)]
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// PKCE authorize response from `GET /oauth/authorize/`.
#[derive(Debug, Deserialize)]
pub struct PkceAuthorizeResponse {
    /// The authorization request ID.
    pub auth_request_id: String,
}

/// PKCE token exchange response from `POST /oauth/token/`.
#[derive(Deserialize)]
pub struct PkceTokenResponse {
    /// JWT access token.
    pub access_token: String,
    /// Token type (always "Bearer").
    #[allow(dead_code)]
    pub token_type: String,
    /// Token lifetime in seconds.
    pub expires_in: i64,
    /// Refresh token for obtaining new access tokens.
    pub refresh_token: Option<String>,
    /// Granted scope.
    #[allow(dead_code)]
    pub scope: Option<String>,
}

impl fmt::Debug for PkceTokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PkceTokenResponse")
            .field("access_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("scope", &self.scope)
            .finish()
    }
}

/// API key creation response.
#[derive(Deserialize)]
pub struct ApiKeyCreateResponse {
    /// The newly created API key.
    pub api_key: String,
}

impl fmt::Debug for ApiKeyCreateResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyCreateResponse")
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

/// API key listing response.
#[derive(Debug, Deserialize)]
pub struct ApiKeyListResponse {
    /// Number of keys returned.
    #[allow(dead_code)]
    pub results: u32,
    /// List of API key objects (nullable).
    pub api_keys: Option<Vec<serde_json::Value>>,
}

/// Generic empty/success response.
#[derive(Debug, Serialize, Deserialize)]
pub struct EmptyResponse {
    /// Any extra fields returned by the API.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}
