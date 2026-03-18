#![allow(clippy::missing_errors_doc)]

/// Credential storage for the Fast.io CLI.
///
/// Stores and retrieves per-profile credentials from
/// `credentials.json` within the config directory. Sensitive fields (tokens)
/// are wrapped in `secrecy::SecretString` to prevent accidental
/// exposure in logs or debug output. The credentials file is
/// written with restrictive permissions (0600 on Unix).
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CliError;

/// Serialize an `Option<SecretString>` by exposing its inner value.
// serde `serialize_with` passes `&Option<T>`, so `ref_option` is unavoidable here.
#[allow(clippy::ref_option)]
fn serialize_secret_opt<S: Serializer>(
    val: &Option<SecretString>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    match val {
        Some(s) => ser.serialize_some(s.expose_secret()),
        None => ser.serialize_none(),
    }
}

/// Deserialize an `Option<SecretString>` from a plain string.
fn deserialize_secret_opt<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<Option<SecretString>, D::Error> {
    let opt: Option<String> = Option::deserialize(de)?;
    Ok(opt.map(SecretString::from))
}

/// Credentials stored for a single profile.
///
/// Sensitive fields use `SecretString` to prevent accidental
/// exposure via `Debug` or logging. Use the `expose_*` methods
/// to access the underlying values when needed.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct StoredCredentials {
    /// JWT access token.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_secret_opt",
        deserialize_with = "deserialize_secret_opt"
    )]
    pub token: Option<SecretString>,
    /// OAuth refresh token.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_secret_opt",
        deserialize_with = "deserialize_secret_opt"
    )]
    pub refresh_token: Option<SecretString>,
    /// API key (alternative to JWT).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_secret_opt",
        deserialize_with = "deserialize_secret_opt"
    )]
    pub api_key: Option<SecretString>,
    /// Token expiration as a UTC Unix timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// User ID associated with the credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Email address associated with the credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Authentication method used (`basic`, `pkce`, `api_key`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
}

impl fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCredentials")
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("expires_at", &self.expires_at)
            .field("user_id", &self.user_id)
            .field("email", &self.email)
            .field("auth_method", &self.auth_method)
            .finish()
    }
}

impl StoredCredentials {
    /// Expose the JWT access token value.
    pub fn expose_token(&self) -> Option<&str> {
        self.token.as_ref().map(ExposeSecret::expose_secret)
    }

    /// Expose the refresh token value.
    #[allow(dead_code)]
    pub fn expose_refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_ref().map(ExposeSecret::expose_secret)
    }

    /// Expose the API key value.
    pub fn expose_api_key(&self) -> Option<&str> {
        self.api_key.as_ref().map(ExposeSecret::expose_secret)
    }
}

/// On-disk credentials file mapping profile names to credentials.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialsFile {
    /// Profile name to credentials mapping.
    pub profiles: HashMap<String, StoredCredentials>,
}

impl CredentialsFile {
    /// Path to the credentials file within the given config directory.
    #[must_use]
    pub fn path(dir: &Path) -> std::path::PathBuf {
        dir.join("credentials.json")
    }

    /// Load credentials from disk, returning an empty set if missing.
    pub fn load(dir: &Path) -> Result<Self, CliError> {
        let path = Self::path(dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(&path)?;
        serde_json::from_str(&data).map_err(|e| CliError::Parse(e.to_string()))
    }

    /// Persist credentials to disk with restricted file permissions.
    ///
    /// Uses atomic write (temp file + rename) to prevent corruption on
    /// crash. On Unix the file is written with mode 0600 so only the
    /// owner can read or write stored tokens.
    pub fn save(&self, dir: &Path) -> Result<(), CliError> {
        crate::config::ensure_config_dir(dir)?;
        let path = Self::path(dir);
        let data =
            serde_json::to_string_pretty(self).map_err(|e| CliError::Parse(e.to_string()))?;
        crate::config::write_secure_file(&path, &data)?;
        Ok(())
    }

    /// Get credentials for a profile.
    #[must_use]
    pub fn get(&self, profile: &str) -> Option<&StoredCredentials> {
        self.profiles.get(profile)
    }

    /// Store credentials for a profile and persist to disk.
    pub fn set(
        &mut self,
        profile: &str,
        creds: StoredCredentials,
        dir: &Path,
    ) -> Result<(), CliError> {
        self.profiles.insert(profile.to_owned(), creds);
        self.save(dir)
    }

    /// Remove credentials for a profile and persist.
    pub fn remove(&mut self, profile: &str, dir: &Path) -> Result<(), CliError> {
        self.profiles.remove(profile);
        self.save(dir)
    }
}
