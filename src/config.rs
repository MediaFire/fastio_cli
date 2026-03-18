#![allow(clippy::missing_errors_doc)]

/// Configuration file management for the Fast.io CLI.
///
/// Manages profile settings and delegates credential storage to the auth module.
/// By default, configuration is stored in `$XDG_CONFIG_HOME/fastio-cli/` (typically
/// `~/.config/fastio-cli/`). Library consumers can specify a custom directory via
/// [`Config::load_from`].
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// Default API base URL.
pub const DEFAULT_API_BASE: &str = "https://api.fast.io/current";

/// Built-in fallback profile used when the requested profile does not exist.
static FALLBACK_PROFILE: std::sync::LazyLock<Profile> = std::sync::LazyLock::new(|| Profile {
    api_base: DEFAULT_API_BASE.to_owned(),
    auth_method: "pkce".to_owned(),
});

/// Name of the default profile.
const DEFAULT_PROFILE_NAME: &str = "default";

/// Top-level configuration stored in `config.json` within the config directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directory where configuration and credential files are stored.
    #[serde(skip)]
    pub config_dir: PathBuf,
    /// Name of the active profile.
    pub default_profile: String,
    /// Map of profile name to profile settings.
    pub profiles: HashMap<String, Profile>,
}

/// Settings for a single named profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// API base URL override.
    pub api_base: String,
    /// Authentication method hint (`pkce`, `basic`, `api_key`).
    pub auth_method: String,
}

impl Default for Config {
    fn default() -> Self {
        let mut profiles = HashMap::new();
        profiles.insert(
            DEFAULT_PROFILE_NAME.to_owned(),
            Profile {
                api_base: DEFAULT_API_BASE.to_owned(),
                auth_method: "pkce".to_owned(),
            },
        );
        Self {
            config_dir: Config::default_dir().unwrap_or_default(),
            default_profile: DEFAULT_PROFILE_NAME.to_owned(),
            profiles,
        }
    }
}

/// Restrict directory permissions to owner-only on Unix systems (mode 0700).
#[cfg(unix)]
fn set_dir_permissions(path: &std::path::Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// No-op on non-Unix platforms.
#[cfg(not(unix))]
fn set_dir_permissions(_path: &std::path::Path) -> Result<(), CliError> {
    Ok(())
}

/// Restrict file permissions to owner-only on Unix systems (mode 0600).
#[cfg(unix)]
fn set_file_permissions(path: &std::path::Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// No-op on non-Unix platforms.
#[cfg(not(unix))]
fn set_file_permissions(_path: &std::path::Path) -> Result<(), CliError> {
    Ok(())
}

/// Create the config directory with restricted permissions if it does not exist.
///
/// If the directory already exists, permissions are re-applied to ensure
/// they have not been loosened since creation.
pub fn ensure_config_dir(dir: &Path) -> Result<(), CliError> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    set_dir_permissions(dir)?;
    Ok(())
}

/// Write data to a file atomically and restrict its permissions to owner-only.
///
/// The data is first written to a temporary file in the same directory, then
/// permissions are applied, and finally the temp file is renamed to the target
/// path. Because `rename` is atomic on the same filesystem, this prevents
/// corruption if the process crashes mid-write.
pub fn write_secure_file(path: &std::path::Path, data: &str) -> Result<(), CliError> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, data)?;
    set_file_permissions(&tmp_path)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

impl Config {
    /// Return the default XDG-compliant configuration directory.
    ///
    /// Uses `$XDG_CONFIG_HOME/fastio-cli` (typically `~/.config/fastio-cli`).
    pub fn default_dir() -> Result<PathBuf, CliError> {
        let base = dirs::config_dir()
            .ok_or_else(|| CliError::Config("unable to determine config directory".to_owned()))?;
        Ok(base.join("fastio-cli"))
    }

    /// Return the path to the config file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    /// Load configuration from the default directory, creating defaults if missing.
    pub fn load() -> Result<Self, CliError> {
        Self::load_from(&Self::default_dir()?)
    }

    /// Load configuration from a custom directory, creating defaults if missing.
    ///
    /// If the file is missing or empty, a default config is created and saved.
    /// If the file contains invalid JSON or fails validation, an error is returned.
    pub fn load_from(dir: &Path) -> Result<Self, CliError> {
        let path = dir.join("config.json");
        if !path.exists() {
            let config = Self {
                config_dir: dir.to_owned(),
                ..Self::default()
            };
            config.save()?;
            return Ok(config);
        }
        let data = std::fs::read_to_string(&path)?;
        if data.trim().is_empty() {
            tracing::warn!("config file is empty, re-creating with defaults");
            let config = Self {
                config_dir: dir.to_owned(),
                ..Self::default()
            };
            config.save()?;
            return Ok(config);
        }
        let mut config: Self =
            serde_json::from_str(&data).map_err(|e| CliError::Parse(e.to_string()))?;
        dir.clone_into(&mut config.config_dir);
        config.validate()?;
        Ok(config)
    }

    /// Validate that the loaded configuration is internally consistent.
    fn validate(&self) -> Result<(), CliError> {
        if self.default_profile.is_empty() {
            return Err(CliError::Config(
                "default_profile is empty in config.json".to_owned(),
            ));
        }
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(CliError::Config(format!(
                "default_profile '{}' does not exist in profiles",
                self.default_profile
            )));
        }
        for (name, profile) in &self.profiles {
            if profile.api_base.is_empty() {
                return Err(CliError::Config(format!(
                    "profile '{name}' has an empty api_base"
                )));
            }
            if profile.auth_method.is_empty() {
                return Err(CliError::Config(format!(
                    "profile '{name}' has an empty auth_method"
                )));
            }
        }
        Ok(())
    }

    /// Persist configuration to disk with restricted file permissions.
    pub fn save(&self) -> Result<(), CliError> {
        ensure_config_dir(&self.config_dir)?;
        let path = self.path();
        let data =
            serde_json::to_string_pretty(self).map_err(|e| CliError::Parse(e.to_string()))?;
        write_secure_file(&path, &data)?;
        Ok(())
    }

    /// Get the active profile, falling back to defaults.
    ///
    /// If the requested profile does not exist, a warning is logged and a
    /// built-in fallback profile is returned.
    #[must_use]
    pub fn active_profile(&self, override_name: Option<&str>) -> &Profile {
        let name = override_name.unwrap_or(&self.default_profile);
        self.profiles.get(name).unwrap_or_else(|| {
            tracing::warn!(
                profile = name,
                "requested profile not found, using built-in fallback"
            );
            &FALLBACK_PROFILE
        })
    }

    /// Resolve the API base URL from profile or override.
    #[must_use]
    pub fn api_base(&self, override_base: Option<&str>, profile: Option<&str>) -> String {
        if let Some(base) = override_base {
            return base.to_owned();
        }
        self.active_profile(profile).api_base.clone()
    }

    /// Delete a profile by name.
    ///
    /// Returns an error if the profile does not exist or if it is the
    /// current default profile (switch default first).
    pub fn delete_profile(&mut self, name: &str) -> Result<(), CliError> {
        if !self.profiles.contains_key(name) {
            return Err(CliError::Config(format!("profile '{name}' does not exist")));
        }
        if self.default_profile == name {
            return Err(CliError::Config(format!(
                "cannot delete the default profile '{name}'. \
                 Switch default first with `fastio configure set-default <other>`."
            )));
        }
        self.profiles.remove(name);
        self.save()
    }
}
