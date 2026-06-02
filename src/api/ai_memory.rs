#![allow(clippy::missing_errors_doc)]

//! AI-memory API endpoints for the Fast.io REST API.
//!
//! AI memory is a markdown blob the calling user authors to steer AI agents
//! working in the context of an organization or workspace. The endpoints are
//! **self-only** — every read, write, and delete operates on the caller's own
//! per-(scope, user) row; no member can read or write another member's row.
//!
//! Both scopes share an identical shape (`~/vividengine/llms/orgs.txt:2193-2324`,
//! `~/vividengine/llms/workspaces.txt:1860-1984`):
//! - `GET    /{org|workspace}/{id}/ai/memory/` — read the caller's row.
//! - `POST   /{org|workspace}/{id}/ai/memory/` — write the caller's row; body
//!   `{ content, revision? }`, form-encoded. An optional `revision` enables
//!   optimistic concurrency (HTTP 409 on mismatch). Omitting it is
//!   last-writer-wins.
//! - `DELETE /{org|workspace}/{id}/ai/memory/` — hard-delete the caller's row.
//!
//! `content` is capped at 64 KB (65,536 *raw bytes* — multibyte UTF-8 chars
//! consume more than one). A never-written row returns a stable empty envelope
//! (`content: ""`, `revision: 0`, `updated: null`) rather than a 404, so
//! callers can detect the unset state without branching on HTTP status.

use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::{ApiError, CliError};

/// Server-enforced upper bound on the `content` payload, in raw bytes
/// (not characters). Multibyte UTF-8 chars consume more than 1 byte.
/// Larger writes return HTTP 400 (`~/vividengine/llms/orgs.txt:2264`).
pub const MEMORY_MAX_BYTES: usize = 65_536;

/// AI-memory scope: organization-level or workspace-level. Each scope is
/// stored independently — the org row and the workspace row do not overlap
/// (`~/vividengine/llms/orgs.txt:2321`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryScope {
    /// `/org/{id}/ai/memory/`.
    Org,
    /// `/workspace/{id}/ai/memory/`.
    Workspace,
}

impl MemoryScope {
    /// The path segment for this scope (`org` or `workspace`).
    #[must_use]
    pub fn segment(self) -> &'static str {
        match self {
            MemoryScope::Org => "org",
            MemoryScope::Workspace => "workspace",
        }
    }

    /// The human-readable label used in validation messages
    /// (`org_id` / `workspace_id`).
    #[must_use]
    pub fn id_label(self) -> &'static str {
        match self {
            MemoryScope::Org => "org_id",
            MemoryScope::Workspace => "workspace_id",
        }
    }
}

/// Validate the `content` payload before sending. Returns a
/// `CliError::Parse` describing the violation if the byte length exceeds
/// [`MEMORY_MAX_BYTES`]. Empty strings are accepted — the server stores an
/// empty row (it is **not** deleted; use [`delete`] for that).
///
/// Mirrors `instructions.rs::validate_content` (same 64 KB raw-byte cap).
fn validate_content(content: &str) -> Result<(), CliError> {
    let len = content.len();
    if len > MEMORY_MAX_BYTES {
        return Err(CliError::Parse(format!(
            "AI memory content must be at most {MEMORY_MAX_BYTES} bytes (got {len})",
        )));
    }
    Ok(())
}

/// Reject empty / whitespace-only scope IDs before they hit the server.
/// `urlencoding::encode("")` collapses to `""`, which would produce paths
/// like `/org//ai/memory/` and surface as an opaque 404. Failing locally
/// gives the caller a crisp message.
fn validate_id(label: &str, id: &str) -> Result<(), CliError> {
    if id.trim().is_empty() {
        return Err(CliError::Parse(format!("{label} must not be empty")));
    }
    Ok(())
}

/// Build the `/{scope}/{id}/ai/memory/` path for a scope + id.
fn memory_path(scope: MemoryScope, id: &str) -> String {
    format!(
        "/{}/{}/ai/memory/",
        scope.segment(),
        urlencoding::encode(id),
    )
}

/// Map a write rejection to a clearer message where the server reported a
/// revision conflict (HTTP 409). On any other error the original is returned
/// unchanged.
///
/// The 409 arm rewrites the message so the caller knows to re-read the row
/// and retry with the fresh `revision`, rather than seeing the generic
/// conflict text. The HTTP status, code, and structured `details` are
/// preserved so downstream rendering / `suggestion()` still works.
fn map_write_error(err: CliError) -> CliError {
    match err {
        CliError::Api(api) if api.http_status == 409 => CliError::Api(ApiError {
            code: api.code,
            error_code: api.error_code,
            message:
                "AI memory changed since last read (revision conflict) — re-get the row, merge \
                 your edits, and retry with the new revision"
                    .to_owned(),
            http_status: api.http_status,
            details: api.details,
        }),
        other => other,
    }
}

/// Read the caller's AI memory row for a scope.
///
/// `GET /{org|workspace}/{id}/ai/memory/`. A never-written row returns the
/// stable empty envelope (`content: ""`, `revision: 0`, `updated: null`) —
/// it is **not** a 404.
pub async fn get(client: &ApiClient, scope: MemoryScope, id: &str) -> Result<Value, CliError> {
    validate_id(scope.id_label(), id)?;
    client.get(&memory_path(scope, id)).await
}

/// Write the caller's AI memory row for a scope.
///
/// `POST /{org|workspace}/{id}/ai/memory/` with a **form-encoded** body
/// (`content`, optional `revision`). When `revision` is supplied the write is
/// conditional: the server returns HTTP 409 on a mismatch and leaves the row
/// untouched (optimistic concurrency). Omitting `revision` is
/// last-writer-wins. `content` is validated against the 64 KB cap before the
/// request is issued.
///
/// On a 409 conflict the error message is rewritten via [`map_write_error`]
/// to guide the caller to re-read and retry.
pub async fn set(
    client: &ApiClient,
    scope: MemoryScope,
    id: &str,
    content: &str,
    revision: Option<u64>,
) -> Result<Value, CliError> {
    validate_id(scope.id_label(), id)?;
    validate_content(content)?;
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    if let Some(rev) = revision {
        form.insert("revision".to_owned(), rev.to_string());
    }
    client
        .post(&memory_path(scope, id), &form)
        .await
        .map_err(map_write_error)
}

/// Hard-delete the caller's AI memory row for a scope.
///
/// `DELETE /{org|workspace}/{id}/ai/memory/`. Subsequent reads return the
/// stable empty envelope (`content: ""`, `revision: 0`).
pub async fn delete(client: &ApiClient, scope: MemoryScope, id: &str) -> Result<Value, CliError> {
    validate_id(scope.id_label(), id)?;
    client.delete(&memory_path(scope, id)).await
}

#[cfg(test)]
mod tests {
    use super::{
        MEMORY_MAX_BYTES, MemoryScope, map_write_error, memory_path, validate_content, validate_id,
    };
    use crate::error::{ApiError, CliError};

    #[test]
    fn org_path_is_correct() {
        assert_eq!(
            memory_path(MemoryScope::Org, "1234567890123456789"),
            "/org/1234567890123456789/ai/memory/"
        );
    }

    #[test]
    fn workspace_path_is_correct() {
        assert_eq!(
            memory_path(MemoryScope::Workspace, "4687730903718774523"),
            "/workspace/4687730903718774523/ai/memory/"
        );
    }

    #[test]
    fn path_url_encodes_id() {
        // Opaque/hostile IDs must not break the path or smuggle segments.
        assert_eq!(
            memory_path(MemoryScope::Org, "a/b id"),
            "/org/a%2Fb%20id/ai/memory/"
        );
    }

    #[test]
    fn scope_segments_and_labels() {
        assert_eq!(MemoryScope::Org.segment(), "org");
        assert_eq!(MemoryScope::Workspace.segment(), "workspace");
        assert_eq!(MemoryScope::Org.id_label(), "org_id");
        assert_eq!(MemoryScope::Workspace.id_label(), "workspace_id");
    }

    #[test]
    fn validate_id_rejects_empty_and_whitespace() {
        assert!(validate_id("org_id", "").is_err());
        assert!(validate_id("workspace_id", "   ").is_err());
        assert!(validate_id("org_id", "abc").is_ok());
    }

    #[test]
    fn validate_content_accepts_empty() {
        // Empty string stores an empty row — it is NOT a delete.
        assert!(validate_content("").is_ok());
    }

    #[test]
    fn validate_content_accepts_boundary_max() {
        let s: String = "a".repeat(MEMORY_MAX_BYTES);
        assert!(validate_content(&s).is_ok());
    }

    #[test]
    fn validate_content_rejects_over_cap() {
        let s: String = "a".repeat(MEMORY_MAX_BYTES + 1);
        let err = validate_content(&s).expect_err("oversized payload must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn validate_content_counts_bytes_not_chars() {
        // U+1F600 GRINNING FACE is 4 bytes in UTF-8. 16384 of these is
        // exactly 65,536 bytes (the cap) — must pass; one more must fail.
        let s: String = "\u{1F600}".repeat(MEMORY_MAX_BYTES / 4);
        assert_eq!(s.len(), MEMORY_MAX_BYTES);
        assert!(validate_content(&s).is_ok());

        let s: String = "\u{1F600}".repeat(MEMORY_MAX_BYTES / 4 + 1);
        assert!(s.len() > MEMORY_MAX_BYTES);
        assert!(validate_content(&s).is_err());
    }

    #[test]
    fn map_write_error_rewrites_409_conflict() {
        let err = CliError::Api(ApiError::new(0, None, "Conflict".to_owned(), 409));
        let mapped = map_write_error(err);
        match mapped {
            CliError::Api(api) => {
                assert_eq!(api.http_status, 409);
                assert!(
                    api.message.contains("revision conflict"),
                    "409 should be rewritten with the re-get/retry hint, got: {}",
                    api.message
                );
            }
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[test]
    fn map_write_error_preserves_non_409() {
        // A 400 (over-cap) must pass through unchanged so the original
        // server message reaches the user.
        let err = CliError::Api(ApiError::new(
            1605,
            None,
            "content too large".to_owned(),
            400,
        ));
        let mapped = map_write_error(err);
        match mapped {
            CliError::Api(api) => {
                assert_eq!(api.http_status, 400);
                assert_eq!(api.message, "content too large");
            }
            other => panic!("expected CliError::Api, got {other:?}"),
        }
    }

    #[test]
    fn map_write_error_preserves_non_api_errors() {
        let err = CliError::Parse("bad".to_owned());
        let mapped = map_write_error(err);
        assert!(matches!(mapped, CliError::Parse(_)));
    }
}
