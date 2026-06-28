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
            apply_only_filter(&mut value, only.as_deref());
            ctx.output.render(&value)?;
            Ok(())
        }
    }
}

/// Map a share unified-search error to a friendlier message for the common
/// "folder share" 404 case (code `1609`), where unified search is not
/// available. Other errors pass through unchanged.
fn map_share_search_error(err: fastio_cli::error::CliError) -> anyhow::Error {
    if let fastio_cli::error::CliError::Api(ref api_err) = err
        && (api_err.http_status == 404 || api_err.code == 1609)
    {
        return anyhow::anyhow!(
            "unified search is not available for workspace-backed (folder) shares"
        );
    }
    anyhow::Error::new(err).context("failed to search share")
}

/// Client-side filter: when `only` is supplied (comma-separated bucket names),
/// drop every bucket not named from the response `buckets` map before
/// rendering. This does NOT save server work — the server always searches
/// every applicable bucket — it only narrows what is displayed.
fn apply_only_filter(value: &mut Value, only: Option<&str>) {
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
                "workflows": {"items": [], "status": "ok"}
            }
        });
        apply_only_filter(&mut value, Some("files, comments"));
        let buckets = value["buckets"].as_object().unwrap();
        assert!(buckets.contains_key("files"));
        assert!(buckets.contains_key("comments"));
        assert!(!buckets.contains_key("workflows"));
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

    #[test]
    fn share_folder_404_maps_to_friendly_message() {
        let api_err = fastio_cli::error::ApiError::new(1609, None, "Not Found".to_owned(), 404);
        let mapped = map_share_search_error(fastio_cli::error::CliError::Api(api_err));
        assert!(mapped.to_string().contains("folder"), "got: {mapped}");
    }

    #[test]
    fn share_other_error_passes_through() {
        let api_err = fastio_cli::error::ApiError::new(1680, None, "Access Denied".to_owned(), 403);
        let mapped = map_share_search_error(fastio_cli::error::CliError::Api(api_err));
        assert!(!mapped.to_string().contains("folder"), "got: {mapped}");
    }
}
