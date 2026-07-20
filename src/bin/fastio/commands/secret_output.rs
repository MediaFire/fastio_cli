//! Shared helpers for handling one-time secrets in command output.
//!
//! A few commands mint or return a secret value (an outbound webhook HMAC
//! secret, a File Share WebSocket token). Those
//! secrets must NEVER be echoed to stdout (where they would leak into logs),
//! but the user still needs a way to capture them. The pattern, shared here so
//! it cannot drift between command modules, is:
//!
//! 1. [`extract_secret`] pulls the named secret out of the response envelope
//!    and wraps it in a [`SecretString`].
//! 2. [`write_secret_file`] writes it to a caller-supplied path, created 0600
//!    atomically (no TOCTOU window), with a one-time stderr confirmation.
//! 3. [`redact_secret_field`] replaces the secret in the rendered response with
//!    a placeholder that names the capture flag (`--secret-file` /
//!    `--token-file`), so what reaches stdout never carries the value.
//!
//! These live here as shared helpers so the File Share `ws-token` command
//! (and any future secret-minting command) reuses the exact same tested
//! behavior rather than a weaker hand-rolled copy.

use std::path::Path;

use anyhow::{Context, Result};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

/// Write a one-time secret to `path` with 0600 permissions, without ever
/// echoing it to stdout. Emits a stderr confirmation (suppressed under
/// `quiet`). The secret is held in a [`SecretString`] and exposed only for the
/// single write.
///
/// Delegates to [`fastio_cli::config::write_secure_file`], which creates the
/// temp file 0600 at open time (`create_new` + `OpenOptionsExt::mode(0o600)` on
/// Unix), writes the secret, then atomically renames it into place. This closes
/// the TOCTOU window a write-then-chmod-in-place would have: a one-time webhook
/// secret / realtime token / WebSocket token is never observable at default
/// (umask) permissions under its final path.
///
/// # Errors
///
/// Returns an error if the secure write fails (e.g. the target path is not
/// writable or already exists).
pub fn write_secret_file(
    path: &Path,
    secret: &SecretString,
    label: &str,
    quiet: bool,
) -> Result<()> {
    fastio_cli::config::write_secure_file(path, secret.expose_secret())
        .with_context(|| format!("failed to write {label} to '{}'", path.display()))?;
    if !quiet {
        eprintln!(
            "{label} written to '{}' (0600). Store it now — it is shown ONLY once and is not \
             retrievable later.",
            path.display()
        );
    }
    Ok(())
}

/// Extract a named secret string from an API response envelope's `response`
/// object (or the top level), wrapping it in a [`SecretString`].
///
/// Also checks the `outbound_webhook_subscription` nesting used by the webhook
/// subscription create/rotate bodies; for top-level secrets (realtime token,
/// WebSocket token) the first lookup matches.
#[must_use]
pub fn extract_secret(value: &Value, key: &str) -> Option<SecretString> {
    let payload = value.get("response").unwrap_or(value);
    payload
        .get(key)
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("outbound_webhook_subscription")
                .and_then(|o| o.get(key))
                .and_then(Value::as_str)
        })
        .map(|s| SecretString::from(s.to_owned()))
}

/// Build the placeholder substituted for a redacted secret field, naming the
/// flag the caller should pass to capture it (e.g. `--secret-file` for webhook
/// secrets, `--token-file` for realtime / WebSocket tokens).
fn redacted_placeholder(capture_flag: &str) -> String {
    format!("<redacted; see {capture_flag}>")
}

/// Replace `key` with the redaction placeholder in a serde object, if present.
fn redact_in_object(obj: &mut serde_json::Map<String, Value>, key: &str, placeholder: &str) {
    if obj.contains_key(key) {
        obj.insert(key.to_owned(), Value::String(placeholder.to_owned()));
    }
}

/// Redact a named secret field in a response so it is not rendered to stdout.
///
/// Covers every shape the secret can appear in:
/// - top-level `key`;
/// - top-level `outbound_webhook_subscription.key` (the POST-envelope-unwrap
///   shape `{result:true, outbound_webhook_subscription:{secret:...}}` the
///   client's response handler produces — there is no `response` wrapper left
///   by the time the command renders);
/// - under `response.key`;
/// - under `response.outbound_webhook_subscription.key`.
///
/// `capture_flag` names the CLI flag (`--secret-file` / `--token-file`) the user
/// should re-run with to capture the value, so the rendered placeholder points
/// at the right flag.
pub fn redact_secret_field(value: &mut Value, key: &str, capture_flag: &str) {
    let placeholder = redacted_placeholder(capture_flag);
    if let Some(obj) = value.get_mut("response").and_then(Value::as_object_mut) {
        redact_in_object(obj, key, &placeholder);
        if let Some(sub) = obj
            .get_mut("outbound_webhook_subscription")
            .and_then(Value::as_object_mut)
        {
            redact_in_object(sub, key, &placeholder);
        }
    }
    if let Some(obj) = value.as_object_mut() {
        redact_in_object(obj, key, &placeholder);
        if let Some(sub) = obj
            .get_mut("outbound_webhook_subscription")
            .and_then(Value::as_object_mut)
        {
            redact_in_object(sub, key, &placeholder);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_secret_finds_nested_and_top_level() {
        let nested = serde_json::json!({
            "response": {"outbound_webhook_subscription": {"secret": "abc123"}}
        });
        assert_eq!(
            extract_secret(&nested, "secret").map(|s| s.expose_secret().to_owned()),
            Some("abc123".to_owned())
        );
        let top = serde_json::json!({"response": {"secret": "xyz"}});
        assert_eq!(
            extract_secret(&top, "secret").map(|s| s.expose_secret().to_owned()),
            Some("xyz".to_owned())
        );
        // A top-level token (realtime / WebSocket mint shape) is found directly.
        let token = serde_json::json!({"result": true, "token": "jwt-here"});
        assert_eq!(
            extract_secret(&token, "token").map(|s| s.expose_secret().to_owned()),
            Some("jwt-here".to_owned())
        );
    }

    #[test]
    fn redact_secret_field_post_unwrap_webhook_shape() {
        let mut v = serde_json::json!({
            "result": true,
            "outbound_webhook_subscription": {"secret": "leak-me", "id": "s1"}
        });
        redact_secret_field(&mut v, "secret", "--secret-file");
        let rendered = serde_json::to_string(&v).unwrap();
        assert!(
            !rendered.contains("leak-me"),
            "secret must be ABSENT from rendered output, got: {rendered}"
        );
        assert_eq!(
            v["outbound_webhook_subscription"]["id"].as_str(),
            Some("s1")
        );
    }

    #[test]
    fn redact_secret_field_post_unwrap_realtime_token_shape() {
        let mut v = serde_json::json!({"result": true, "token": "secret-jwt", "expires": 60});
        redact_secret_field(&mut v, "token", "--token-file");
        redact_secret_field(&mut v, "auth_token", "--token-file");
        let rendered = serde_json::to_string(&v).unwrap();
        assert!(
            !rendered.contains("secret-jwt"),
            "realtime token must be ABSENT from rendered output, got: {rendered}"
        );
        assert_eq!(v["expires"].as_i64(), Some(60));
        assert_eq!(
            v["token"].as_str(),
            Some("<redacted; see --token-file>"),
            "placeholder must cite the token capture flag"
        );
    }

    #[test]
    fn redact_secret_field_wrapped_shapes_still_covered() {
        let mut nested = serde_json::json!({
            "response": {"outbound_webhook_subscription": {"secret": "leak", "id": "s1"}}
        });
        redact_secret_field(&mut nested, "secret", "--secret-file");
        assert!(!serde_json::to_string(&nested).unwrap().contains("leak"));

        let mut top = serde_json::json!({"response": {"secret": "leak", "id": "s2"}});
        redact_secret_field(&mut top, "secret", "--secret-file");
        assert!(!serde_json::to_string(&top).unwrap().contains("leak"));
        assert_eq!(top["response"]["id"].as_str(), Some("s2"));
    }

    #[cfg(unix)]
    #[test]
    fn write_secret_file_sets_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("so-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.txt");
        let secret = SecretString::from("topsecret".to_owned());
        write_secret_file(&path, &secret, "test secret", true).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "topsecret");
        std::fs::remove_dir_all(&dir).ok();
    }
}
