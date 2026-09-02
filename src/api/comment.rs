#![allow(clippy::missing_errors_doc)]

/// Comment API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for comment CRUD and reactions on workspace/share files.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Parameters for [`list_comments`].
pub struct ListCommentsParams<'a> {
    /// Kind of parent entity (`workspace` or `share`).
    pub entity_type: &'a str,
    /// Unique identifier of the parent workspace or share.
    pub entity_id: &'a str,
    /// File/folder node within the entity to list comments for.
    pub node_id: &'a str,
    /// Sort order for results: `asc` or `desc` (server default `asc`). Forwarded
    /// verbatim as the `sort` query param.
    pub sort: Option<&'a str>,
    /// Maximum number of comments to return.
    pub limit: Option<u32>,
    /// Number of comments to skip for pagination.
    pub offset: Option<u32>,
}

/// List comments on a specific file.
///
/// `GET /comments/{entity_type}/{entity_id}/{node_id}/`
pub async fn list_comments(
    client: &ApiClient,
    params: &ListCommentsParams<'_>,
) -> Result<Value, CliError> {
    let mut query = HashMap::new();
    if let Some(v) = params.sort {
        query.insert("sort".to_owned(), v.to_owned());
    }
    if let Some(l) = params.limit {
        query.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = params.offset {
        query.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/comments/{}/{}/{}/",
        urlencoding::encode(params.entity_type),
        urlencoding::encode(params.entity_id),
        urlencoding::encode(params.node_id),
    );
    if query.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &query).await
    }
}

/// Parameters for [`add_comment`].
///
/// `body` (and optionally `parent_id`) are the only fields a plain comment or
/// reply needs; the remaining fields carry the optional create extensions
/// documented in comments.txt:
///
/// - `reference` — an anchoring reference object (see Reference Anchoring).
/// - `properties` — arbitrary key-value metadata.
/// - `target_id` / `target_ids` — inline attachment(s) on a **new** comment
///   (single or batch, ≤25). The server ignores these on an update; the
///   command/MCP layer only sends them on create.
pub struct AddCommentParams<'a> {
    /// Kind of parent entity (`workspace` or `share`).
    pub entity_type: &'a str,
    /// Unique identifier of the parent workspace or share.
    pub entity_id: &'a str,
    /// File/folder node the comment is anchored to.
    pub node_id: &'a str,
    /// Comment text content.
    pub body: &'a str,
    /// Parent comment ID for a single-level threaded reply.
    pub parent_id: Option<&'a str>,
    /// Optional anchoring reference (JSON object) into a file position.
    pub reference: Option<&'a Value>,
    /// Optional arbitrary key-value metadata (JSON object).
    pub properties: Option<&'a Value>,
    /// Inline single attachment (new comment only).
    pub target_id: Option<&'a str>,
    /// Inline multiple attachments (new comment only, ≤25).
    pub target_ids: Option<&'a [String]>,
}

/// Build the request body for [`add_comment`].
///
/// Extracted as a pure function so the body construction is testable without a
/// network round-trip. Only set fields are emitted, matching the server's
/// "omit to leave unset" convention.
fn build_add_comment_body(params: &AddCommentParams<'_>) -> Value {
    let mut body = serde_json::json!({ "body": params.body });
    if let Some(parent_id) = params.parent_id {
        body["parent_id"] = Value::String(parent_id.to_owned());
    }
    if let Some(reference) = params.reference {
        body["reference"] = reference.clone();
    }
    if let Some(properties) = params.properties {
        body["properties"] = properties.clone();
    }
    if let Some(target_id) = params.target_id {
        body["target_id"] = Value::String(target_id.to_owned());
    }
    if let Some(target_ids) = params.target_ids {
        body["target_ids"] = serde_json::json!(target_ids);
    }
    body
}

/// Maximum comment `body` length, in **characters (code points)** — the
/// platform's authoritative unit, confirmed after its fix `c6eb446b38`.
///
/// Over-limit is a **hard rejection**, never truncation: `162417` on create,
/// `164797` on update.
///
/// **RESOLVED 2026-08-07 — the deploy window this warned about is closed.**
/// Measured at the boundary: 200 CJK (600 B) and 500 CJK (1 500 B)
/// accepted, 501 rejected with `166910` reporting *"has 501 characters"*. The
/// server counts characters and says so. The byte-window caveat that stood here
/// was removed from the user-facing hints the same day.
///
/// History, because the reasoning outlived the window: the server was still
/// byte-counting on 2026-08-06 (`strlen` against a `varchar(8192)` CHARACTER
/// column), so a CJK or emoji author was refused sooner than this bound allows.
/// It could never make this check wrongly refuse, because `chars <= bytes` for
/// every string means a character bound is at worst lenient against a byte
/// server. That framing is why this stayed correct across the platform's
/// reversal, and it is the durable half.
///
/// **The 500-limit on visible text is deliberately NOT checked here** — it
/// discounts mention markup, and this crate has no mention grammar, so it cannot
/// be computed correctly. The server owns it (`166910`).
///
/// That discount is also **conditional**, which only widens the gap: measured
/// 2026-08-10 with a pair differing solely by a code fence, 460 visible
/// characters plus 76 characters of UNFENCED `@[file:…]` markup was accepted,
/// while the same 460 plus the SAME markup inside a fence was rejected
/// (`166910`). Computing the visible length therefore needs the server's fence
/// rules on top of its mention grammar — two things this crate has neither of.
/// Treat `166910` as the authority and read the count from its message.
pub const COMMENT_BODY_MAX_CHARS: usize = 8192;

/// Validate a comment body before sending it.
///
/// See [`COMMENT_BODY_MAX_CHARS`] for the unit and for why the 500-limit on
/// visible text is left to the server.
///
/// # Errors
/// [`CliError::Parse`] when the body is blank after trimming, or longer than
/// [`COMMENT_BODY_MAX_CHARS`] characters.
fn validate_comment_body(text: &str) -> Result<(), CliError> {
    if text.trim().is_empty() {
        return Err(CliError::Parse("comment body must not be empty".to_owned()));
    }
    let chars = text.chars().count();
    if chars > COMMENT_BODY_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "comment body must be at most {COMMENT_BODY_MAX_CHARS} characters (got {chars})"
        )));
    }
    Ok(())
}

/// Add a comment to a file (or reply to one via `parent_id`).
///
/// `POST /comments/{entity_type}/{entity_id}/{node_id}/`
pub async fn add_comment(
    client: &ApiClient,
    params: &AddCommentParams<'_>,
) -> Result<Value, CliError> {
    validate_comment_body(params.body)?;
    let body = build_add_comment_body(params);
    let path = format!(
        "/comments/{}/{}/{}/",
        urlencoding::encode(params.entity_type),
        urlencoding::encode(params.entity_id),
        urlencoding::encode(params.node_id),
    );
    client.post_json(&path, &body).await
}

/// Edit an existing comment by ID.
///
/// `POST /comments/{comment_id}/update/`
/// Author-only. Works for every comment surface — workspace, share, node, and
/// File Share. The edit cannot move the comment; entity, scope, and threading
/// are immutable.
pub async fn update_comment(
    client: &ApiClient,
    comment_id: &str,
    text: &str,
) -> Result<Value, CliError> {
    validate_comment_body(text)?;
    let body = serde_json::json!({ "body": text });
    let path = format!("/comments/{}/update/", urlencoding::encode(comment_id));
    client.post_json(&path, &body).await
}

/// Delete a comment.
///
/// `DELETE /comments/{comment_id}/delete/`
pub async fn delete_comment(client: &ApiClient, comment_id: &str) -> Result<Value, CliError> {
    let path = format!("/comments/{}/delete/", urlencoding::encode(comment_id),);
    client.delete(&path).await
}

/// Get comment details.
///
/// `GET /comments/{comment_id}/details/`
pub async fn get_comment_details(client: &ApiClient, comment_id: &str) -> Result<Value, CliError> {
    let path = format!("/comments/{}/details/", urlencoding::encode(comment_id),);
    client.get(&path).await
}

/// List all comments across a workspace or share.
///
/// `GET /comments/{entity_type}/{entity_id}/`
pub async fn list_all_comments(
    client: &ApiClient,
    entity_type: &str,
    entity_id: &str,
    sort: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(v) = sort {
        params.insert("sort".to_owned(), v.to_owned());
    }
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/comments/{}/{}/",
        urlencoding::encode(entity_type),
        urlencoding::encode(entity_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add an emoji reaction to a comment.
///
/// `POST /comments/{comment_id}/reactions/`
pub async fn add_reaction(
    client: &ApiClient,
    comment_id: &str,
    emoji: &str,
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "emoji": emoji });
    let path = format!("/comments/{}/reactions/", urlencoding::encode(comment_id));
    client.post_json(&path, &body).await
}

/// Remove an emoji reaction from a comment.
///
/// `DELETE /comments/{comment_id}/reactions/`
pub async fn remove_reaction(client: &ApiClient, comment_id: &str) -> Result<Value, CliError> {
    let path = format!("/comments/{}/reactions/", urlencoding::encode(comment_id));
    client.delete(&path).await
}

/// Bulk-delete multiple comments.
///
/// `POST /comments/bulk/delete/`
pub async fn bulk_delete_comments(
    client: &ApiClient,
    comment_ids: &[String],
) -> Result<Value, CliError> {
    let body = serde_json::json!({ "comment_ids": comment_ids });
    client.post_json("/comments/bulk/delete/", &body).await
}

// ─── Comment Attachments ──────────────────────────────────────────────────────

/// Selector for which object(s) to attach to a comment.
///
/// Mirrors the server's `target_id` (single) / `target_ids` (batch, ≤25) body
/// fields on `POST /comments/{comment_id}/attachments/`. The mutual exclusion
/// (and the "at least one" requirement) is enforced by the CLI/MCP layer.
pub enum CommentAttachTargets<'a> {
    /// Attach a single object (`target_id`).
    Single(&'a str),
    /// Attach multiple objects (`target_ids`, ≤25 per comment).
    Multiple(&'a [String]),
}

/// Build the request body for [`attach_comment`].
///
/// Extracted as a pure function so the body construction is testable without a
/// network round-trip.
fn build_attach_body(targets: &CommentAttachTargets<'_>) -> Value {
    match targets {
        CommentAttachTargets::Single(id) => serde_json::json!({ "target_id": id }),
        CommentAttachTargets::Multiple(ids) => serde_json::json!({ "target_ids": ids }),
    }
}

/// List the objects attached to a comment (hydrated, access-gated).
///
/// `GET /comments/{comment_id}/attachments/`
pub async fn list_comment_attachments(
    client: &ApiClient,
    comment_id: &str,
) -> Result<Value, CliError> {
    let path = format!("/comments/{}/attachments/", urlencoding::encode(comment_id),);
    client.get(&path).await
}

/// Attach one object (`target_id`) or many (`target_ids`) to a comment.
///
/// `POST /comments/{comment_id}/attachments/`. Idempotent (already-attached
/// objects are skipped) and atomic (a partial-batch failure attaches nothing);
/// author-only — the server enforces it. Returns the full updated hydrated
/// attachment list.
pub async fn attach_comment(
    client: &ApiClient,
    comment_id: &str,
    targets: &CommentAttachTargets<'_>,
) -> Result<Value, CliError> {
    let body = build_attach_body(targets);
    let path = format!("/comments/{}/attachments/", urlencoding::encode(comment_id),);
    client.post_json(&path, &body).await
}

/// Build the request body for [`detach_comment`].
fn build_detach_body(target_id: &str) -> Value {
    serde_json::json!({ "target_id": target_id })
}

/// Detach a single object from a comment by its `target_id`.
///
/// `POST /comments/{comment_id}/attachments/detach/`. Idempotent — detaching an
/// object that is not attached returns success with `removed: false`. Returns
/// the updated hydrated attachment list.
pub async fn detach_comment(
    client: &ApiClient,
    comment_id: &str,
    target_id: &str,
) -> Result<Value, CliError> {
    let body = build_detach_body(target_id);
    let path = format!(
        "/comments/{}/attachments/detach/",
        urlencoding::encode(comment_id),
    );
    client.post_json(&path, &body).await
}

#[cfg(test)]
mod tests {
    use super::{
        AddCommentParams, COMMENT_BODY_MAX_CHARS, CommentAttachTargets, build_add_comment_body,
        build_attach_body, build_detach_body, validate_comment_body,
    };
    use serde_json::json;

    #[test]
    fn comment_body_max_is_characters_so_it_is_safe_in_both_windows() {
        assert!(validate_comment_body("hello").is_ok());
        assert!(validate_comment_body("").is_err());
        assert!(validate_comment_body("   ").is_err(), "blank after trim");

        // Boundary, in CHARACTERS.
        assert!(validate_comment_body(&"a".repeat(COMMENT_BODY_MAX_CHARS)).is_ok());
        assert!(validate_comment_body(&"a".repeat(COMMENT_BODY_MAX_CHARS + 1)).is_err());

        // The load-bearing property: a 3 000-character CJK body is 9 000 BYTES.
        // When this was written the byte server rejected it (162417) and the
        // client deliberately did not — the bound means characters, so the same
        // input is valid, and a byte mirror would have blocked it permanently.
        // Measured 2026-08-07: the server now counts characters too.
        //
        let cjk = "猫".repeat(3000);
        assert_eq!(cjk.chars().count(), 3000, "well under 8192 CHARACTERS");
        assert!(cjk.len() > 8192, "but well over 8192 BYTES");
        assert!(
            validate_comment_body(&cjk).is_ok(),
            "a character bound must stay lenient where the byte server is strict"
        );

        // And the converse direction can never happen: rejecting on characters
        // implies the byte count is at least as large, so anything this check
        // refuses today's byte server refuses too.
        let over = "猫".repeat(COMMENT_BODY_MAX_CHARS + 1);
        assert!(validate_comment_body(&over).is_err());
        assert!(over.len() > COMMENT_BODY_MAX_CHARS, "chars <= bytes always");
    }

    /// The over-length message names CHARACTERS and must not resurrect the
    /// retired byte-window caveat.
    ///
    /// This guard exists because the caveat outlived its window HERE by two
    /// days. It was removed from the `HINT_COMMENT_*` constants on 2026-08-07,
    /// the same hour the server was measured counting characters, and a test
    /// was written to keep it out — but that test's scope was the hints, and
    /// this string is a `CliError::Parse` built in `validate_comment_body`. The
    /// stale text sat one module away from a guard aimed at exactly it.
    ///
    /// So the negative half below is the point, not decoration: it is the check
    /// whose absence let a wrong sentence reach users for two days. Lowercase
    /// the haystack — the string that got away spelled it `BYTES`, and a
    /// case-sensitive needle would have missed it.
    #[test]
    fn over_length_message_states_characters_and_no_byte_caveat() {
        let over = "a".repeat(COMMENT_BODY_MAX_CHARS + 1);
        let Err(err) = validate_comment_body(&over) else {
            panic!("over-length body must be refused");
        };
        let msg = err.to_string();

        // Positive: the unit is named, and the actual count is reported so the
        // author can see how far over they are.
        assert!(
            msg.contains("characters"),
            "message must name the unit, got: {msg}"
        );
        assert!(
            msg.contains(&(COMMENT_BODY_MAX_CHARS + 1).to_string()),
            "message must report the measured length, got: {msg}"
        );

        // Negative: no byte vocabulary in any casing.
        assert!(
            !msg.to_lowercase().contains("byte"),
            "the byte-window caveat was retired 2026-08-07 — the server counts \
             characters and reports them; got: {msg}"
        );
    }

    #[test]
    fn add_comment_body_minimal_is_body_only() {
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: None,
            reference: None,
            properties: None,
            target_id: None,
            target_ids: None,
        });
        assert_eq!(body, json!({ "body": "hi" }));
    }

    #[test]
    fn add_comment_body_includes_reference_and_properties() {
        // `document` + a `page` anchor — the documented shape. This fixture
        // said `{"type":"page"}`, which the server REJECTS ("Invalid reference
        // type"). The test still passed because it asserts the body is
        // FORWARDED, not that the value is valid — so it could never have
        // caught the help text that told users the same wrong thing.
        let reference = json!({ "type": "document", "page": 3 });
        let properties = json!({ "k": "v" });
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: Some("p1"),
            reference: Some(&reference),
            properties: Some(&properties),
            target_id: None,
            target_ids: None,
        });
        assert_eq!(body["parent_id"], json!("p1"));
        assert_eq!(body["reference"], reference);
        assert_eq!(body["properties"], properties);
    }

    #[test]
    fn add_comment_body_inline_single_target() {
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: None,
            reference: None,
            properties: None,
            target_id: Some("t1"),
            target_ids: None,
        });
        assert_eq!(body["target_id"], json!("t1"));
        assert!(body.get("target_ids").is_none());
    }

    #[test]
    fn add_comment_body_inline_multiple_targets() {
        let ids = vec!["a".to_owned(), "b".to_owned()];
        let body = build_add_comment_body(&AddCommentParams {
            entity_type: "workspace",
            entity_id: "1",
            node_id: "n",
            body: "hi",
            parent_id: None,
            reference: None,
            properties: None,
            target_id: None,
            target_ids: Some(&ids),
        });
        assert_eq!(body["target_ids"], json!(["a", "b"]));
        assert!(body.get("target_id").is_none());
    }

    #[test]
    fn attach_body_single_uses_target_id() {
        let body = build_attach_body(&CommentAttachTargets::Single("t1"));
        assert_eq!(body, json!({ "target_id": "t1" }));
    }

    #[test]
    fn attach_body_multiple_uses_target_ids() {
        let ids = vec!["a".to_owned(), "b".to_owned()];
        let body = build_attach_body(&CommentAttachTargets::Multiple(&ids));
        assert_eq!(body, json!({ "target_ids": ["a", "b"] }));
    }

    #[test]
    fn detach_body_uses_target_id() {
        let body = build_detach_body("t1");
        assert_eq!(body, json!({ "target_id": "t1" }));
    }
}
