#![allow(clippy::missing_errors_doc)]

//! Unified (grouped-bucket) search across a workspace or share.
//!
//! Maps to the `GET /workspace/{id}/search/` and `GET /share/{id}/search/`
//! endpoints documented at `~/vividengine/llms/storage.txt:1774-1985`. Unlike
//! the per-type endpoints (`storage/search`, `metadata/search`, comments),
//! this composes a single query into a set of independently
//! paginated, independently health-reported **buckets** (`files`, `metadata`,
//! `comments` — the share endpoint omits `metadata`).
//!
//! Pagination is **per bucket** via the documented `<bucket>_offset` /
//! `<bucket>_limit` pairs — there is no global `limit`/`offset`, and there is
//! no server-side `only` parameter (bucket filtering is client-side; see the
//! command layer). The response is rendered by the bucket-aware path in
//! `output::render_buckets`, never the lossy `flatten_response`.

use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Maximum length of the unified-search query string (`storage.txt:1799`).
pub const MAX_QUERY_LEN: usize = 1024;

/// Per-bucket pagination parameters for unified search.
///
/// Each field maps to the documented `<bucket>_offset` / `<bucket>_limit`
/// query parameters. All are optional; an omitted offset defaults to `0` and
/// an omitted limit to `25` server-side. `metadata_*` is ignored by the share
/// endpoint (metadata is workspace-only).
///
/// `#[non_exhaustive]` because the server may add buckets (and thus param
/// pairs) without an API-version bump.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct UnifiedSearchParams {
    /// Offset for the `files` bucket.
    pub files_offset: Option<u32>,
    /// Page size for the `files` bucket.
    pub files_limit: Option<u32>,
    /// Offset for the `metadata` bucket (workspace only).
    pub metadata_offset: Option<u32>,
    /// Page size for the `metadata` bucket (workspace only).
    pub metadata_limit: Option<u32>,
    /// Offset for the `comments` bucket.
    pub comments_offset: Option<u32>,
    /// Page size for the `comments` bucket.
    pub comments_limit: Option<u32>,
}

impl UnifiedSearchParams {
    /// An empty parameter set (all buckets use server defaults). Equivalent to
    /// [`Default::default`]; provided so callers in other crates can build the
    /// `#[non_exhaustive]` struct without struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the `files` bucket pagination (offset, limit).
    #[must_use]
    pub fn files(mut self, offset: Option<u32>, limit: Option<u32>) -> Self {
        self.files_offset = offset;
        self.files_limit = limit;
        self
    }

    /// Set the `metadata` bucket pagination (workspace only).
    #[must_use]
    pub fn metadata(mut self, offset: Option<u32>, limit: Option<u32>) -> Self {
        self.metadata_offset = offset;
        self.metadata_limit = limit;
        self
    }

    /// Set the `comments` bucket pagination.
    #[must_use]
    pub fn comments(mut self, offset: Option<u32>, limit: Option<u32>) -> Self {
        self.comments_offset = offset;
        self.comments_limit = limit;
        self
    }

    /// Build the query-parameter map, inserting `search` plus any supplied
    /// per-bucket offsets/limits. `include_metadata` is `false` for share
    /// searches so the workspace-only `metadata_*` params are never sent.
    fn into_query(self, query: &str, include_metadata: bool) -> HashMap<String, String> {
        let mut params = HashMap::new();
        params.insert("search".to_owned(), query.to_owned());
        let mut put = |k: &str, v: Option<u32>| {
            if let Some(n) = v {
                params.insert(k.to_owned(), n.to_string());
            }
        };
        put("files_offset", self.files_offset);
        put("files_limit", self.files_limit);
        if include_metadata {
            put("metadata_offset", self.metadata_offset);
            put("metadata_limit", self.metadata_limit);
        }
        put("comments_offset", self.comments_offset);
        put("comments_limit", self.comments_limit);
        params
    }
}

/// Validate a unified-search query string against the server contract:
/// non-blank after trimming and at most [`MAX_QUERY_LEN`] characters
/// (`storage.txt:1799`). Returns the original (untrimmed) query on success so
/// the caller forwards exactly what the user typed.
fn validate_query(query: &str) -> Result<(), CliError> {
    if query.trim().is_empty() {
        return Err(CliError::Parse("search query must not be empty".to_owned()));
    }
    if query.chars().count() > MAX_QUERY_LEN {
        return Err(CliError::Parse(format!(
            "search query must be at most {MAX_QUERY_LEN} characters"
        )));
    }
    Ok(())
}

/// Unified grouped-bucket search across an entire workspace.
///
/// `GET /workspace/{workspace_id}/search/`
///
/// Returns up to three buckets — `files`, `metadata`, `comments`
/// — each with its own `items`, pagination, `total`/`total_relation`,
/// `has_more`, and `status` (`ok` / `degraded`). See module docs.
pub async fn unified_search_workspace(
    client: &ApiClient,
    workspace_id: &str,
    query: &str,
    params: UnifiedSearchParams,
) -> Result<Value, CliError> {
    validate_query(query)?;
    let q = params.into_query(query, true);
    let path = format!("/workspace/{}/search/", urlencoding::encode(workspace_id));
    client.get_with_params(&path, &q).await
}

/// Unified grouped-bucket search across a share.
///
/// `GET /share/{share_id}/search/`
///
/// Returns the share-applicable subset of buckets (typically `files`, plus
/// `comments` when commenting is enabled); `metadata` is workspace-only and
/// is never requested. Workspace-backed (folder) shares return `404`
/// (code `1609`), which the command layer surfaces as a friendly "not
/// available for folder shares" message.
pub async fn unified_search_share(
    client: &ApiClient,
    share_id: &str,
    query: &str,
    params: UnifiedSearchParams,
) -> Result<Value, CliError> {
    validate_query(query)?;
    let q = params.into_query(query, false);
    let path = format!("/share/{}/search/", urlencoding::encode(share_id));
    client.get_with_params(&path, &q).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_query_includes_search_and_bucket_params() {
        let params = UnifiedSearchParams {
            files_limit: Some(10),
            comments_offset: Some(5),
            ..UnifiedSearchParams::default()
        };
        let q = params.into_query("hello", true);
        assert_eq!(q.get("search").map(String::as_str), Some("hello"));
        assert_eq!(q.get("files_limit").map(String::as_str), Some("10"));
        assert_eq!(q.get("comments_offset").map(String::as_str), Some("5"));
        // Unset params are absent (server applies defaults).
        assert!(!q.contains_key("files_offset"));
    }

    #[test]
    fn share_query_omits_metadata_params() {
        let params = UnifiedSearchParams {
            metadata_limit: Some(7),
            metadata_offset: Some(2),
            files_limit: Some(3),
            ..UnifiedSearchParams::default()
        };
        let q = params.into_query("docs", false);
        assert!(!q.contains_key("metadata_limit"));
        assert!(!q.contains_key("metadata_offset"));
        assert_eq!(q.get("files_limit").map(String::as_str), Some("3"));
    }

    #[test]
    fn workspace_query_includes_metadata_params() {
        let params = UnifiedSearchParams {
            metadata_limit: Some(7),
            ..UnifiedSearchParams::default()
        };
        let q = params.into_query("docs", true);
        assert_eq!(q.get("metadata_limit").map(String::as_str), Some("7"));
    }

    #[test]
    fn validate_query_rejects_blank() {
        assert!(validate_query("   ").is_err());
        assert!(validate_query("").is_err());
    }

    #[test]
    fn validate_query_rejects_too_long() {
        let long: String = "x".repeat(MAX_QUERY_LEN + 1);
        assert!(validate_query(&long).is_err());
        let ok: String = "x".repeat(MAX_QUERY_LEN);
        assert!(validate_query(&ok).is_ok());
    }
}
