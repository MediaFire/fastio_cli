#![allow(clippy::missing_errors_doc)]

/// JSON output formatter.
///
/// Pretty-prints JSON values to stdout.
use std::io::{self, Write};

use serde_json::Value;

/// Render a JSON value to stdout with pretty formatting.
pub fn render(value: &Value) -> Result<(), io::Error> {
    let output = serde_json::to_string_pretty(value)
        .map_err(|e| io::Error::other(format!("JSON serialization error: {e}")))?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{output}")
}
