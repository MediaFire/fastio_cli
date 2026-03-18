#![allow(clippy::missing_errors_doc)]

/// Table output formatter using `comfy-table`.
///
/// Dynamically discovers columns from JSON keys and renders
/// a human-readable table to stdout.
use std::io::{self, Write};

use comfy_table::{ContentArrangement, Table};
use serde_json::Value;

/// Render a JSON value as a table to stdout.
pub fn render(value: &Value, no_color: bool) -> Result<(), io::Error> {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);

    if no_color {
        table.force_no_tty();
    }

    match value {
        Value::Array(items) => render_array(&mut table, items),
        Value::Object(_) => render_array(&mut table, std::slice::from_ref(value)),
        other => {
            // Scalar values: just print them directly.
            let mut stdout = io::stdout().lock();
            return writeln!(stdout, "{}", format_scalar(other));
        }
    }

    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{table}")
}

/// Render an array of objects as table rows.
fn render_array(table: &mut Table, items: &[Value]) {
    if items.is_empty() {
        return;
    }

    // Collect all unique keys in insertion order from the first object.
    let headers = collect_headers(items);
    if headers.is_empty() {
        return;
    }

    table.set_header(&headers);

    for item in items {
        let row: Vec<String> = headers
            .iter()
            .map(|h| item.get(h.as_str()).map_or_else(String::new, format_scalar))
            .collect();
        table.add_row(row);
    }
}

/// Collect column headers from an array of JSON objects.
fn collect_headers(items: &[Value]) -> Vec<String> {
    let mut headers = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for item in items {
        if let Value::Object(map) = item {
            for key in map.keys() {
                if seen.insert(key.clone()) {
                    headers.push(key.clone());
                }
            }
        }
    }
    headers
}

/// Format a scalar JSON value as a display string.
fn format_scalar(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => serde_json::to_string(a).unwrap_or_default(),
        Value::Object(o) => serde_json::to_string(o).unwrap_or_default(),
    }
}
