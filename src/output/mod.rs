#![allow(clippy::missing_errors_doc)]

//! Output formatting module for the Fast.io CLI.
//!
//! Supports JSON, table, and CSV output formats with automatic
//! TTY detection and field filtering.

/// CSV output renderer.
pub mod csv_output;
/// Field filtering for structured output.
pub mod format;
/// JSON output renderer.
pub mod json;
/// Table output renderer.
pub mod table;

use std::io::IsTerminal;

use serde_json::Value;

/// Keys to skip when searching for the primary data array in an API response object.
const METADATA_KEYS: &[&str] = &["pagination", "meta", "links"];

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
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json => write!(f, "json"),
            Self::Table => write!(f, "table"),
            Self::Csv => write!(f, "csv"),
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
            _ => Self::auto_detect(),
        }
    }

    /// Auto-detect: table for TTY stdout, JSON for piped output.
    fn auto_detect() -> Self {
        if std::io::stdout().is_terminal() {
            Self::Table
        } else {
            Self::Json
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
        }
    }
}

/// Flatten an API response envelope for table/CSV rendering.
///
/// Detects common API response patterns where the meaningful data is nested
/// inside a wrapper object (e.g., `{"orgs": [...], "pagination": {...}}`),
/// and extracts the data array so renderers produce proper rows and columns.
///
/// The heuristic: find the first key (skipping metadata keys like "pagination",
/// "meta", "links") whose value is either:
/// - An array of values (use that array directly)
/// - An object containing an "items" array (use the items array)
/// - A plain object (use it as a single-row value)
///
/// If the input is already an array or a scalar, it is returned as-is.
fn flatten_response(value: &Value) -> Value {
    let Value::Object(map) = value else {
        // Already an array or scalar — nothing to flatten.
        return value.clone();
    };

    // Find the first non-metadata key with meaningful data.
    for (key, val) in map {
        if METADATA_KEYS.contains(&key.as_str()) {
            continue;
        }

        match val {
            // Key maps to an array — use it as the row data.
            Value::Array(_) => return val.clone(),

            // Key maps to an object that contains an "items" array — use items.
            Value::Object(inner) => {
                if let Some(Value::Array(_)) = inner.get("items") {
                    return inner["items"].clone();
                }
                // Single nested object (e.g., {"user": {...}}) — return the
                // inner object so the renderer treats its keys as columns.
                if map.len() <= 2 && !inner.is_empty() {
                    return val.clone();
                }
            }

            _ => {}
        }
    }

    // No recognizable pattern — fall through to the original value so
    // the renderers handle it with their existing logic.
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
}
