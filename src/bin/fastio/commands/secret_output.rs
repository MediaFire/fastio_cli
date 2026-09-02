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
/// Capture-or-redact a `lock_token` from a lock-acquire response.
///
/// `lock_token` is a bearer capability: whoever holds it can release or override
/// the lock. Rendering it to stdout puts it in shell history, terminal
/// scrollback, and any CI transcript — the same exposure `ws-token` already
/// avoids, which is why this mirrors that shipped behaviour exactly rather than
/// inventing a second convention.
///
/// This lives here, shared, because there are TWO acquire surfaces
/// (`fastio lock acquire` and `fastio files lock acquire`). Fixing one and
/// leaving the other is how a guard becomes invisible: the code looks correct on
/// whichever path a reviewer happens to open.
///
/// Redaction applies to EVERY output format, because the leak is the payload,
/// not the rendering — a `--format json` consumer piping to a log is the case
/// most likely to persist it.
///
/// # Errors
/// Returns an error only if `token_file` was supplied and could not be written.
pub fn capture_or_redact_lock_token(
    value: &mut Value,
    token_file: Option<&Path>,
    quiet: bool,
) -> Result<()> {
    let Some(token) = extract_secret(value, "lock_token") else {
        return Ok(());
    };
    if let Some(path) = token_file {
        write_secret_file(path, &token, "lock token", quiet)?;
    } else if !quiet {
        eprintln!(
            "WARNING: the lock token is REDACTED from stdout to avoid leaking it into logs. \
             Re-run with --lock-token-file <path> to capture it (written 0600); you need it for \
             `lock release --lock-token`."
        );
    }
    redact_secret_field(value, "lock_token", "--lock-token-file");
    Ok(())
}

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
    use serde_json::json;

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

    /// A `lock_token` must never survive into rendered output, in ANY format:
    /// the leak is the payload, not the rendering, and `--format json` piped to
    /// a log is the case most likely to persist it.
    #[test]
    fn lock_token_is_redacted_from_the_rendered_value() {
        let mut v = json!({
            "result": true,
            "lock_token": "5e143937c16e1eeed6810c549bf40b4740151daeaf36623a01e41f66084a0d08",
            "expires_at": "2026-08-25 22:25:41 UTC"
        });
        super::capture_or_redact_lock_token(&mut v, None, true).expect("redact");
        let rendered = v.to_string();
        assert!(
            !rendered.contains("5e143937c16e1eeed6810c549bf40b4740151daeaf36623a01e41f66084a0d08"),
            "the raw lock token must not survive into output: {rendered}"
        );
        assert!(
            rendered.contains("expires_at"),
            "non-secret fields must be preserved: {rendered}"
        );
    }

    /// With `--lock-token-file` the token must reach the FILE (0600) and still
    /// leave stdout clean — capture and redaction are not alternatives.
    #[cfg(unix)]
    #[test]
    fn lock_token_file_captures_0600_and_still_redacts_output() {
        use std::os::unix::fs::PermissionsExt;
        let secret = "5e143937c16e1eeed6810c549bf40b4740151daeaf36623a01e41f66084a0d08";
        let dir = std::env::temp_dir().join(format!("fastio-locktok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("tok");
        let mut v = json!({ "result": true, "lock_token": secret });
        super::capture_or_redact_lock_token(&mut v, Some(&path), true).expect("capture");

        let written = std::fs::read_to_string(&path).expect("read token file");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        let rendered = v.to_string();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(written.contains(secret), "the token must reach the file");
        assert_eq!(mode, 0o600, "the token file must be 0600, got {mode:o}");
        assert!(
            !rendered.contains(secret),
            "capturing to a file must ALSO redact stdout: {rendered}"
        );
    }
}
