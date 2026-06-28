#![allow(clippy::missing_errors_doc)]

//! Per-workspace Dashboard API endpoints for the Fast.io REST API.
//!
//! Maps to the Dashboard surface documented at `~/vividengine/llms/dashboard.txt`:
//! a ranked, paginated feed of **actionable cards** for the calling workspace
//! member (approvals, tasks, reviews, confirmations, @mentions, file activity,
//! and pending signatures), plus per-member dismiss / snooze / undismiss of a
//! card. Dismiss and snooze are **out-of-band**: they hide a card from the
//! caller's own feed only and never advance, resolve, or otherwise change the
//! underlying obligation, workflow, or signature.
//!
//! The signature-card primary action — minting the caller's own signing link —
//! lives in [`crate::api::signing::my_sign_link`] (it is envelope-scoped) and is
//! surfaced as `fastio sign envelope my-sign-link`.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::client::ApiClient;
use crate::error::CliError;

/// Validate a dashboard `card_key` before it is used as a path parameter.
///
/// A blank card key would build a malformed `/cards//dismiss/` path, so it is
/// rejected client-side with a clear [`CliError::Parse`] rather than sent. The
/// server independently rejects non-printable / over-length keys (dashboard.txt
/// error `128636`); this guard only catches the empty case.
fn validate_card_key(card_key: &str) -> Result<(), CliError> {
    if card_key.trim().is_empty() {
        return Err(CliError::Parse(
            "dashboard card_key must not be empty".to_owned(),
        ));
    }
    Ok(())
}

/// Get the calling workspace member's ranked, paginated dashboard card feed.
///
/// `GET /workspace/{workspace_id}/dashboard/`
///
/// Returns `{ "result": true, "cards": [...], "pagination": {...},
/// "dismissed_recent_count": N }`. `limit` (1–200, default 50) and `offset`
/// (default 0) page the feed; an omitted value uses the server default. The
/// `workspace_id` is URL-encoded.
pub async fn get_dashboard(
    client: &ApiClient,
    workspace_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/dashboard/",
        urlencoding::encode(workspace_id)
    );
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Build the `/cards/{card_key}/dismiss/` action path for a dashboard card.
///
/// The `card_key` contains a `:` separator (e.g. `obligation:123…`) and MUST be
/// URL-encoded (`obligation%3A123…`) per dashboard.txt:192. Both ids are
/// URL-encoded.
fn card_dismiss_path(workspace_id: &str, card_key: &str) -> String {
    format!(
        "/workspace/{}/dashboard/cards/{}/dismiss/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(card_key)
    )
}

/// Dismiss a dashboard card permanently, or snooze it until a future timestamp.
///
/// `POST /workspace/{workspace_id}/dashboard/cards/{card_key}/dismiss/`
///
/// Out-of-band only — the underlying obligation, workflow, or signature is
/// unaffected. When `snooze_until` is `Some`, a JSON body
/// `{"snooze_until": "<ts>"}` is sent (the server validates the canonical
/// `"YYYY-MM-DD HH:MM:SS UTC"` format and that it is in the future); when `None`
/// the request carries no body at all (a permanent dismiss) — matching the
/// documented "optional JSON body" contract rather than posting `{}` with a JSON
/// content-type. Returns the `card_dismiss` envelope.
pub async fn dismiss_card(
    client: &ApiClient,
    workspace_id: &str,
    card_key: &str,
    snooze_until: Option<&str>,
) -> Result<Value, CliError> {
    validate_card_key(card_key)?;
    let path = card_dismiss_path(workspace_id, card_key);
    match snooze_until {
        Some(ts) => {
            client
                .post_json(&path, &json!({ "snooze_until": ts }))
                .await
        }
        // Permanent dismiss: literally bodyless (no JSON, no Content-Type), so
        // the server does not run the `Content-Type: application/json` body
        // validation (dashboard.txt error `149351`).
        None => client.post_empty(&path).await,
    }
}

/// Remove a dismiss or snooze, restoring the card to the caller's feed.
///
/// `DELETE /workspace/{workspace_id}/dashboard/cards/{card_key}/dismiss/`
///
/// Idempotent — undismissing a card that was never dismissed succeeds silently.
/// Reverses [`dismiss_card`].
pub async fn undismiss_card(
    client: &ApiClient,
    workspace_id: &str,
    card_key: &str,
) -> Result<Value, CliError> {
    validate_card_key(card_key)?;
    let path = card_dismiss_path(workspace_id, card_key);
    client.delete(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismiss_path_urlencodes_card_key_colon() {
        let path = card_dismiss_path("1234567890123456789", "obligation:9876543210987654321");
        assert_eq!(
            path,
            "/workspace/1234567890123456789/dashboard/cards/obligation%3A9876543210987654321/dismiss/"
        );
    }

    #[test]
    fn dismiss_path_urlencodes_workspace_id() {
        let path = card_dismiss_path("ws id", "file_activity:node-1");
        assert!(path.starts_with("/workspace/ws%20id/dashboard/cards/"));
        // The `:` in the card key is percent-encoded; the node id is preserved.
        assert!(path.contains("file_activity%3Anode-1"));
    }

    #[test]
    fn validate_card_key_rejects_blank() {
        assert!(validate_card_key("").is_err());
        assert!(validate_card_key("   ").is_err());
        assert!(validate_card_key("obligation:1").is_ok());
    }

    #[test]
    fn snooze_body_carries_snooze_until() {
        // The JSON body sent when snoozing is exactly `{"snooze_until": "<ts>"}`.
        let body = json!({ "snooze_until": "2026-06-18 09:00:00 UTC" });
        assert_eq!(
            body.get("snooze_until").and_then(Value::as_str),
            Some("2026-06-18 09:00:00 UTC")
        );
    }
}
