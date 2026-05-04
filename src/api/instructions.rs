#![allow(clippy::missing_errors_doc)]

/// AI instructions API endpoints for the Fast.io REST API.
///
/// All four profile types (user, org, workspace, share) share the same
/// shape: GET / POST / DELETE on a `/instructions/` path. The body for
/// the write call is a single `content` form field — a markdown blob
/// up to 65,536 *raw bytes* (multibyte chars count for more than one).
///
/// The aiinstructions.md spec describes this as PUT, but the server
/// actually accepts the write as POST (form-encoded), which is what we
/// send here. DELETE is a soft-clear and is idempotent. The first read
/// of an unwritten slot returns `content: ""` with null timestamps —
/// it is **not** a 404.
///
/// User profile is self-only and has no per-user `/me/` variant; the
/// other three each expose two scopes:
/// - profile-wide: `/{type}/{id}/instructions/` (admin/owner only)
/// - per-user override: `/{type}/{id}/instructions/me/`
///
/// Server-side merging does not happen — clients that want a merged
/// effective view fetch both and concatenate.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Server-enforced upper bound on the `content` payload, in raw bytes
/// (not characters). Multibyte UTF-8 chars consume more than 1 byte.
pub const INSTRUCTIONS_MAX_BYTES: usize = 65_536;

/// Validate the `content` payload before sending. Returns a
/// `CliError::Parse` describing the violation if the byte length
/// exceeds [`INSTRUCTIONS_MAX_BYTES`]. Empty strings are accepted —
/// the server treats them as a clear, equivalent to DELETE.
fn validate_content(content: &str) -> Result<(), CliError> {
    let len = content.len();
    if len > INSTRUCTIONS_MAX_BYTES {
        return Err(CliError::Parse(format!(
            "instructions content must be at most {INSTRUCTIONS_MAX_BYTES} bytes (got {len})",
        )));
    }
    Ok(())
}

/// Reject empty / whitespace-only profile IDs before they hit the
/// server. `urlencoding::encode("")` collapses to `""`, which would
/// produce paths like `/org//instructions/` — surfaces as an opaque
/// 404. Failing locally gives the caller a crisp message.
fn validate_id(label: &str, id: &str) -> Result<(), CliError> {
    if id.trim().is_empty() {
        return Err(CliError::Parse(format!("{label} must not be empty")));
    }
    Ok(())
}

/// Get the current user's self-scoped AI instructions.
///
/// `GET /user/me/instructions/`
pub async fn get_user_instructions(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/me/instructions/").await
}

/// Set the current user's self-scoped AI instructions.
///
/// `POST /user/me/instructions/` with form-encoded `content` field.
pub async fn set_user_instructions(client: &ApiClient, content: &str) -> Result<Value, CliError> {
    validate_content(content)?;
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    client.post("/user/me/instructions/", &form).await
}

/// Clear the current user's self-scoped AI instructions.
///
/// `DELETE /user/me/instructions/`
pub async fn delete_user_instructions(client: &ApiClient) -> Result<Value, CliError> {
    client.delete("/user/me/instructions/").await
}

/// Get the org-wide AI instructions (owner / admin only).
///
/// `GET /org/{org_id}/instructions/`
pub async fn get_org_instructions(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    validate_id("org_id", org_id)?;
    let path = format!("/org/{}/instructions/", urlencoding::encode(org_id));
    client.get(&path).await
}

/// Set the org-wide AI instructions (owner / admin only).
///
/// `POST /org/{org_id}/instructions/`
pub async fn set_org_instructions(
    client: &ApiClient,
    org_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    validate_id("org_id", org_id)?;
    validate_content(content)?;
    let path = format!("/org/{}/instructions/", urlencoding::encode(org_id));
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    client.post(&path, &form).await
}

/// Clear the org-wide AI instructions.
///
/// `DELETE /org/{org_id}/instructions/`
pub async fn delete_org_instructions(client: &ApiClient, org_id: &str) -> Result<Value, CliError> {
    validate_id("org_id", org_id)?;
    let path = format!("/org/{}/instructions/", urlencoding::encode(org_id));
    client.delete(&path).await
}

/// Get the calling user's per-user override of an org's AI instructions.
///
/// `GET /org/{org_id}/instructions/me/`
pub async fn get_org_user_instructions(
    client: &ApiClient,
    org_id: &str,
) -> Result<Value, CliError> {
    validate_id("org_id", org_id)?;
    let path = format!("/org/{}/instructions/me/", urlencoding::encode(org_id));
    client.get(&path).await
}

/// Set the calling user's per-user override of an org's AI instructions.
///
/// `POST /org/{org_id}/instructions/me/`
pub async fn set_org_user_instructions(
    client: &ApiClient,
    org_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    validate_id("org_id", org_id)?;
    validate_content(content)?;
    let path = format!("/org/{}/instructions/me/", urlencoding::encode(org_id));
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    client.post(&path, &form).await
}

/// Clear the calling user's per-user override of an org's AI instructions.
///
/// `DELETE /org/{org_id}/instructions/me/`
pub async fn delete_org_user_instructions(
    client: &ApiClient,
    org_id: &str,
) -> Result<Value, CliError> {
    validate_id("org_id", org_id)?;
    let path = format!("/org/{}/instructions/me/", urlencoding::encode(org_id));
    client.delete(&path).await
}

/// Get the workspace-wide AI instructions (owner / admin only).
///
/// `GET /workspace/{workspace_id}/instructions/`
pub async fn get_workspace_instructions(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    validate_id("workspace_id", workspace_id)?;
    let path = format!(
        "/workspace/{}/instructions/",
        urlencoding::encode(workspace_id),
    );
    client.get(&path).await
}

/// Set the workspace-wide AI instructions (owner / admin only).
///
/// `POST /workspace/{workspace_id}/instructions/`
pub async fn set_workspace_instructions(
    client: &ApiClient,
    workspace_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    validate_id("workspace_id", workspace_id)?;
    validate_content(content)?;
    let path = format!(
        "/workspace/{}/instructions/",
        urlencoding::encode(workspace_id),
    );
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    client.post(&path, &form).await
}

/// Clear the workspace-wide AI instructions.
///
/// `DELETE /workspace/{workspace_id}/instructions/`
pub async fn delete_workspace_instructions(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    validate_id("workspace_id", workspace_id)?;
    let path = format!(
        "/workspace/{}/instructions/",
        urlencoding::encode(workspace_id),
    );
    client.delete(&path).await
}

/// Get the calling user's per-user override of a workspace's AI
/// instructions. Blocked for guests (members only).
///
/// `GET /workspace/{workspace_id}/instructions/me/`
pub async fn get_workspace_user_instructions(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    validate_id("workspace_id", workspace_id)?;
    let path = format!(
        "/workspace/{}/instructions/me/",
        urlencoding::encode(workspace_id),
    );
    client.get(&path).await
}

/// Set the calling user's per-user override of a workspace's AI
/// instructions. Blocked for guests.
///
/// `POST /workspace/{workspace_id}/instructions/me/`
pub async fn set_workspace_user_instructions(
    client: &ApiClient,
    workspace_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    validate_id("workspace_id", workspace_id)?;
    validate_content(content)?;
    let path = format!(
        "/workspace/{}/instructions/me/",
        urlencoding::encode(workspace_id),
    );
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    client.post(&path, &form).await
}

/// Clear the calling user's per-user override of a workspace's AI
/// instructions.
///
/// `DELETE /workspace/{workspace_id}/instructions/me/`
pub async fn delete_workspace_user_instructions(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    validate_id("workspace_id", workspace_id)?;
    let path = format!(
        "/workspace/{}/instructions/me/",
        urlencoding::encode(workspace_id),
    );
    client.delete(&path).await
}

/// Get the share-wide AI instructions (owner / admin only).
///
/// `GET /share/{share_id}/instructions/`
pub async fn get_share_instructions(client: &ApiClient, share_id: &str) -> Result<Value, CliError> {
    validate_id("share_id", share_id)?;
    let path = format!("/share/{}/instructions/", urlencoding::encode(share_id));
    client.get(&path).await
}

/// Set the share-wide AI instructions (owner / admin only).
///
/// `POST /share/{share_id}/instructions/`
pub async fn set_share_instructions(
    client: &ApiClient,
    share_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    validate_id("share_id", share_id)?;
    validate_content(content)?;
    let path = format!("/share/{}/instructions/", urlencoding::encode(share_id));
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    client.post(&path, &form).await
}

/// Clear the share-wide AI instructions.
///
/// `DELETE /share/{share_id}/instructions/`
pub async fn delete_share_instructions(
    client: &ApiClient,
    share_id: &str,
) -> Result<Value, CliError> {
    validate_id("share_id", share_id)?;
    let path = format!("/share/{}/instructions/", urlencoding::encode(share_id));
    client.delete(&path).await
}

/// Get the calling user's per-user override of a share's AI
/// instructions. Registered share members only — anonymous/link guests
/// are blocked with HTTP 403, code 185733.
///
/// `GET /share/{share_id}/instructions/me/`
pub async fn get_share_user_instructions(
    client: &ApiClient,
    share_id: &str,
) -> Result<Value, CliError> {
    validate_id("share_id", share_id)?;
    let path = format!("/share/{}/instructions/me/", urlencoding::encode(share_id));
    client.get(&path).await
}

/// Set the calling user's per-user override of a share's AI
/// instructions. Registered members only.
///
/// `POST /share/{share_id}/instructions/me/`
pub async fn set_share_user_instructions(
    client: &ApiClient,
    share_id: &str,
    content: &str,
) -> Result<Value, CliError> {
    validate_id("share_id", share_id)?;
    validate_content(content)?;
    let path = format!("/share/{}/instructions/me/", urlencoding::encode(share_id));
    let mut form = HashMap::new();
    form.insert("content".to_owned(), content.to_owned());
    client.post(&path, &form).await
}

/// Clear the calling user's per-user override of a share's AI
/// instructions.
///
/// `DELETE /share/{share_id}/instructions/me/`
pub async fn delete_share_user_instructions(
    client: &ApiClient,
    share_id: &str,
) -> Result<Value, CliError> {
    validate_id("share_id", share_id)?;
    let path = format!("/share/{}/instructions/me/", urlencoding::encode(share_id));
    client.delete(&path).await
}

#[cfg(test)]
mod tests {
    use super::{INSTRUCTIONS_MAX_BYTES, validate_content, validate_id};
    use crate::error::CliError;

    #[test]
    fn validate_id_rejects_empty() {
        let err = validate_id("org_id", "").expect_err("empty id must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn validate_id_rejects_whitespace_only() {
        let err =
            validate_id("workspace_id", "   ").expect_err("whitespace-only id must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn validate_id_accepts_valid() {
        assert!(validate_id("share_id", "abc123").is_ok());
        assert!(validate_id("org_id", "1234567890123456789").is_ok());
    }

    #[test]
    fn validate_content_accepts_empty() {
        assert!(validate_content("").is_ok());
    }

    #[test]
    fn validate_content_accepts_short_payload() {
        assert!(validate_content("hello world").is_ok());
    }

    #[test]
    fn validate_content_accepts_boundary_max() {
        let s: String = "a".repeat(INSTRUCTIONS_MAX_BYTES);
        assert!(validate_content(&s).is_ok());
    }

    #[test]
    fn validate_content_rejects_over_cap() {
        let s: String = "a".repeat(INSTRUCTIONS_MAX_BYTES + 1);
        let err = validate_content(&s).expect_err("oversized payload must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn validate_content_counts_bytes_not_chars() {
        // U+1F600 GRINNING FACE is 4 bytes in UTF-8. A string of
        // 16384 of these is 65,536 bytes (exactly the cap) — must pass.
        let s: String = "\u{1F600}".repeat(INSTRUCTIONS_MAX_BYTES / 4);
        assert_eq!(s.len(), INSTRUCTIONS_MAX_BYTES);
        assert!(validate_content(&s).is_ok());

        // One more codepoint pushes it 4 bytes over and must be rejected.
        let s: String = "\u{1F600}".repeat(INSTRUCTIONS_MAX_BYTES / 4 + 1);
        assert!(s.len() > INSTRUCTIONS_MAX_BYTES);
        assert!(validate_content(&s).is_err());
    }
}
