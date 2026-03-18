#![allow(clippy::missing_errors_doc)]

/// CSV output formatter.
///
/// Renders JSON objects and arrays as CSV with a header row.
use std::io::{self, Write};

use serde_json::Value;

/// Render a JSON value as CSV to stdout.
pub fn render(value: &Value) -> Result<(), io::Error> {
    let items = match value {
        Value::Array(items) => items.clone(),
        Value::Object(_) => vec![value.clone()],
        other => {
            let mut stdout = io::stdout().lock();
            return writeln!(stdout, "{}", scalar_to_string(other));
        }
    };

    if items.is_empty() {
        return Ok(());
    }

    let headers = collect_headers(&items);
    if headers.is_empty() {
        return Ok(());
    }

    let mut stdout = io::stdout().lock();
    let mut wtr = csv::Writer::from_writer(vec![]);

    wtr.write_record(&headers)
        .map_err(|e| io::Error::other(e.to_string()))?;

    for item in &items {
        let row: Vec<String> = headers
            .iter()
            .map(|h| {
                item.get(h.as_str())
                    .map_or_else(String::new, scalar_to_string)
            })
            .collect();
        wtr.write_record(&row)
            .map_err(|e| io::Error::other(e.to_string()))?;
    }

    let data = wtr
        .into_inner()
        .map_err(|e| io::Error::other(e.to_string()))?;
    stdout.write_all(&data)
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

/// Convert a JSON value to a string for CSV output.
fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => serde_json::to_string(a).unwrap_or_default(),
        Value::Object(o) => serde_json::to_string(o).unwrap_or_default(),
    }
}
