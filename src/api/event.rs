#![allow(clippy::missing_errors_doc)]

/// Event and activity API endpoints for the Fast.io REST API.
///
/// Maps to endpoints for event search, details, and activity polling.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Parameters for [`search_events`].
#[derive(Default)]
pub struct SearchEventsParams<'a> {
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
    /// Maximum number of events to return.
    pub limit: Option<u32>,
    /// Number of events to skip for pagination.
    pub offset: Option<u32>,
}

/// Search audit/event logs.
///
/// `GET /events/search/`
pub async fn search_events(
    client: &ApiClient,
    params: &SearchEventsParams<'_>,
) -> Result<Value, CliError> {
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
    if let Some(l) = params.limit {
        query.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = params.offset {
        query.insert("offset".to_owned(), o.to_string());
    }
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
    /// Free-text context passed to the AI summarizer.
    pub user_context: Option<&'a str>,
    /// Maximum number of events to include in the summary.
    pub limit: Option<u32>,
    /// Number of events to skip before summarizing.
    pub offset: Option<u32>,
}

/// Get an AI-powered summary of events.
///
/// `GET /events/search/summarize/`
pub async fn summarize_events(
    client: &ApiClient,
    params: &SummarizeEventsParams<'_>,
) -> Result<Value, CliError> {
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
    if let Some(v) = params.user_context {
        query.insert("user_context".to_owned(), v.to_owned());
    }
    if let Some(l) = params.limit {
        query.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = params.offset {
        query.insert("offset".to_owned(), o.to_string());
    }
    if query.is_empty() {
        client.get("/events/search/summarize/").await
    } else {
        client
            .get_with_params("/events/search/summarize/", &query)
            .await
    }
}
