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

/// Best-effort owner-only file permissions on non-Unix platforms.
///
/// On Unix the owner-only (0600) mode is set atomically at file-creation time
/// via `OpenOptionsExt::mode` in [`write_secure_file`], so this helper exists
/// only for the non-Unix fallback (where no mode-at-open API is available); it
/// is a no-op there.
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

/// Compute a UNIQUE sibling temp path for an atomic [`write_secure_file`].
///
/// Appends a `.<pid>.<counter>.tmp` suffix to the destination name so the temp
/// lives in the **same directory** as `path` — a prerequisite for the atomic
/// rename (rename is only atomic within one filesystem, and a sibling is
/// guaranteed to be on the same one) — while remaining unique per call. The
/// PID disambiguates concurrent processes and a process-global [`AtomicU64`]
/// counter disambiguates concurrent in-process writes, so two callers never
/// collide on a predictable `<path>.tmp` sibling (which was also symlink/
/// pre-creation prone). Mirrors the `partial_path` pattern in
/// `client.rs::download_file_stream`; deliberately does NOT use wall-clock time.
fn secure_tmp_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    std::path::PathBuf::from(name)
}

/// Write data to a file atomically and restrict its permissions to owner-only.
///
/// The data is written to a UNIQUE temporary sibling, durably flushed, then
/// atomically renamed over the target path (`rename` is atomic on the same
/// filesystem, preventing a truncated/corrupt file if the process crashes
/// mid-write). An existing destination is replaced even on Windows (where
/// `rename` refuses to overwrite) via a backup swap.
///
/// On Unix the temp is created with `create_new(true)` at mode `0600` so it is
/// **never** world-readable for any window (the previous `std::fs::write`
/// followed by a separate chmod left a predictable `<path>.tmp` sibling briefly
/// at the umask default, ~0644 — a permission race and a symlink/collision
/// target). On non-Unix the temp is created best-effort (no mode-at-open API)
/// but still uses a unique name.
///
/// Temp creation is a DISTINCT first step: if `create_new` fails (the unique
/// path is somehow already taken, a permission error, a symlink, …) the call
/// returns immediately WITHOUT cleanup — we must never `remove_file` a path
/// this invocation did not create. Only once the temp is confirmed ours does
/// the write/rename run with best-effort cleanup on any error.
pub fn write_secure_file(path: &std::path::Path, data: &str) -> Result<(), CliError> {
    let tmp_path = secure_tmp_path(path);
    // Establish ownership of the temp BEFORE entering the cleanup-bearing path.
    let file = create_secure_temp(&tmp_path)?;
    finalize_secure_write(file, &tmp_path, path, data).inspect_err(|_| {
        // The temp is ours now; remove it best-effort on any failure so we
        // never leak a partially-written sibling.
        let _ = std::fs::remove_file(&tmp_path);
    })
}

/// Create the unique temp sibling, owner-only at creation time on Unix.
///
/// Uses `create_new(true)` so the call FAILS (rather than truncating /
/// following a symlink) if anything already exists at the unique path. On Unix
/// the mode is set atomically via `OpenOptionsExt::mode(0o600)`; on non-Unix
/// the (no-op) [`set_file_permissions`] is applied after creation as a
/// best-effort fallback.
fn create_secure_temp(tmp_path: &std::path::Path) -> Result<std::fs::File, CliError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(tmp_path)?;
    #[cfg(not(unix))]
    set_file_permissions(tmp_path)?;
    Ok(file)
}

/// Write `data` into the already-owned temp `file`, durably flush it, then
/// atomically replace `path` with the temp. The caller owns temp cleanup on
/// any error returned here.
fn finalize_secure_write(
    mut file: std::fs::File,
    tmp_path: &std::path::Path,
    path: &std::path::Path,
    data: &str,
) -> Result<(), CliError> {
    use std::io::Write;
    file.write_all(data.as_bytes())?;
    file.flush()?;
    // Durability barrier so the renamed file's contents are on disk.
    file.sync_all()?;
    drop(file);
    atomic_replace(tmp_path, path)?;
    Ok(())
}

/// Atomically replace `dest` with `tmp`.
///
/// On Unix `rename` replaces an existing `dest` in one step. On Windows
/// `rename` refuses to overwrite an existing file (`AlreadyExists`); mirror the
/// backup-swap used by `client.rs::download_file_stream`: move `dest` aside to a
/// unique sibling backup, rename `tmp` into place, then remove the backup
/// (rolling back if the second rename fails so `dest` is never left missing).
fn atomic_replace(tmp: &std::path::Path, dest: &std::path::Path) -> Result<(), CliError> {
    match std::fs::rename(tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let backup = secure_tmp_path(dest);
            std::fs::rename(dest, &backup)?;
            match std::fs::rename(tmp, dest) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&backup);
                    Ok(())
                }
                Err(replace_err) => {
                    // Restore the original so `dest` is never left missing.
                    let _ = std::fs::rename(&backup, dest);
                    Err(replace_err.into())
                }
            }
        }
        Err(e) => Err(e.into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a unique temp directory under the system temp dir without pulling
    /// in a dev-dependency. Uses pid + an atomic counter for uniqueness.
    fn unique_temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fastio-cli-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn write_secure_file_writes_contents_and_is_atomic() {
        let dir = unique_temp_dir("write");
        let path = dir.join("secret.txt");
        write_secure_file(&path, "hunter2").expect("write");
        let read_back = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(read_back, "hunter2");
        // No predictable `<path>.tmp` sibling and no leftover temp siblings.
        assert!(!path.with_extension("tmp").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("secret.txt.") && name.ends_with(".tmp")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "left an orphaned temp sibling: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_secure_file_result_is_0600_and_leaves_no_world_readable_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_temp_dir("perms");
        let path = dir.join("creds.json");
        write_secure_file(&path, "{\"token\":\"x\"}").expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "result file must be 0600, got {mode:o}");
        // The predictable race-prone sibling must never exist after the call.
        assert!(!path.with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
