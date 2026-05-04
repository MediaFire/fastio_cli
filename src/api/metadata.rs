#![allow(clippy::missing_errors_doc)]

/// Metadata extraction and template management API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for metadata-eligible files, template node management,
/// AI-based file matching, batch extraction, and single-node extraction.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List files eligible for metadata extraction in a workspace.
///
/// `GET /workspace/{workspace_id}/metadata/eligible/`
pub async fn list_eligible(
    client: &ApiClient,
    workspace_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/metadata/eligible/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add files to a metadata template.
///
/// The server enforces a per-template node cap that varies by plan tier
/// (Free/Agent/Pro: 10, Business: 1000). If `current + nodes_to_add` would
/// exceed the cap, the call returns `400` with API error code `1605` and
/// a message naming the cap, the attempted count, and the remaining slots.
/// Callers that bulk-add should pre-flight by reading the template details
/// (for `plan_node_limit` / `total_count_unfiltered`) and only send what
/// fits.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/nodes/add/`
pub async fn add_nodes_to_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/nodes/add/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let mut form = HashMap::new();
    form.insert("node_ids".to_owned(), node_ids.to_owned());
    client.post(&path, &form).await
}

/// Remove files from a metadata template.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/nodes/remove/`
pub async fn remove_nodes_from_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/nodes/remove/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let mut form = HashMap::new();
    form.insert("node_ids".to_owned(), node_ids.to_owned());
    client.post(&path, &form).await
}

/// List files mapped to a metadata template.
///
/// Response includes the per-template node rows plus these plan-aware
/// additive fields: `plan_node_limit` (per-template cap; `-1` is unlimited),
/// `total_count` (capped row count), `total_count_unfiltered` (true row
/// count in storage), and `is_truncated` (true when storage holds more rows
/// than the plan permits to surface). Each row also carries an
/// `autoextractable` boolean — `true` when the node is a file, not
/// trashed, and has a completed AI summary — which callers can use to
/// gate "extract now" / "re-extract" affordances without a follow-up
/// probe. Pagination is server-clamped: `offset + limit` cannot exceed
/// `plan_node_limit`. The default sort order is currently subject to
/// change pending a server-side fix; pass an explicit order if you depend
/// on a particular ordering.
///
/// `GET /workspace/{workspace_id}/metadata/templates/{template_id}/nodes/`
pub async fn list_template_nodes(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    sort_field: Option<&str>,
    sort_dir: Option<&str>,
) -> Result<Value, CliError> {
    let sort_field = sort_field.filter(|s| !s.trim().is_empty());
    let sort_dir = sort_dir.filter(|s| !s.trim().is_empty());
    validate_sort_params(sort_field, sort_dir)?;
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if let Some(field) = sort_field {
        params.insert("sort_field".to_owned(), field.to_owned());
    }
    if let Some(dir) = sort_dir {
        params.insert("sort_dir".to_owned(), dir.to_owned());
    }
    let path = format!(
        "/workspace/{}/metadata/templates/{}/nodes/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// AI-based file matching for a metadata template.
///
/// The server silently caps the number of matched files at the workspace's
/// `plan_node_limit` to avoid burning credits on rows the listing would
/// hide anyway. The response shape is unchanged; check `template details`
/// or call `preview-match` first to see how many files would be admitted.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/auto-match/`
pub async fn auto_match_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/auto-match/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let form = HashMap::new();
    client.post(&path, &form).await
}

/// Batch-extract metadata for all files mapped to a template.
///
/// `POST /workspace/{workspace_id}/metadata/templates/{template_id}/extract-all/`
pub async fn extract_all(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/templates/{}/extract-all/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id),
    );
    let form = HashMap::new();
    client.post(&path, &form).await
}

/// Enqueue an asynchronous metadata extraction for a single storage node.
///
/// **Async contract (breaking change from the prior synchronous version):**
/// the server returns `202 Accepted` within ~500ms with a payload of the
/// shape `{ result, job_id, template_id, node_id, fields, status,
/// status_uri }`. The extracted values are **not** in this response —
/// callers must poll [`crate::api::workspace::jobs_status`] for an entry
/// in `metadata_extract` with `kind: "single"` matching this `node_id`,
/// then read the values from
/// `GET /workspace/{ws}/storage/{node}/metadata/details/` once the job
/// reports `status: "completed"`. On `status: "errored"`, surface
/// `error_message` to the user.
///
/// `fields` is an optional JSON-encoded array of template field names for
/// partial extraction. Pass `None` for a full-row extraction: the server
/// runs only the autoextract-eligible subset (fields with
/// `autoextract: true` or omitted). Passing an explicit list is treated
/// as a manual override — the listed fields are extracted verbatim,
/// including any opted-out columns. Different `fields` subsets coexist as
/// independent jobs.
///
/// **Empty-scope corner case:** if a full-row call resolves to an empty
/// effective scope (e.g. every template field has `autoextract: false`),
/// the server responds successfully but does not enqueue a job — the
/// response will not contain a `job_id`. Callers should not assume one
/// is present.
///
/// **Idempotency:** the server dedupes on `(node_id, template_id,
/// fields_scope)` while a prior job is in flight, returning the existing
/// `job_id` instead of enqueueing a duplicate. No client-side debounce
/// required. The node must be `autoextractable` (a file, not a folder,
/// not trashed, with a completed AI summary); check the
/// `GET /metadata/details/` response's top-level `autoextractable` field
/// before calling to gate "extract now" affordances.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/metadata/extract/`
pub async fn extract_node_metadata(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    template_id: &str,
    fields: Option<&str>,
) -> Result<Value, CliError> {
    if let Some(f) = fields {
        validate_extract_fields(f)?;
    }
    let path = format!(
        "/workspace/{}/storage/{}/metadata/extract/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    let mut form = HashMap::new();
    form.insert("template_id".to_owned(), template_id.to_owned());
    if let Some(f) = fields {
        form.insert("fields".to_owned(), f.to_owned());
    }
    client.post(&path, &form).await
}

/// Preview files that match a proposed metadata template description.
///
/// Used by the view-creation flow to surface candidate files before a
/// template is persisted. Paired with [`suggest_fields`] to drive the
/// AI-assisted "create view" wizard. Response includes additive plan-aware
/// fields: `plan_node_limit`, `would_truncate_at` (= `min(total_matched,
/// plan_node_limit)`), `total_eligible`, `total_scanned`, `total_matched`,
/// and `matched_files[]`, so callers can warn the user before they trigger
/// an `auto-match` that would silently cap.
///
/// `POST /workspace/{workspace_id}/metadata/templates/preview-match/`
pub async fn preview_match(
    client: &ApiClient,
    workspace_id: &str,
    name: &str,
    description: &str,
) -> Result<Value, CliError> {
    validate_name(name)?;
    validate_description(description)?;
    let path = format!(
        "/workspace/{}/metadata/templates/preview-match/",
        urlencoding::encode(workspace_id),
    );
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    form.insert("description".to_owned(), description.to_owned());
    client.post(&path, &form).await
}

/// Ask the server to suggest 1-5 custom columns for a proposed template.
///
/// Returns a `suggested_fields` array directly compatible with the `fields`
/// parameter of [`create_template`]. Each suggested field may also carry
/// a display-only `example_value` that the server strips when round-tripped
/// into create-template (unknown keys are ignored), so callers can pass
/// the array straight through. `node_ids` must be a JSON-stringified array
/// of 1-25 node IDs (typically sampled from [`preview_match`] results).
/// `user_context` is an optional short hint (at most 64 chars; letters,
/// numbers, and spaces only) that helps bias the suggestions - e.g.
/// `"photo collection"`. Rate limited; concurrent calls per user+workspace
/// return `409 Conflict` — retry after a short backoff.
///
/// `POST /workspace/{workspace_id}/metadata/templates/suggest-fields/`
pub async fn suggest_fields(
    client: &ApiClient,
    workspace_id: &str,
    node_ids: &str,
    description: &str,
    user_context: Option<&str>,
) -> Result<Value, CliError> {
    validate_description(description)?;
    validate_node_ids(node_ids)?;
    let user_context = user_context.filter(|s| !s.trim().is_empty());
    if let Some(ctx) = user_context {
        validate_user_context(ctx)?;
    }
    let path = format!(
        "/workspace/{}/metadata/templates/suggest-fields/",
        urlencoding::encode(workspace_id),
    );
    let mut form = HashMap::new();
    form.insert("node_ids".to_owned(), node_ids.to_owned());
    form.insert("description".to_owned(), description.to_owned());
    if let Some(ctx) = user_context {
        form.insert("user_context".to_owned(), ctx.to_owned());
    }
    client.post(&path, &form).await
}

/// Create a metadata template (a.k.a. "view" in the Fast.io UI).
///
/// `fields` is a JSON-stringified array of column definitions - each with
/// `name`, `type`, `description`, and optional `max`/`min`/`can_be_null`/
/// `fixed_list`/`autoextract`. The output of [`suggest_fields`] is drop-in
/// compatible after user review.
///
/// The optional per-field `autoextract` boolean (default `true`) controls
/// whether the column participates in automatic extraction jobs. Set it
/// to `false` on columns you want to manage manually (user notes, review
/// flags) - they are skipped by extraction but still writeable via the
/// custom KV endpoints. At least one field must have `autoextract` true
/// (or omit the key); the server rejects an all-opted-out template with
/// API error `1605`. [`validate_fields`] mirrors this check client-side.
///
/// Available on every plan (Free/Agent: 1 template per workspace, Pro: 2,
/// Business: 10). The server enforces the per-workspace template count and
/// rejects with a plan-tier error if the cap is exceeded.
///
/// `POST /workspace/{workspace_id}/metadata/templates/`
pub async fn create_template(
    client: &ApiClient,
    workspace_id: &str,
    name: &str,
    description: &str,
    category: &str,
    fields: &str,
) -> Result<Value, CliError> {
    validate_name(name)?;
    validate_category(category)?;
    validate_description(description)?;
    validate_fields(fields)?;
    let path = format!(
        "/workspace/{}/metadata/templates/",
        urlencoding::encode(workspace_id),
    );
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    form.insert("description".to_owned(), description.to_owned());
    form.insert("category".to_owned(), category.to_owned());
    form.insert("fields".to_owned(), fields.to_owned());
    client.post(&path, &form).await
}

/// Maximum character count for a template description.
const DESCRIPTION_MAX_CHARS: usize = 2000;
/// Maximum character count for the optional `user_context` hint.
const USER_CONTEXT_MAX_CHARS: usize = 64;
/// Maximum number of node IDs accepted by `suggest-fields`.
const SUGGEST_NODE_IDS_MAX: usize = 25;
/// Maximum character count for a template name.
const NAME_MAX_CHARS: usize = 255;
/// Maximum character count for a template category slug.
const CATEGORY_MAX_CHARS: usize = 50;

fn validate_name(name: &str) -> Result<(), CliError> {
    if name.trim().is_empty() {
        return Err(CliError::Parse("name must not be empty".to_owned()));
    }
    let len = name.chars().count();
    if len > NAME_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "name must be at most {NAME_MAX_CHARS} chars (got {len})",
        )));
    }
    Ok(())
}

fn validate_category(category: &str) -> Result<(), CliError> {
    if category.trim().is_empty() {
        return Err(CliError::Parse("category must not be empty".to_owned()));
    }
    let len = category.chars().count();
    if len > CATEGORY_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "category must be at most {CATEGORY_MAX_CHARS} chars (got {len})",
        )));
    }
    Ok(())
}

fn validate_description(description: &str) -> Result<(), CliError> {
    if description.trim().is_empty() {
        return Err(CliError::Parse("description must not be empty".to_owned()));
    }
    let len = description.chars().count();
    if len > DESCRIPTION_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "description must be at most {DESCRIPTION_MAX_CHARS} chars (got {len})",
        )));
    }
    Ok(())
}

fn validate_node_ids(node_ids: &str) -> Result<(), CliError> {
    let parsed: Vec<String> = serde_json::from_str(node_ids)
        .map_err(|e| CliError::Parse(format!("node_ids must be a JSON array of strings: {e}")))?;
    if parsed.is_empty() {
        return Err(CliError::Parse(
            "node_ids must contain at least 1 ID".to_owned(),
        ));
    }
    if parsed.len() > SUGGEST_NODE_IDS_MAX {
        return Err(CliError::Parse(format!(
            "node_ids may contain at most {SUGGEST_NODE_IDS_MAX} IDs (got {})",
            parsed.len(),
        )));
    }
    if parsed.iter().any(|s| s.trim().is_empty()) {
        return Err(CliError::Parse(
            "node_ids entries must not be empty".to_owned(),
        ));
    }
    Ok(())
}

/// Returns true for the 17 codepoints in Unicode general category `Zs`
/// (Space Separator) as of Unicode 15. Deliberately excludes `Zl` (U+2028)
/// and `Zp` (U+2029), which Rust's `char::is_whitespace()` would otherwise
/// admit. Used by [`validate_user_context`] to mirror the server-side
/// regex `^[\p{L}\p{N}\p{Zs}]*$`.
fn is_unicode_space_separator(c: char) -> bool {
    matches!(
        c,
        '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            // U+2000..=U+200A is a contiguous Zs block (EN QUAD..HAIR SPACE).
            | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

fn validate_user_context(ctx: &str) -> Result<(), CliError> {
    let len = ctx.chars().count();
    if len > USER_CONTEXT_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "user_context must be at most {USER_CONTEXT_MAX_CHARS} chars (got {len})",
        )));
    }
    // Mirrors server-side regex /^[\p{L}\p{N}\p{Zs}]*$/u as closely as the
    // stdlib allows. `is_alphanumeric` is a slight superset of \p{L}\p{N}
    // (it admits Other_Alphabetic combining marks), so a small number of
    // exotic combining-mark inputs may pass here and be rejected server-side.
    // The server is authoritative for that edge.
    if ctx
        .chars()
        .any(|c| !(c.is_alphanumeric() || is_unicode_space_separator(c)))
    {
        return Err(CliError::Parse(
            "user_context may only contain letters, numbers, and spaces".to_owned(),
        ));
    }
    Ok(())
}

/// Validate the optional `fields` argument for `extract_node_metadata`.
///
/// Accepts a JSON-encoded array of 1 or more non-blank field names. The
/// server treats an absent value as a full-row extraction, so an empty
/// array would be ambiguous and is rejected here.
fn validate_extract_fields(fields: &str) -> Result<(), CliError> {
    let parsed: Vec<String> = serde_json::from_str(fields).map_err(|e| {
        CliError::Parse(format!(
            "extract fields must be a JSON array of strings: {e}",
        ))
    })?;
    if parsed.is_empty() {
        return Err(CliError::Parse(
            "extract fields must contain at least 1 field name (omit the param for a full-row extraction)".to_owned(),
        ));
    }
    if parsed.iter().any(|s| s.trim().is_empty()) {
        return Err(CliError::Parse(
            "extract fields entries must not be empty".to_owned(),
        ));
    }
    Ok(())
}

/// Validate the `sort_field` / `sort_dir` pair for `list_template_nodes`.
///
/// Per docs, `sort_dir` is only meaningful when `sort_field` is set, and
/// `sort_dir` must be one of `asc` or `desc`.
fn validate_sort_params(sort_field: Option<&str>, sort_dir: Option<&str>) -> Result<(), CliError> {
    if let Some(dir) = sort_dir {
        validate_sort_dir(dir)?;
    }
    if sort_dir.is_some() && sort_field.is_none() {
        return Err(CliError::Parse("sort_dir requires sort_field".to_owned()));
    }
    Ok(())
}

fn validate_sort_dir(dir: &str) -> Result<(), CliError> {
    if matches!(dir, "asc" | "desc") {
        Ok(())
    } else {
        Err(CliError::Parse(
            "sort_dir must be either \"asc\" or \"desc\"".to_owned(),
        ))
    }
}

fn validate_fields(fields: &str) -> Result<(), CliError> {
    let parsed: Vec<Value> = serde_json::from_str(fields)
        .map_err(|e| CliError::Parse(format!("fields must be a JSON array: {e}")))?;
    if parsed.is_empty() {
        return Err(CliError::Parse(
            "fields must contain at least 1 column definition".to_owned(),
        ));
    }
    // Each entry must be a JSON object (the server rejects bare strings /
    // numbers / nulls, so we fail fast with a crisp message).
    for (i, field) in parsed.iter().enumerate() {
        if !field.is_object() {
            return Err(CliError::Parse(format!(
                "fields[{i}] must be a JSON object with name/type/description",
            )));
        }
        // If `autoextract` is present, it must be a boolean. Anything else
        // (string "false", integer 0, null) is rejected rather than
        // silently defaulted.
        if let Some(v) = field.get("autoextract")
            && !v.is_boolean()
        {
            return Err(CliError::Parse(format!(
                "fields[{i}].autoextract must be a boolean",
            )));
        }
    }
    // Mirror the server-side rule (API error 1605): at least one field
    // must participate in automatic extraction. A missing `autoextract`
    // key defaults to true, so fields without the key satisfy the rule.
    let any_autoextract = parsed.iter().any(|field| match field.get("autoextract") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => false,
    });
    if !any_autoextract {
        return Err(CliError::Parse(
            "fields must contain at least 1 column with autoextract=true (or with the key omitted)"
                .to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CATEGORY_MAX_CHARS, DESCRIPTION_MAX_CHARS, NAME_MAX_CHARS, SUGGEST_NODE_IDS_MAX,
        USER_CONTEXT_MAX_CHARS, validate_category, validate_description, validate_extract_fields,
        validate_fields, validate_name, validate_node_ids, validate_sort_dir, validate_sort_params,
        validate_user_context,
    };

    #[test]
    fn description_rejects_empty_or_whitespace() {
        assert!(validate_description("").is_err());
        assert!(validate_description("   ").is_err());
    }

    #[test]
    fn description_rejects_too_long() {
        let s: String = "x".repeat(DESCRIPTION_MAX_CHARS + 1);
        assert!(validate_description(&s).is_err());
    }

    #[test]
    fn description_accepts_boundary_values() {
        assert!(validate_description("x").is_ok());
        let s: String = "x".repeat(DESCRIPTION_MAX_CHARS);
        assert!(validate_description(&s).is_ok());
    }

    #[test]
    fn description_accepts_valid() {
        assert!(validate_description("photo collection").is_ok());
    }

    #[test]
    fn node_ids_rejects_malformed_json() {
        assert!(validate_node_ids("not json").is_err());
        assert!(validate_node_ids("{}").is_err());
        assert!(validate_node_ids("[null]").is_err());
    }

    #[test]
    fn node_ids_rejects_empty_array() {
        assert!(validate_node_ids("[]").is_err());
    }

    #[test]
    fn node_ids_rejects_over_limit() {
        let ids: Vec<String> = (0..=SUGGEST_NODE_IDS_MAX)
            .map(|i| format!("id{i}"))
            .collect();
        let json = serde_json::to_string(&ids).expect("serialize ids");
        assert!(validate_node_ids(&json).is_err());
    }

    #[test]
    fn node_ids_accepts_boundary_max() {
        let ids: Vec<String> = (0..SUGGEST_NODE_IDS_MAX)
            .map(|i| format!("id{i}"))
            .collect();
        let json = serde_json::to_string(&ids).expect("serialize ids");
        assert!(validate_node_ids(&json).is_ok());
    }

    #[test]
    fn node_ids_rejects_blank_entry() {
        assert!(validate_node_ids(r#"["n1",""]"#).is_err());
        assert!(validate_node_ids(r#"["n1","   "]"#).is_err());
    }

    #[test]
    fn node_ids_accepts_valid() {
        assert!(validate_node_ids(r#"["n1","n2"]"#).is_ok());
    }

    #[test]
    fn user_context_rejects_too_long() {
        let s: String = "a".repeat(USER_CONTEXT_MAX_CHARS + 1);
        assert!(validate_user_context(&s).is_err());
    }

    #[test]
    fn user_context_accepts_boundary_max() {
        let s: String = "a".repeat(USER_CONTEXT_MAX_CHARS);
        assert!(validate_user_context(&s).is_ok());
    }

    #[test]
    fn user_context_rejects_special_chars_and_controls() {
        assert!(validate_user_context("hello!").is_err());
        assert!(validate_user_context("a\tb").is_err());
        assert!(validate_user_context("a\nb").is_err());
    }

    #[test]
    fn user_context_rejects_line_and_paragraph_separators() {
        // U+2028 LINE SEPARATOR (Zl) and U+2029 PARAGRAPH SEPARATOR (Zp)
        // are NOT in \p{Zs} and the server regex would reject them.
        assert!(validate_user_context("a\u{2028}b").is_err());
        assert!(validate_user_context("a\u{2029}b").is_err());
    }

    #[test]
    fn user_context_accepts_zs_separators() {
        // Sample of \p{Zs} codepoints that should be accepted.
        assert!(validate_user_context("a\u{00A0}b").is_ok()); // NO-BREAK SPACE
        assert!(validate_user_context("a\u{1680}b").is_ok()); // OGHAM SPACE MARK
        assert!(validate_user_context("a\u{2003}b").is_ok()); // EM SPACE
        assert!(validate_user_context("a\u{3000}b").is_ok()); // IDEOGRAPHIC SPACE
    }

    #[test]
    fn user_context_accepts_letters_numbers_spaces() {
        assert!(validate_user_context("photo collection 2026").is_ok());
        assert!(validate_user_context("").is_ok());
        assert!(validate_user_context("\u{00E9}t\u{00E9}").is_ok());
    }

    #[test]
    fn fields_rejects_malformed_or_empty() {
        assert!(validate_fields("{}").is_err());
        assert!(validate_fields("not json").is_err());
        assert!(validate_fields("[]").is_err());
    }

    #[test]
    fn fields_accepts_valid() {
        assert!(validate_fields(r#"[{"name":"Location","type":"string"}]"#).is_ok());
    }

    #[test]
    fn fields_rejects_all_autoextract_false() {
        // Server API error 1605: every field opted out of autoextract.
        let json = r#"[
            {"name":"Notes","type":"string","autoextract":false},
            {"name":"Reviewer","type":"string","autoextract":false}
        ]"#;
        assert!(validate_fields(json).is_err());
    }

    #[test]
    fn fields_accepts_mixed_autoextract() {
        let json = r#"[
            {"name":"Location","type":"string","autoextract":true},
            {"name":"Notes","type":"string","autoextract":false}
        ]"#;
        assert!(validate_fields(json).is_ok());
    }

    #[test]
    fn fields_accepts_missing_autoextract_key() {
        // Missing key defaults to true.
        let json = r#"[
            {"name":"Location","type":"string"},
            {"name":"Notes","type":"string","autoextract":false}
        ]"#;
        assert!(validate_fields(json).is_ok());
    }

    #[test]
    fn fields_rejects_non_object_entry() {
        assert!(validate_fields(r#"["Location"]"#).is_err());
        assert!(validate_fields("[null]").is_err());
        assert!(validate_fields("[42]").is_err());
        // Mixed: the bare string must not let the all-opted-out case slip through.
        assert!(validate_fields(r#"["Location", {"name":"X","autoextract":false}]"#).is_err());
    }

    #[test]
    fn fields_rejects_non_boolean_autoextract() {
        assert!(validate_fields(r#"[{"name":"X","autoextract":"false"}]"#).is_err());
        assert!(validate_fields(r#"[{"name":"X","autoextract":0}]"#).is_err());
        assert!(validate_fields(r#"[{"name":"X","autoextract":null}]"#).is_err());
    }

    #[test]
    fn sort_params_rejects_sort_dir_without_sort_field() {
        assert!(validate_sort_params(None, Some("asc")).is_err());
        assert!(validate_sort_params(None, Some("desc")).is_err());
    }

    #[test]
    fn sort_params_accepts_valid_pairs() {
        assert!(validate_sort_params(None, None).is_ok());
        assert!(validate_sort_params(Some("field1"), None).is_ok());
        assert!(validate_sort_params(Some("field1"), Some("asc")).is_ok());
        assert!(validate_sort_params(Some("field1"), Some("desc")).is_ok());
    }

    #[test]
    fn sort_params_rejects_invalid_sort_dir() {
        assert!(validate_sort_params(Some("field1"), Some("ASC")).is_err());
        assert!(validate_sort_params(Some("field1"), Some("ascending")).is_err());
    }

    #[test]
    fn name_rejects_empty_or_whitespace() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
    }

    #[test]
    fn name_rejects_too_long() {
        let s: String = "x".repeat(NAME_MAX_CHARS + 1);
        assert!(validate_name(&s).is_err());
    }

    #[test]
    fn name_accepts_boundary_values() {
        assert!(validate_name("T").is_ok());
        let s: String = "x".repeat(NAME_MAX_CHARS);
        assert!(validate_name(&s).is_ok());
    }

    #[test]
    fn category_rejects_empty_or_whitespace() {
        assert!(validate_category("").is_err());
        assert!(validate_category("   ").is_err());
    }

    #[test]
    fn category_rejects_too_long() {
        let s: String = "x".repeat(CATEGORY_MAX_CHARS + 1);
        assert!(validate_category(&s).is_err());
    }

    #[test]
    fn category_accepts_boundary_values() {
        assert!(validate_category("financial").is_ok());
        let s: String = "x".repeat(CATEGORY_MAX_CHARS);
        assert!(validate_category(&s).is_ok());
    }

    #[test]
    fn sort_dir_accepts_asc_desc() {
        assert!(validate_sort_dir("asc").is_ok());
        assert!(validate_sort_dir("desc").is_ok());
    }

    #[test]
    fn sort_dir_rejects_other() {
        assert!(validate_sort_dir("ASC").is_err());
        assert!(validate_sort_dir("ascending").is_err());
        assert!(validate_sort_dir("").is_err());
        assert!(validate_sort_dir(" asc").is_err());
        assert!(validate_sort_dir("asc ").is_err());
    }

    #[test]
    fn extract_fields_rejects_malformed_or_empty() {
        assert!(validate_extract_fields("not json").is_err());
        assert!(validate_extract_fields("{}").is_err());
        assert!(validate_extract_fields("[]").is_err());
        assert!(validate_extract_fields("[null]").is_err());
        assert!(validate_extract_fields(r#"["foo",""]"#).is_err());
        assert!(validate_extract_fields(r#"["foo","   "]"#).is_err());
    }

    #[test]
    fn extract_fields_accepts_valid() {
        assert!(validate_extract_fields(r#"["field1"]"#).is_ok());
        assert!(validate_extract_fields(r#"["field1","field2","field3"]"#).is_ok());
    }
}
