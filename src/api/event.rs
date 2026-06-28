#![allow(clippy::missing_errors_doc)]

/// Event and activity API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for event search, details, and activity polling.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Parameters for [`search_events`].
///
/// Covers the documented audit/event-search filters (events.txt). One of
/// `workspace_id` / `share_id` / `user_id` / `org_id` / `parent_event_id` is
/// required by the server; `parent_event_id` cannot combine with filters other
/// than `acknowledged` / `limit` / `offset` (the server enforces this).
#[derive(Default)]
pub struct SearchEventsParams<'a> {
    /// Scope results to a specific workspace.
    pub workspace_id: Option<&'a str>,
    /// Scope results to a specific share.
    pub share_id: Option<&'a str>,
    /// Filter events by the acting user profile.
    pub user_id: Option<&'a str>,
    /// Filter events by organization.
    pub org_id: Option<&'a str>,
    /// Exact event type to match (e.g. `file.upload`).
    pub event: Option<&'a str>,
    /// Top-level event category filter.
    pub category: Option<&'a str>,
    /// Second-level event subcategory filter.
    pub subcategory: Option<&'a str>,
    /// Drill into a serial/batch parent event's children.
    pub parent_event_id: Option<&'a str>,
    /// Filter by the user who triggered the event (distinct from `user_id`).
    pub calling_user_id: Option<&'a str>,
    /// Filter by related object (file/folder) ID.
    pub object_id: Option<&'a str>,
    /// Audit-log read filter: `external_audit_log` or `external`.
    pub visibility: Option<&'a str>,
    /// Filter by acknowledgment status (forwarded as `true`/`false`).
    pub acknowledged: Option<bool>,
    /// Lower bound for event creation time (`created-min`).
    pub created_min: Option<&'a str>,
    /// Upper bound for event creation time (`created-max`).
    pub created_max: Option<&'a str>,
    /// Maximum number of events to return.
    pub limit: Option<u32>,
    /// Number of events to skip for pagination.
    pub offset: Option<u32>,
}

/// Build the query map for [`search_events`].
///
/// Extracted as a pure function so the parameter wiring — including the
/// hyphenated `created-min` / `created-max` keys and the `acknowledged`
/// boolean→string mapping — is testable without a network round-trip.
fn build_search_query(params: &SearchEventsParams<'_>) -> HashMap<String, String> {
    let mut query = HashMap::new();
    if let Some(v) = params.workspace_id {
        query.insert("workspace_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.share_id {
        query.insert("share_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.user_id {
        query.insert("user_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.org_id {
        query.insert("org_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.event {
        query.insert("event".to_owned(), v.to_owned());
    }
    if let Some(v) = params.category {
        query.insert("category".to_owned(), v.to_owned());
    }
    if let Some(v) = params.subcategory {
        query.insert("subcategory".to_owned(), v.to_owned());
    }
    if let Some(v) = params.parent_event_id {
        query.insert("parent_event_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.calling_user_id {
        query.insert("calling_user_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.object_id {
        query.insert("object_id".to_owned(), v.to_owned());
    }
    if let Some(v) = params.visibility {
        query.insert("visibility".to_owned(), v.to_owned());
    }
    if let Some(b) = params.acknowledged {
        query.insert("acknowledged".to_owned(), b.to_string());
    }
    if let Some(v) = params.created_min {
        query.insert("created-min".to_owned(), v.to_owned());
    }
    if let Some(v) = params.created_max {
        query.insert("created-max".to_owned(), v.to_owned());
    }
    if let Some(l) = params.limit {
        query.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = params.offset {
        query.insert("offset".to_owned(), o.to_string());
    }
    query
}

/// Search audit/event logs.
///
/// `GET /events/search/`
pub async fn search_events(
    client: &ApiClient,
    params: &SearchEventsParams<'_>,
) -> Result<Value, CliError> {
    let query = build_search_query(params);
    if query.is_empty() {
        client.get("/events/search/").await
    } else {
        client.get_with_params("/events/search/", &query).await
    }
}

/// Get event details.
///
/// `GET /event/{event_id}/details/`
pub async fn get_event_details(client: &ApiClient, event_id: &str) -> Result<Value, CliError> {
    let path = format!("/event/{}/details/", urlencoding::encode(event_id),);
    client.get(&path).await
}

/// Poll for activity on a workspace or share.
///
/// `GET /activity/poll/{entity_id}/`
///
/// When `updated` is `true`, the server returns only events newer than `lastactivity`
/// (used by activity-list incremental polling).
pub async fn poll_activity(
    client: &ApiClient,
    entity_id: &str,
    lastactivity: Option<&str>,
    wait: Option<u32>,
    updated: bool,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(v) = lastactivity {
        params.insert("lastactivity".to_owned(), v.to_owned());
    }
    if let Some(v) = wait {
        params.insert("wait".to_owned(), v.to_string());
    }
    if updated {
        params.insert("updated".to_owned(), "1".to_owned());
    }
    let path = format!("/activity/poll/{}/", urlencoding::encode(entity_id),);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Acknowledge an event.
///
/// `POST /event/{event_id}/ack/`
pub async fn acknowledge_event(client: &ApiClient, event_id: &str) -> Result<Value, CliError> {
    let path = format!("/event/{}/ack/", urlencoding::encode(event_id));
    let mut body = HashMap::new();
    body.insert("event_id".to_owned(), event_id.to_owned());
    client.post(&path, &body).await
}

/// Parameters for [`summarize_events`].
///
/// The summarize endpoint accepts every filter that `/events/search/` does
/// (events.txt) plus the summarize-only `user_context`. The shared filters
/// mirror [`SearchEventsParams`] so the query is built by the same
/// [`build_search_query`] builder rather than a divergent copy.
#[derive(Default)]
pub struct SummarizeEventsParams<'a> {
    /// Scope results to a specific workspace.
    pub workspace_id: Option<&'a str>,
    /// Scope results to a specific share.
    pub share_id: Option<&'a str>,
    /// Filter events by the acting user.
    pub user_id: Option<&'a str>,
    /// Filter events by organization.
    pub org_id: Option<&'a str>,
    /// Exact event type to match (e.g. `file.upload`).
    pub event: Option<&'a str>,
    /// Top-level event category filter.
    pub category: Option<&'a str>,
    /// Second-level event subcategory filter.
    pub subcategory: Option<&'a str>,
    /// Drill into a serial/batch parent event's children.
    pub parent_event_id: Option<&'a str>,
    /// Filter by the user who triggered the event (distinct from `user_id`).
    pub calling_user_id: Option<&'a str>,
    /// Filter by related object (file/folder) ID.
    pub object_id: Option<&'a str>,
    /// Audit-log read filter: `external_audit_log` or `external`.
    pub visibility: Option<&'a str>,
    /// Filter by acknowledgment status (forwarded as `true`/`false`).
    pub acknowledged: Option<bool>,
    /// Lower bound for event creation time (`created-min`).
    pub created_min: Option<&'a str>,
    /// Upper bound for event creation time (`created-max`).
    pub created_max: Option<&'a str>,
    /// Free-text context passed to the AI summarizer.
    pub user_context: Option<&'a str>,
    /// Maximum number of events to include in the summary.
    pub limit: Option<u32>,
    /// Number of events to skip before summarizing.
    pub offset: Option<u32>,
}

/// Build the query map for [`summarize_events`].
///
/// Reuses [`build_search_query`] for the shared search filters (so the
/// hyphenated `created-min` / `created-max` keys and the `acknowledged`
/// boolean→string mapping stay in one place) and then layers the
/// summarize-only `user_context` on top. Extracted so the parameter wiring is
/// testable without a network round-trip.
fn build_summarize_query(params: &SummarizeEventsParams<'_>) -> HashMap<String, String> {
    let search = SearchEventsParams {
        workspace_id: params.workspace_id,
        share_id: params.share_id,
        user_id: params.user_id,
        org_id: params.org_id,
        event: params.event,
        category: params.category,
        subcategory: params.subcategory,
        parent_event_id: params.parent_event_id,
        calling_user_id: params.calling_user_id,
        object_id: params.object_id,
        visibility: params.visibility,
        acknowledged: params.acknowledged,
        created_min: params.created_min,
        created_max: params.created_max,
        limit: params.limit,
        offset: params.offset,
    };
    let mut query = build_search_query(&search);
    if let Some(v) = params.user_context {
        query.insert("user_context".to_owned(), v.to_owned());
    }
    query
}

/// Get an AI-powered summary of events.
///
/// `GET /events/search/summarize/`
pub async fn summarize_events(
    client: &ApiClient,
    params: &SummarizeEventsParams<'_>,
) -> Result<Value, CliError> {
    let query = build_summarize_query(params);
    if query.is_empty() {
        client.get("/events/search/summarize/").await
    } else {
        client
            .get_with_params("/events/search/summarize/", &query)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SearchEventsParams, SummarizeEventsParams, build_search_query, build_summarize_query,
    };

    #[test]
    fn search_query_empty_when_no_filters() {
        let q = build_search_query(&SearchEventsParams::default());
        assert!(q.is_empty());
    }

    #[test]
    fn search_query_forwards_audit_filters() {
        let q = build_search_query(&SearchEventsParams {
            workspace_id: Some("1234567890123456789"),
            parent_event_id: Some("evt-parent"),
            calling_user_id: Some("9876543210987654321"),
            object_id: Some("node-abc"),
            visibility: Some("external_audit_log"),
            subcategory: Some("storage"),
            ..Default::default()
        });
        assert_eq!(
            q.get("workspace_id").map(String::as_str),
            Some("1234567890123456789")
        );
        assert_eq!(
            q.get("parent_event_id").map(String::as_str),
            Some("evt-parent")
        );
        assert_eq!(
            q.get("calling_user_id").map(String::as_str),
            Some("9876543210987654321")
        );
        assert_eq!(q.get("object_id").map(String::as_str), Some("node-abc"));
        assert_eq!(
            q.get("visibility").map(String::as_str),
            Some("external_audit_log")
        );
        assert_eq!(q.get("subcategory").map(String::as_str), Some("storage"));
        // user_id is distinct from calling_user_id and must not be set here.
        assert!(!q.contains_key("user_id"));
    }

    #[test]
    fn search_query_uses_hyphenated_time_bound_keys() {
        let q = build_search_query(&SearchEventsParams {
            workspace_id: Some("1"),
            created_min: Some("2025-12-01T06:00:00Z"),
            created_max: Some("2025-12-31 23:59:59"),
            ..Default::default()
        });
        assert_eq!(
            q.get("created-min").map(String::as_str),
            Some("2025-12-01T06:00:00Z")
        );
        assert_eq!(
            q.get("created-max").map(String::as_str),
            Some("2025-12-31 23:59:59")
        );
        // The non-hyphenated forms must NOT be present.
        assert!(!q.contains_key("created_min"));
        assert!(!q.contains_key("created_max"));
    }

    #[test]
    fn search_query_acknowledged_maps_bool_to_string() {
        let q_true = build_search_query(&SearchEventsParams {
            workspace_id: Some("1"),
            acknowledged: Some(true),
            ..Default::default()
        });
        assert_eq!(q_true.get("acknowledged").map(String::as_str), Some("true"));

        let q_false = build_search_query(&SearchEventsParams {
            workspace_id: Some("1"),
            acknowledged: Some(false),
            ..Default::default()
        });
        assert_eq!(
            q_false.get("acknowledged").map(String::as_str),
            Some("false")
        );

        let q_unset = build_search_query(&SearchEventsParams {
            workspace_id: Some("1"),
            ..Default::default()
        });
        assert!(!q_unset.contains_key("acknowledged"));
    }

    #[test]
    fn summarize_query_empty_when_no_filters() {
        let q = build_summarize_query(&SummarizeEventsParams::default());
        assert!(q.is_empty());
    }

    #[test]
    fn summarize_query_forwards_audit_filters_and_user_context() {
        let q = build_summarize_query(&SummarizeEventsParams {
            workspace_id: Some("1234567890123456789"),
            parent_event_id: Some("evt-parent"),
            calling_user_id: Some("9876543210987654321"),
            object_id: Some("node-abc"),
            visibility: Some("external_audit_log"),
            subcategory: Some("storage"),
            acknowledged: Some(true),
            created_min: Some("2025-12-01T06:00:00Z"),
            created_max: Some("2025-12-31 23:59:59"),
            user_context: Some("Focus on uploads"),
            ..Default::default()
        });
        // Audit filters forwarded identically to search.
        assert_eq!(
            q.get("workspace_id").map(String::as_str),
            Some("1234567890123456789")
        );
        assert_eq!(
            q.get("parent_event_id").map(String::as_str),
            Some("evt-parent")
        );
        assert_eq!(
            q.get("calling_user_id").map(String::as_str),
            Some("9876543210987654321")
        );
        assert_eq!(q.get("object_id").map(String::as_str), Some("node-abc"));
        assert_eq!(
            q.get("visibility").map(String::as_str),
            Some("external_audit_log")
        );
        assert_eq!(q.get("subcategory").map(String::as_str), Some("storage"));
        assert_eq!(q.get("acknowledged").map(String::as_str), Some("true"));
        // Hyphenated time-bound keys must be reused from build_search_query.
        assert_eq!(
            q.get("created-min").map(String::as_str),
            Some("2025-12-01T06:00:00Z")
        );
        assert_eq!(
            q.get("created-max").map(String::as_str),
            Some("2025-12-31 23:59:59")
        );
        assert!(!q.contains_key("created_min"));
        assert!(!q.contains_key("created_max"));
        // Summarize-only param layered on top.
        assert_eq!(
            q.get("user_context").map(String::as_str),
            Some("Focus on uploads")
        );
        // user_id is distinct from calling_user_id and must not be set here.
        assert!(!q.contains_key("user_id"));
    }
}
