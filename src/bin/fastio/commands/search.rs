//! Unified (grouped-bucket) search command handlers.
//!
//! `fastio search workspace <id> <query>` and `fastio search share <id> <query>`
//! issue one query and return results grouped into per-type buckets. The
//! bucket-aware renderer in `fastio_cli::output` renders each bucket as its
//! own labelled section (table/CSV/markdown) and surfaces degraded / lower-
//! bound notices; JSON passes the grouped shape through unchanged.

use anyhow::{Context, Result};
use serde_json::Value;

use fastio_cli::api;
use fastio_cli::api::search::UnifiedSearchParams;

use super::CommandContext;

/// Internal command enum for the `search` group.
#[derive(Debug)]
pub enum SearchCommand {
    /// Unified search across a workspace.
    Workspace {
        /// Workspace ID.
        workspace_id: String,
        /// Search query.
        query: String,
        /// Per-bucket pagination parameters.
        params: UnifiedSearchParams,
        /// Optional comma-separated list of buckets to display (client-side).
        only: Option<String>,
    },
    /// Unified search across a share.
    Share {
        /// Share ID.
        share_id: String,
        /// Search query.
        query: String,
        /// Per-bucket pagination parameters.
        params: UnifiedSearchParams,
        /// Optional comma-separated list of buckets to display (client-side).
        only: Option<String>,
    },
}

/// Execute a unified-search command.
pub async fn execute(command: SearchCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        SearchCommand::Workspace {
            workspace_id,
            query,
            params,
            only,
        } => {
            let client = ctx.build_client()?;
            let mut value =
                api::search::unified_search_workspace(&client, &workspace_id, &query, params)
                    .await
                    .context("failed to search workspace")?;
            ctx.warn_if_content_search_unavailable(&value);
            apply_only_filter(&mut value, only.as_deref());
            ctx.output.render(&value)?;
            Ok(())
        }
        SearchCommand::Share {
            share_id,
            query,
            params,
            only,
        } => {
            let client = ctx.build_client()?;
            let mut value =
                match api::search::unified_search_share(&client, &share_id, &query, params).await {
                    Ok(v) => v,
                    Err(e) => return Err(map_share_search_error(e)),
                };
            ctx.warn_if_content_search_unavailable(&value);
            apply_only_filter(&mut value, only.as_deref());
            ctx.output.render(&value)?;
            Ok(())
        }
    }
}

/// The message shown when a share **unified** search hits the "folder share"
/// case.
///
/// Shared with the MCP `search` tool so the CLI and MCP surfaces cannot drift
/// on the wording of the same server condition (CLI/MCP parity). The FLAT
/// (`files search --share`) leg has its own wording —
/// [`crate::commands::files::FILE_SEARCH_UNAVAILABLE_ON_FOLDER_SHARE`] — because
/// the condition is shared but the two surfaces must not describe each other.
pub const FOLDER_SHARE_SEARCH_UNAVAILABLE: &str =
    "unified search is not available for workspace-backed (folder) shares";

/// The API error code **both** share search endpoints return for a
/// workspace-backed (folder) share.
///
/// Verified 2026-08-29 two ways: from the server's own not-found handling,
/// which raises THIS unique code with the message "Search is not
/// available for Shared Folders"; and by measuring the wire, where
/// `/share/{id}/search/` and `/share/{id}/storage/search/` both answer 404 with
/// this code (they differ only in `error.resource`).
const FOLDER_SHARE_CODE: u32 = 139_420;

/// Whether a **share search** error is specifically the "folder share" case —
/// search is not available for workspace-backed shares.
///
/// Serves both share search routes deliberately: the unified
/// `/share/{id}/search/` (this module) and the flat
/// `/share/{id}/storage/search/` (`commands::files`). They emit the same code
/// for this condition, so the classifier is shared and only the user-facing
/// wording differs per surface — see
/// [`crate::commands::files::FILE_SEARCH_UNAVAILABLE_ON_FOLDER_SHARE`] and
/// [`FOLDER_SHARE_SEARCH_UNAVAILABLE`].
///
/// **Deliberately keyed on the unique error CODE, not on HTTP 404 alone.** This
/// endpoint returns at least two distinct 404s, and the orphaned-share one is
/// checked FIRST by the backend:
///
/// | `error.code` | condition | route |
/// |------|---------|---|
/// | `139420` | "Search is not available for Shared Folders" — **this case** | both share search routes |
/// | `142794` | "This shared folder no longer exists" (orphaned share) | `/share/{id}/search/` |
/// | `179101` | the same orphaned-share condition | `/share/{id}/storage/search/` |
/// | `9992`   | the API director's unrouted/not-found response | any deleted route |
///
/// A bare `http_status == 404` test (as this predicate used to be) relabels all
/// of those as "folder share", discarding a more accurate server message —
/// including the director miss that means the route itself is gone. An
/// unreadable error body (`ERR_BODY_UNAVAILABLE`, code `0`) is excluded for the
/// same reason: an unknown 404 is not a known condition.
///
/// **The orphaned codes are a DIFFERENT condition, not a missed case of this
/// one — do not fold them in.** Doing so would answer "search is not available
/// for folder shares" to a caller whose share no longer exists, which is the
/// same mislabeling this predicate was narrowed to remove. They are mutually
/// exclusive at the source: the backend's orphan guard runs *before* the
/// folder-share test, so at most one can fire per call. If orphaned-specific
/// handling is ever added it needs **both** `142794` and `179101` — that is the
/// leg where the two routes genuinely diverge, and a one-code version would be
/// half-right depending on which route the caller hit.
/// (Codes confirmed against the server 2026-08-29; `139420` also measured on
/// the wire.)
///
/// **`1609` is NOT a candidate here and must not be added back.** The docs'
/// error table renders this row as `1609 (Not Found)`, but the published API
/// docs ("Reading the error tables") state that four-digit `16xx`/`17xx` values
/// are **HTTP-status classes, not `error.code`**, and that comparing one against
/// `error.code` *will never match*; only five/six-digit values (plus the
/// `9661`-`9669` family) are real `error.code`s. A `1609` arm here would be
/// permanently dead code, and a predicate written as `404 && code == 1609`
/// would never fire at all.
#[must_use]
pub fn is_folder_share_search_error(err: &fastio_cli::error::CliError) -> bool {
    matches!(
        err,
        fastio_cli::error::CliError::Api(api_err)
            if api_err.http_status == 404 && api_err.code == FOLDER_SHARE_CODE
    )
}

/// Map a share unified-search error to a friendlier message for the "folder
/// share" case (HTTP 404 + `error.code` `139420`), where unified search is not
/// available. Other errors — including every OTHER 404 — pass through
/// unchanged, carrying the server's own message.
fn map_share_search_error(err: fastio_cli::error::CliError) -> anyhow::Error {
    if is_folder_share_search_error(&err) {
        return anyhow::anyhow!(FOLDER_SHARE_SEARCH_UNAVAILABLE);
    }
    anyhow::Error::new(err).context("failed to search share")
}

/// Client-side filter: when `only` is supplied (comma-separated bucket names),
/// drop every bucket not named from the response `buckets` map before
/// rendering. This does NOT save server work — the server always searches
/// every applicable bucket — it only narrows what is displayed.
///
/// Shared with the MCP `search` tool, where narrowing the buckets is also a
/// token saving for the caller.
pub fn apply_only_filter(value: &mut Value, only: Option<&str>) {
    let Some(only) = only else { return };
    let wanted: Vec<String> = only
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if wanted.is_empty() {
        return;
    }
    if let Some(buckets) = value.get_mut("buckets").and_then(Value::as_object_mut) {
        buckets.retain(|name, _| wanted.iter().any(|w| w == &name.to_ascii_lowercase()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_filter_keeps_named_buckets() {
        let mut value = json!({
            "result": true,
            "buckets": {
                "files": {"items": [], "status": "ok"},
                "comments": {"items": [], "status": "ok"},
                "metadata": {"items": [], "status": "ok"}
            }
        });
        apply_only_filter(&mut value, Some("files, comments"));
        let buckets = value["buckets"].as_object().unwrap();
        assert!(buckets.contains_key("files"));
        assert!(buckets.contains_key("comments"));
        assert!(!buckets.contains_key("metadata"));
    }

    #[test]
    fn only_filter_noop_when_absent() {
        let mut value = json!({"buckets": {"files": {}, "comments": {}}});
        apply_only_filter(&mut value, None);
        assert_eq!(value["buckets"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn only_filter_empty_string_is_noop() {
        let mut value = json!({"buckets": {"files": {}, "comments": {}}});
        apply_only_filter(&mut value, Some("  ,  "));
        assert_eq!(value["buckets"].as_object().unwrap().len(), 2);
    }

    /// Build an `Api` `CliError` for the classifier tests.
    fn api_err(code: u32, http_status: u16, message: &str) -> fastio_cli::error::CliError {
        fastio_cli::error::CliError::Api(fastio_cli::error::ApiError::new(
            code,
            None,
            message.to_owned(),
            http_status,
        ))
    }

    #[test]
    fn share_folder_unique_code_maps_to_friendly_message() {
        // The wire code the backend actually emits for this condition.
        let mapped = map_share_search_error(api_err(
            139_420,
            404,
            "Search is not available for Shared Folders",
        ));
        assert!(mapped.to_string().contains("folder"), "got: {mapped}");
    }

    #[test]
    fn status_class_1609_is_not_a_wire_code_and_must_not_match() {
        // Per the published API docs ("Reading the error tables"): four-digit
        // 16xx/17xx values are HTTP-status CLASSES, not `error.code`, and
        // comparing one against `error.code` never matches. Feeding code 1609
        // and requiring a folder-share message would pin a case the wire cannot
        // produce, so this is kept as a negative control — nobody should
        // "restore" the 1609 arm.
        assert!(!is_folder_share_search_error(&api_err(
            1609,
            404,
            "Not Found"
        )));
    }

    #[test]
    fn share_other_error_passes_through() {
        let mapped = map_share_search_error(api_err(1680, 403, "Access Denied"));
        assert!(!mapped.to_string().contains("folder"), "got: {mapped}");
    }

    #[test]
    fn share_non_folder_404s_are_not_relabelled_as_folder_shares() {
        // The regression this predicate exists to prevent. Each of these is a
        // 404 that is NOT the folder-share condition; relabelling any of them
        // discards a more accurate server message. The orphaned-share case is
        // the sharp one — the backend checks it BEFORE the folder-share test,
        // so it is reachable on exactly the same call.
        for (code, message) in [
            (142_794_u32, "This shared folder no longer exists"),
            (9992, "Resource not found."),
            (0, "error body unavailable"),
        ] {
            let err = api_err(code, 404, message);
            assert!(
                !is_folder_share_search_error(&err),
                "code {code} must NOT be classified as the folder-share case"
            );
            let mapped = map_share_search_error(err);
            // `{:#}` — anyhow's plain Display shows only the outermost context
            // ("failed to search share"); the server's own message is in the
            // SOURCE CHAIN. Asserting on `to_string()` here would pass for the
            // wrong reason (it never contains the message, folder-share or not)
            // and so could not detect the swallowing this test exists to catch.
            let full = format!("{mapped:#}");
            assert!(
                full.contains(message),
                "the server's own message must survive for code {code}, got: {full}"
            );
            assert!(
                !full.contains("workspace-backed"),
                "code {code} must not be relabelled as the folder-share case, got: {full}"
            );
        }
    }

    #[test]
    fn folder_share_classifier_requires_a_404() {
        // Code alone is not enough either — the pair is the discriminator.
        assert!(!is_folder_share_search_error(&api_err(
            139_420, 500, "boom"
        )));
        assert!(!is_folder_share_search_error(&api_err(1609, 200, "ok")));
    }
}
