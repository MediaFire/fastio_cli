//! `fastio view <workspace_id> <node_id>` — render a markdown note or `.md`
//! file in the terminal.
//!
//! Dispatch (decision locked in Phase 0):
//!  1. Try `readnote/` (structured JSON `{content, note}`) — the canonical
//!     path for **note** nodes. Render `content`.
//!  2. If `readnote/` reports specifically that the node is **not a note**
//!     (HTTP 400 / code `1605` whose message says so), fetch the node's
//!     details and, only if it is a markdown file (`.md`/`.markdown` extension
//!     or `text/markdown` mimetype), fall back to the raw `read/` endpoint and
//!     render it. Any other `1605` (invalid node id, version mismatch) is a
//!     real error and is surfaced — never masked by the fallback.
//!
//! Rendering is TTY-gated by [`fastio_cli::output::view`]: piped output,
//! `--raw`, and `--no-color` all produce verbatim markdown (no ANSI), so
//! scripts and LLM consumers get faithful bytes and never a pager.

use anyhow::{Context, Result};
use serde_json::Value;

use fastio_cli::api;
use fastio_cli::error::CliError;
use fastio_cli::output::view::{ViewMode, render_markdown};

use super::CommandContext;

/// Internal command for `fastio view`.
#[derive(Debug)]
pub struct ViewCommand {
    /// Workspace ID.
    pub workspace_id: String,
    /// Node ID of the note or `.md` file.
    pub node_id: String,
    /// Print raw markdown without terminal rendering.
    pub raw: bool,
    /// Optional specific version `OpaqueId`.
    pub version: Option<String>,
    /// Reserved no-pager flag (no pager is ever launched).
    pub no_pager: bool,
}

/// Execute `fastio view`.
pub async fn execute(command: &ViewCommand, ctx: &CommandContext<'_>) -> Result<()> {
    anyhow::ensure!(
        !command.workspace_id.trim().is_empty(),
        "workspace ID must not be empty"
    );
    anyhow::ensure!(
        !command.node_id.trim().is_empty(),
        "node ID must not be empty"
    );
    // `--no-pager` is accepted for forward-compatibility and to let scripts
    // state non-interactive intent explicitly; `view` never launches a pager,
    // so the flag is intentionally a no-op today.
    tracing::trace!(
        no_pager = command.no_pager,
        "fastio view: no pager is ever launched"
    );
    let client = ctx.build_client()?;

    // Step 1: try the structured note endpoint.
    let markdown = match api::workspace::read_note(
        &client,
        &command.workspace_id,
        &command.node_id,
        command.version.as_deref(),
    )
    .await
    {
        Ok(value) => extract_note_content(&value)
            .ok_or_else(|| anyhow::anyhow!("note response did not contain markdown content"))?,
        // Step 2: the node is specifically NOT a note. Verify it is a markdown
        // file before reading raw bytes — `view` is a markdown viewer, not a
        // hexdump, so a non-markdown node is a clear error rather than a
        // mangled-bytes dump. Other `1605` errors (invalid id, version
        // mismatch) are preserved by `is_not_a_note` returning `false`.
        Err(err) if is_not_a_note(&err) => {
            let details =
                api::storage::get_file_details(&client, &command.workspace_id, &command.node_id)
                    .await
                    .context("failed to fetch node details for view fallback")?;
            if !node_is_markdown(&details) {
                let descriptor = node_type_descriptor(&details);
                anyhow::bail!(
                    "view supports notes and markdown files; {} is {}",
                    command.node_id,
                    descriptor
                );
            }
            api::storage::read_raw(
                &client,
                &command.workspace_id,
                &command.node_id,
                command.version.as_deref(),
            )
            .await
            .context("failed to read file content")?
        }
        Err(err) => return Err(anyhow::Error::new(err).context("failed to read note")),
    };

    // `--quiet` suppresses output entirely, consistent with the other paths.
    if ctx.output.quiet {
        return Ok(());
    }

    let mode = ViewMode::resolve_runtime(command.raw, ctx.output.no_color);
    render_markdown(&markdown, mode).context("failed to render markdown")?;
    Ok(())
}

/// Extract the `content` string from a `readnote/` response value.
///
/// The client unwraps the envelope, so the value is the inner `response`
/// object `{content, note}`. Returns `None` if `content` is absent or not a
/// string.
fn extract_note_content(value: &Value) -> Option<String> {
    value
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Whether an error indicates **specifically** that the node is not a note (so
/// `view` may consider the markdown-file fallback).
///
/// The `readnote/` endpoint returns HTTP 400 / code `1605` for THREE distinct
/// conditions (`storage.txt:753,755,758`): "Invalid node ID", "Node is not a
/// note", and "Version does not belong to this note". Only the middle one
/// warrants the raw-read fallback; the other two are genuine errors that must
/// be surfaced, never masked. We therefore additionally require the error
/// message (or its structured `details`) to contain the "not a note" phrase —
/// matching `code == 1605` alone would mask invalid-id and version-mismatch
/// errors behind a raw read.
fn is_not_a_note(err: &CliError) -> bool {
    let CliError::Api(api_err) = err else {
        return false;
    };
    if api_err.http_status != 400 || api_err.code != 1605 {
        return false;
    }
    if message_says_not_a_note(&api_err.message) {
        return true;
    }
    // Fall back to scanning the structured `details` payload (the server may
    // carry the human phrase under `reason`/`message` there).
    api_err
        .details
        .as_deref()
        .is_some_and(details_say_not_a_note)
}

/// Case-insensitive check for the "not a note" phrase in a message string.
fn message_says_not_a_note(message: &str) -> bool {
    message.to_ascii_lowercase().contains("not a note")
}

/// Scan a structured error `details` value for the "not a note" phrase in any
/// string field, recursively.
fn details_say_not_a_note(details: &Value) -> bool {
    match details {
        Value::String(s) => message_says_not_a_note(s),
        Value::Array(items) => items.iter().any(details_say_not_a_note),
        Value::Object(map) => map.values().any(details_say_not_a_note),
        _ => false,
    }
}

/// Whether a `details` response describes a markdown node — the only file type
/// (besides notes) that `fastio view` will render. True when the node name
/// ends in `.md`/`.markdown` (case-insensitive) OR the mimetype is
/// `text/markdown`. The client unwraps the envelope, so the value is the
/// `response` body `{format, node: {...}}`.
fn node_is_markdown(details: &Value) -> bool {
    let Some(node) = details.get("node") else {
        return false;
    };
    if let Some(name) = node.get("name").and_then(Value::as_str)
        && let Some(ext) = std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
        && (ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
    {
        return true;
    }
    node.get("mimetype")
        .and_then(Value::as_str)
        .is_some_and(|m| m.eq_ignore_ascii_case("text/markdown"))
}

/// A short human descriptor of a node for the "not markdown" error message,
/// preferring `mimetype`, then `type`, then a generic fallback.
fn node_type_descriptor(details: &Value) -> String {
    let node = details.get("node");
    node.and_then(|n| n.get("mimetype"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| node.and_then(|n| n.get("type")).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map_or_else(|| "not a markdown file".to_owned(), |s| format!("a {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_note_content() {
        let v = json!({"content": "# Title\n\nBody", "note": {"id": "n1"}});
        assert_eq!(extract_note_content(&v).as_deref(), Some("# Title\n\nBody"));
    }

    #[test]
    fn missing_content_returns_none() {
        let v = json!({"note": {"id": "n1"}});
        assert_eq!(extract_note_content(&v), None);
    }

    #[test]
    fn non_string_content_returns_none() {
        let v = json!({"content": 42});
        assert_eq!(extract_note_content(&v), None);
    }

    #[test]
    fn not_a_note_detects_1605_400() {
        let err = CliError::Api(fastio_cli::error::ApiError::new(
            1605,
            None,
            "Node is not a note".to_owned(),
            400,
        ));
        assert!(is_not_a_note(&err));
    }

    #[test]
    fn not_a_note_ignores_404() {
        let err = CliError::Api(fastio_cli::error::ApiError::new(
            1609,
            None,
            "Note not found".to_owned(),
            404,
        ));
        assert!(!is_not_a_note(&err));
    }

    #[test]
    fn not_a_note_ignores_non_api_errors() {
        let err = CliError::Parse("bad".to_owned());
        assert!(!is_not_a_note(&err));
    }

    #[test]
    fn not_a_note_preserves_invalid_node_id_1605() {
        // FIX 3: a 1605 "Invalid node ID" must NOT trigger the fallback —
        // it is a real error, not a not-a-note signal.
        let err = CliError::Api(fastio_cli::error::ApiError::new(
            1605,
            None,
            "Invalid node ID".to_owned(),
            400,
        ));
        assert!(!is_not_a_note(&err));
    }

    #[test]
    fn not_a_note_preserves_version_mismatch_1605() {
        // FIX 3: a 1605 "Version does not belong to this note" must NOT
        // trigger the fallback either.
        let err = CliError::Api(fastio_cli::error::ApiError::new(
            1605,
            None,
            "Version does not belong to this note".to_owned(),
            400,
        ));
        assert!(!is_not_a_note(&err));
    }

    #[test]
    fn not_a_note_detected_via_details_payload() {
        // The phrase may arrive in the structured `details` rather than the
        // top-level message.
        let mut err = fastio_cli::error::ApiError::new(1605, None, "Invalid Input".to_owned(), 400);
        err.details = Some(Box::new(json!({"reason": "Node is not a note"})));
        assert!(is_not_a_note(&CliError::Api(err)));
    }

    #[test]
    fn node_is_markdown_accepts_md_extension() {
        let details =
            json!({"node": {"name": "README.MD", "mimetype": "application/octet-stream"}});
        assert!(node_is_markdown(&details));
    }

    #[test]
    fn node_is_markdown_accepts_markdown_extension() {
        let details = json!({"node": {"name": "notes.markdown"}});
        assert!(node_is_markdown(&details));
    }

    #[test]
    fn node_is_markdown_accepts_text_markdown_mimetype() {
        let details = json!({"node": {"name": "weird-name", "mimetype": "text/markdown"}});
        assert!(node_is_markdown(&details));
    }

    #[test]
    fn node_is_markdown_rejects_non_markdown() {
        // FIX 4: a binary/non-markdown file must be rejected so `view` never
        // dumps mangled bytes.
        let details = json!({"node": {"name": "photo.jpg", "mimetype": "image/jpeg"}});
        assert!(!node_is_markdown(&details));
        // Missing `node` is also not markdown.
        assert!(!node_is_markdown(&json!({"result": true})));
    }

    #[test]
    fn node_type_descriptor_prefers_mimetype() {
        let details = json!({"node": {"name": "photo.jpg", "mimetype": "image/jpeg"}});
        assert_eq!(node_type_descriptor(&details), "a image/jpeg");
        let folder = json!({"node": {"name": "Docs", "type": "folder"}});
        assert_eq!(node_type_descriptor(&folder), "a folder");
        assert_eq!(node_type_descriptor(&json!({})), "not a markdown file");
    }
}
