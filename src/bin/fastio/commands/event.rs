/// Event command implementations for `fastio event *`.
///
/// Handles event listing, details, activity polling, and the org change feed.
use anyhow::{Context, Result};
use serde_json::Value;

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::output::markdown::sanitize_inline;
use fastio_cli::output::{OutputConfig, OutputFormat, csv_output, format, table};

/// Event subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EventCommand {
    /// List/search events.
    List {
        /// Filter by workspace ID.
        workspace: Option<String>,
        /// Filter by share ID.
        share: Option<String>,
        /// Narrow by user profile ID. NOT the actor, and not a reliable
        /// identity — see the `--user-id` long help on `event list`.
        user_id: Option<String>,
        /// Filter by organization ID.
        org_id: Option<String>,
        /// Filter by event name.
        event: Option<String>,
        /// Filter by category.
        category: Option<String>,
        /// Filter by subcategory.
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children.
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event.
        calling_user_id: Option<String>,
        /// Filter by related object ID.
        object_id: Option<String>,
        /// Audit-log read filter: `external_audit_log` or `external`.
        visibility: Option<String>,
        /// Filter by acknowledgment status.
        acknowledged: Option<bool>,
        /// Lower bound for event creation time.
        created_min: Option<String>,
        /// Upper bound for event creation time.
        created_max: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Get event details.
    Info {
        /// Event ID.
        event_id: String,
    },
    /// Long-poll for activity updates.
    Poll {
        /// Workspace or share ID.
        entity_id: String,
        /// Last activity timestamp.
        lastactivity: Option<String>,
        /// Max wait time in seconds.
        wait: Option<u32>,
    },
    /// Acknowledge an event.
    Ack {
        /// Event ID to acknowledge.
        event_id: String,
    },
    /// AI-powered event summary.
    Summarize {
        /// Filter by workspace ID.
        workspace: Option<String>,
        /// Filter by share ID.
        share: Option<String>,
        /// Narrow by user profile ID. NOT the actor, and not a reliable
        /// identity — see the `--user-id` long help on `event list`.
        user_id: Option<String>,
        /// Filter by organization ID.
        org_id: Option<String>,
        /// Filter by event name.
        event: Option<String>,
        /// Filter by category.
        category: Option<String>,
        /// Filter by subcategory.
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children.
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event.
        calling_user_id: Option<String>,
        /// Filter by related object ID.
        object_id: Option<String>,
        /// Audit-log read filter: `external_audit_log` or `external`.
        visibility: Option<String>,
        /// Filter by acknowledgment status.
        acknowledged: Option<bool>,
        /// Lower bound for event creation time.
        created_min: Option<String>,
        /// Upper bound for event creation time.
        created_max: Option<String>,
        /// Free-text context for the AI summarizer.
        user_context: Option<String>,
        /// Max events to include.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Read one page of the org-wide storage change feed.
    Changes {
        /// Organization ID (19-digit).
        org_id: String,
        /// Cursor from the previous response; `None` bootstraps.
        cursor: Option<String>,
        /// Maximum number of changes to return (1-1000).
        limit: Option<u32>,
    },
}

/// Execute an event subcommand.
pub async fn execute(command: &EventCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        EventCommand::List {
            workspace,
            share,
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            limit,
            offset,
        } => {
            let params = api::event::SearchEventsParams {
                workspace_id: workspace.as_deref(),
                share_id: share.as_deref(),
                user_id: user_id.as_deref(),
                org_id: org_id.as_deref(),
                event: event.as_deref(),
                category: category.as_deref(),
                subcategory: subcategory.as_deref(),
                parent_event_id: parent_event_id.as_deref(),
                calling_user_id: calling_user_id.as_deref(),
                object_id: object_id.as_deref(),
                visibility: visibility.as_deref(),
                acknowledged: *acknowledged,
                created_min: created_min.as_deref(),
                created_max: created_max.as_deref(),
                limit: *limit,
                offset: *offset,
            };
            list(ctx, &params).await
        }
        EventCommand::Info { event_id } => info(ctx, event_id).await,
        EventCommand::Poll {
            entity_id,
            lastactivity,
            wait,
        } => poll(ctx, entity_id, lastactivity.as_deref(), *wait).await,
        EventCommand::Ack { event_id } => ack(ctx, event_id).await,
        EventCommand::Summarize {
            workspace,
            share,
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            user_context,
            limit,
            offset,
        } => {
            let params = api::event::SummarizeEventsParams {
                workspace_id: workspace.as_deref(),
                share_id: share.as_deref(),
                user_id: user_id.as_deref(),
                org_id: org_id.as_deref(),
                event: event.as_deref(),
                category: category.as_deref(),
                subcategory: subcategory.as_deref(),
                parent_event_id: parent_event_id.as_deref(),
                calling_user_id: calling_user_id.as_deref(),
                object_id: object_id.as_deref(),
                visibility: visibility.as_deref(),
                acknowledged: *acknowledged,
                created_min: created_min.as_deref(),
                created_max: created_max.as_deref(),
                user_context: user_context.as_deref(),
                limit: *limit,
                offset: *offset,
            };
            summarize(ctx, &params).await
        }
        EventCommand::Changes {
            org_id,
            cursor,
            limit,
        } => changes(ctx, org_id, cursor.as_deref(), *limit).await,
    }
}

/// List/search events.
async fn list(ctx: &CommandContext<'_>, params: &api::event::SearchEventsParams<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::search_events(&client, params)
        .await
        .context("failed to search events")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get event details.
async fn info(ctx: &CommandContext<'_>, event_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::get_event_details(&client, event_id)
        .await
        .context("failed to get event details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Long-poll for activity updates.
async fn poll(
    ctx: &CommandContext<'_>,
    entity_id: &str,
    lastactivity: Option<&str>,
    wait: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::poll_activity(&client, entity_id, lastactivity, wait, false)
        .await
        .context("failed to poll activity")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Acknowledge an event.
async fn ack(ctx: &CommandContext<'_>, event_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::acknowledge_event(&client, event_id)
        .await
        .context("failed to acknowledge event")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get an AI-powered summary of events.
async fn summarize(
    ctx: &CommandContext<'_>,
    params: &api::event::SummarizeEventsParams<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::event::summarize_events(&client, params)
        .await
        .context("failed to summarize events")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Keys that stay on the response under `--fields`: without them a caller
/// cannot take the next step of the feed.
const CHANGES_KEPT_KEYS: &[&str] = &["cursor", "has_more"];

/// Read one page of the org-wide storage change feed.
async fn changes(
    ctx: &CommandContext<'_>,
    org_id: &str,
    cursor: Option<&str>,
    limit: Option<u32>,
) -> Result<()> {
    let org_id = org_id.trim();
    anyhow::ensure!(
        super::is_profile_id(org_id),
        "invalid org ID '{org_id}': expected a 19-digit numeric ID"
    );
    let client = ctx.build_client()?;
    let value = api::event::org_changes(&client, org_id, cursor, limit)
        .await
        .map_err(api::event::map_org_changes_error)
        .context("failed to read the org change feed")?;
    render_changes(ctx.output, &value)
}

/// Render a change-feed page.
///
/// JSON and markdown render the whole response. Table and CSV render the
/// `changes` rows only — never the generic flatten, which picks
/// `profiles.items` on a bootstrap page because `changes` is empty — and put
/// the cursor state on stderr so stdout stays one clean table/CSV document.
fn render_changes(output: &OutputConfig, value: &Value) -> Result<()> {
    if output.quiet {
        return Ok(());
    }
    let fields = output.fields.as_deref().filter(|f| !f.is_empty());
    if matches!(output.format, OutputFormat::Table | OutputFormat::Csv) {
        let rows = changes_rows(value, fields);
        if output.format == OutputFormat::Table {
            table::render(&rows, output.no_color)?;
        } else {
            csv_output::render(&rows)?;
        }
        for line in changes_sidecar_lines(value) {
            eprintln!("{line}");
        }
        return Ok(());
    }
    let shaped = changes_with_fields(value, fields);
    let unfiltered = OutputConfig {
        fields: None,
        ..output.clone()
    };
    unfiltered.render(&shaped)?;
    Ok(())
}

/// The table/CSV rows: the `changes` array (projected by `--fields`), or an
/// empty array when the page has none.
fn changes_rows(value: &Value, fields: Option<&[String]>) -> Value {
    let rows = value
        .get("changes")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    match fields {
        Some(f) => format::filter_fields(&rows, Some(f)),
        None => rows,
    }
}

/// Apply `--fields` to the whole response, then restore `cursor` and
/// `has_more` so the next call can still be made.
fn changes_with_fields(value: &Value, fields: Option<&[String]>) -> Value {
    let Some(f) = fields else {
        return value.clone();
    };
    let mut filtered = format::filter_fields(value, Some(f));
    if let (Value::Object(out), Value::Object(orig)) = (&mut filtered, value) {
        for key in CHANGES_KEPT_KEYS {
            if let Some(v) = orig.get(*key) {
                out.insert((*key).to_owned(), v.clone());
            }
        }
    }
    filtered
}

/// The cursor state printed to stderr beside table/CSV output.
fn changes_sidecar_lines(value: &Value) -> Vec<String> {
    let scalar = |v: Option<&Value>| match v {
        Some(Value::String(s)) => sanitize_inline(s),
        Some(Value::Null) | None => "(none)".to_owned(),
        Some(other) => sanitize_inline(&other.to_string()),
    };
    vec![
        format!("cursor: {}", scalar(value.get("cursor"))),
        format!("has_more: {}", scalar(value.get("has_more"))),
        format!(
            "profiles.version: {}",
            scalar(value.get("profiles").and_then(|p| p.get("version")))
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::{changes_rows, changes_sidecar_lines, changes_with_fields};
    use serde_json::{Value, json};

    fn fields(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Shape of a real bootstrap page: no changes, a populated readable set.
    fn bootstrap() -> Value {
        json!({
            "result": true,
            "changes": [],
            "cursor": "ab+c/d==",
            "has_more": false,
            "profiles": {
                "version": "9b1f0c",
                "items": [
                    {"id": "4829105738291047362", "type": "workspace"},
                    {"id": "5510392857104938271", "type": "share"}
                ]
            }
        })
    }

    fn page() -> Value {
        json!({
            "result": true,
            "changes": [{
                "event_id": "e1",
                "event": "workspace_storage_file_added",
                "profile_id": "4829105738291047362",
                "profile_type": "workspace",
                "object_id": "n1",
                "created": "2026-09-25 21:40:11 UTC"
            }],
            "cursor": "next",
            "has_more": true,
            "profiles": {"version": "9b1f0c", "items": []}
        })
    }

    /// Table/CSV on bootstrap must render ZERO change rows — not the profiles
    /// list, which is what the generic flatten would pick.
    #[test]
    fn bootstrap_table_rows_are_empty_not_profiles() {
        assert_eq!(changes_rows(&bootstrap(), None), json!([]));
        assert_eq!(
            changes_rows(&bootstrap(), Some(&fields(&["id", "type"]))),
            json!([])
        );
    }

    #[test]
    fn table_rows_are_the_changes_projected_by_fields() {
        let rows = changes_rows(&page(), Some(&fields(&["event_id", "event"])));
        assert_eq!(
            rows,
            json!([{"event_id": "e1", "event": "workspace_storage_file_added"}])
        );
    }

    #[test]
    fn table_rows_default_to_empty_when_changes_missing() {
        assert_eq!(changes_rows(&json!({"result": true}), None), json!([]));
    }

    /// `--fields` must never strip the cursor state, on a bootstrap page (where
    /// nothing matches) or a populated one.
    #[test]
    fn fields_keep_cursor_and_has_more() {
        let out = changes_with_fields(&bootstrap(), Some(&fields(&["event_id"])));
        assert_eq!(out.get("cursor"), Some(&json!("ab+c/d==")));
        assert_eq!(out.get("has_more"), Some(&json!(false)));

        let out = changes_with_fields(&page(), Some(&fields(&["event_id"])));
        assert_eq!(out.get("cursor"), Some(&json!("next")));
        assert_eq!(out.get("has_more"), Some(&json!(true)));
        assert_eq!(out["changes"], json!([{"event_id": "e1"}]));
    }

    #[test]
    fn no_fields_returns_the_response_unchanged() {
        assert_eq!(changes_with_fields(&page(), None), page());
    }

    #[test]
    fn sidecar_lines_carry_cursor_has_more_and_version() {
        assert_eq!(
            changes_sidecar_lines(&bootstrap()),
            vec![
                "cursor: ab+c/d==".to_owned(),
                "has_more: false".to_owned(),
                "profiles.version: 9b1f0c".to_owned(),
            ]
        );
    }

    #[test]
    fn sidecar_lines_strip_terminal_control_characters() {
        let v = json!({"cursor": "a\u{1b}]0;x\u{7}b", "has_more": true});
        let lines = changes_sidecar_lines(&v);
        assert!(!lines[0].contains('\u{1b}'), "got: {:?}", lines[0]);
        assert_eq!(lines[2], "profiles.version: (none)");
    }
}
