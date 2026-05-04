#![allow(clippy::missing_errors_doc)]

//! Output formatting module for the Fast.io CLI.
//!
//! Supports JSON, table, CSV, and Markdown output formats with automatic
//! TTY detection and field filtering. Markdown is the default for
//! non-TTY stdout and is byte-equivalent to the server-side
//! `?output=markdown` contract (see `markdown.rs`); table is the default
//! for TTY.
//!
//! The markdown path renders the full response envelope — preamble,
//! error promotion, H1 sections — so it does NOT go through
//! `flatten_response`, unlike the table and CSV paths which consume
//! only the primary data payload.

/// CSV output renderer.
pub mod csv_output;
/// Field filtering for structured output.
pub mod format;
/// JSON output renderer.
pub mod json;
/// Markdown output renderer — byte-equivalent to the server-side
/// `?output=markdown` contract.
pub mod markdown;
/// Table output renderer.
pub mod table;

use std::io::IsTerminal;

use serde_json::Value;

/// Keys to skip when searching for the primary data array in an API
/// response object. Includes both classic pagination/metadata wrappers
/// and the envelope-level `result` field, which flows through the
/// client now that markdown rendering needs it for the `**Result:**`
/// preamble.
const METADATA_KEYS: &[&str] = &["pagination", "meta", "links", "result"];

/// Supported output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputFormat {
    /// Pretty-printed JSON.
    Json,
    /// Human-readable table (default for TTY).
    Table,
    /// Comma-separated values.
    Csv,
    /// GitHub-flavored Markdown, byte-equivalent to the server-side
    /// `?output=markdown` contract.
    Markdown,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json => write!(f, "json"),
            Self::Table => write!(f, "table"),
            Self::Csv => write!(f, "csv"),
            Self::Markdown => write!(f, "markdown"),
        }
    }
}

impl OutputFormat {
    /// Parse a format string (from `--format` flag).
    #[must_use]
    pub fn from_str_or_default(s: Option<&str>) -> Self {
        match s {
            Some("json") => Self::Json,
            Some("table") => Self::Table,
            Some("csv") => Self::Csv,
            Some("markdown" | "md") => Self::Markdown,
            _ => Self::auto_detect(),
        }
    }

    /// Auto-detect: table for TTY stdout, markdown for piped output.
    ///
    /// Markdown replaced JSON as the non-TTY default on 2026-04-15 because
    /// LLM consumers (MCP tools and pipelines feeding agents) get a much
    /// more compact, higher-signal representation from markdown than from
    /// pretty-printed JSON. Pass `--format json` to restore the old shape.
    fn auto_detect() -> Self {
        if std::io::stdout().is_terminal() {
            Self::Table
        } else {
            Self::Markdown
        }
    }
}

/// Configuration for output rendering.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    /// The output format to use.
    pub format: OutputFormat,
    /// Optional field filter (comma-separated field names).
    pub fields: Option<Vec<String>>,
    /// Disable colored output.
    pub no_color: bool,
    /// Suppress all output.
    pub quiet: bool,
}

impl OutputConfig {
    /// Build an `OutputConfig` from CLI flags.
    #[must_use]
    pub fn from_flags(
        format: Option<&str>,
        fields: Option<&str>,
        no_color: bool,
        quiet: bool,
    ) -> Self {
        Self {
            format: OutputFormat::from_str_or_default(format),
            fields: fields.map(|f| f.split(',').map(|s| s.trim().to_owned()).collect()),
            no_color,
            quiet,
        }
    }

    /// Render a JSON value to stdout using the configured format.
    pub fn render(&self, value: &Value) -> Result<(), std::io::Error> {
        if self.quiet {
            return Ok(());
        }

        let filtered = format::filter_fields(value, self.fields.as_deref());

        match self.format {
            OutputFormat::Json => json::render(&filtered),
            OutputFormat::Table => {
                let flattened = flatten_response(&filtered);
                table::render(&flattened, self.no_color)
            }
            OutputFormat::Csv => {
                let flattened = flatten_response(&filtered);
                csv_output::render(&flattened)
            }
            // Markdown renders the full envelope (preamble + H1
            // sections), so it MUST NOT go through `flatten_response`;
            // flattening strips the envelope and breaks rules 1 and 3
            // of the server contract.
            OutputFormat::Markdown => markdown::render(&filtered),
        }
    }
}

/// Flatten an API response envelope for table/CSV/markdown rendering.
///
/// Detects common API response patterns where the meaningful data is nested
/// inside a wrapper object (e.g., `{"orgs": [...], "pagination": {...}}`),
/// and extracts the data array so renderers produce proper rows and columns.
///
/// Two-pass heuristic (pass 2 only runs if pass 1 found nothing):
///
/// 1. Prefer array payloads. Walk non-metadata keys for an `Array` value or
///    a nested `Object` with an `items` array, and return the first match.
///    This avoids misclassifying a sidecar summary object (e.g. `"stats"`)
///    as the primary payload when an actual array (e.g. `"workspaces"`)
///    exists alongside it.
/// 2. Fall back to a single nested object (`{"user": {...}}`) — return the
///    inner object so the renderer treats its keys as columns.
///
/// Metadata keys (`pagination`, `meta`, `links`) are skipped in both passes.
/// If the input is already an array or a scalar, it is returned as-is.
fn flatten_response(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };

    // Pass 1: prefer arrays / items-arrays over plain nested objects, so
    // `{"stats": {...}, "workspaces": [...]}` doesn't return `stats`.
    for (key, val) in map {
        if METADATA_KEYS.contains(&key.as_str()) {
            continue;
        }
        match val {
            Value::Array(_) => return val.clone(),
            Value::Object(inner) => {
                if let Some(Value::Array(_)) = inner.get("items") {
                    return inner["items"].clone();
                }
            }
            _ => {}
        }
    }

    // Pass 2: fall back to a single nested object keyed under a simple
    // wrapper like `{"user": {...}}`.
    for (key, val) in map {
        if METADATA_KEYS.contains(&key.as_str()) {
            continue;
        }
        if let Value::Object(inner) = val
            && !inner.is_empty()
            && map.len() <= 2
        {
            return val.clone();
        }
    }

    value.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flatten_extracts_top_level_array() {
        let input = json!({
            "orgs": [{"id": "1", "name": "Acme"}, {"id": "2", "name": "Beta"}],
            "pagination": {"offset": 0, "limit": 25}
        });
        let result = flatten_response(&input);
        assert!(result.is_array());
        assert_eq!(result.as_array().unwrap().len(), 2);
        assert_eq!(result[0]["name"], "Acme");
    }

    #[test]
    fn flatten_extracts_nested_items_array() {
        let input = json!({
            "nodes": {"count": 3, "items": [{"id": "a"}, {"id": "b"}, {"id": "c"}]},
            "pagination": {"offset": 0}
        });
        let result = flatten_response(&input);
        assert!(result.is_array());
        assert_eq!(result.as_array().unwrap().len(), 3);
    }

    #[test]
    fn flatten_extracts_single_object() {
        let input = json!({"user": {"id": "42", "email": "a@b.com"}});
        let result = flatten_response(&input);
        assert!(result.is_object());
        assert_eq!(result["id"], "42");
    }

    #[test]
    fn flatten_passes_through_plain_array() {
        let input = json!([{"id": "1"}, {"id": "2"}]);
        let result = flatten_response(&input);
        assert!(result.is_array());
        assert_eq!(result.as_array().unwrap().len(), 2);
    }

    #[test]
    fn flatten_passes_through_scalar() {
        let input = json!("hello");
        let result = flatten_response(&input);
        assert_eq!(result, json!("hello"));
    }

    #[test]
    fn flatten_skips_metadata_keys() {
        let input = json!({
            "pagination": {"offset": 0},
            "workspaces": [{"id": "w1"}]
        });
        let result = flatten_response(&input);
        assert!(result.is_array());
        assert_eq!(result[0]["id"], "w1");
    }

    #[test]
    fn flatten_prefers_array_over_sibling_object() {
        // BTreeMap iteration order puts `stats` before `workspaces`
        // alphabetically; pass 1 must skip the sibling object and return
        // the array payload. Regression guard for the mis-route noted in
        // the 2026-04-15 review (M1).
        let input = json!({
            "stats": {"count": 3, "total": 9},
            "workspaces": [{"id": "w1"}, {"id": "w2"}]
        });
        let result = flatten_response(&input);
        assert!(result.is_array(), "got: {result}");
        assert_eq!(result.as_array().unwrap().len(), 2);
        assert_eq!(result[0]["id"], "w1");
    }
}
