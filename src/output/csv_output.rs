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

    // Display copies are sanitized; lookups keep the ORIGINAL key strings so a
    // key containing stripped characters still resolves its column values.
    let display_headers: Vec<String> = headers
        .iter()
        .map(|h| super::strip_terminal_hostile(h))
        .collect();
    wtr.write_record(&display_headers)
        .map_err(|e| io::Error::other(e.to_string()))?;

    for item in &items {
        wtr.write_record(build_row(item, &headers))
            .map_err(|e| io::Error::other(e.to_string()))?;
    }

    let data = wtr
        .into_inner()
        .map_err(|e| io::Error::other(e.to_string()))?;
    stdout.write_all(&data)
}

/// Collect column headers from an array of JSON objects.
///
/// `pub(crate)` so the bucket-aware CSV path can assert header order/shape in
/// tests; otherwise an internal helper of the CSV renderer.
pub(crate) fn collect_headers(items: &[Value]) -> Vec<String> {
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

/// Build one CSV record: the field for every header, in column order. A header
/// the item does not carry writes an empty field.
fn build_row(item: &Value, headers: &[String]) -> Vec<String> {
    headers
        .iter()
        .map(|h| {
            item.get(h.as_str())
                .map_or_else(String::new, |v| format_field(h, v))
        })
        .collect()
}

/// Format one CSV field, given the column `key` it belongs to.
///
/// A room message's `parts` column renders its file references as `label (id)`
/// via [`super::format_reference_parts`], shared with the table renderer so the
/// two surfaces cannot drift. Every other column, and any `parts` value that
/// renderer does not recognize, falls through to [`scalar_to_string`].
fn format_field(key: &str, value: &Value) -> String {
    if key == super::PARTS_KEY
        && let Some(rendered) = super::format_reference_parts(value)
    {
        return rendered;
    }
    scalar_to_string(value)
}

/// Convert a JSON value to a string for CSV output.
///
/// Strings are stripped of terminal-hostile bytes (`--format csv` on a TTY
/// prints raw — see [`crate::output::strip_terminal_hostile`]); JSON output
/// remains the byte-faithful format. Serialized arrays/objects are
/// sanitized after encoding: `to_string` escapes C0 controls but passes
/// bidi / zero-width / BOM through unchanged.
fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => super::strip_terminal_hostile(s),
        // Serialized JSON is sanitized too: `to_string` escapes C0 controls
        // but emits bidi / zero-width / BOM code points verbatim, and this is
        // the path a peer-authored `parts` array lands on when it is malformed
        // — which its author controls. Without this, sanitization would be
        // opt-out by sending a deliberately broken array.
        Value::Array(a) => {
            super::strip_terminal_hostile(&serde_json::to_string(a).unwrap_or_default())
        }
        Value::Object(o) => {
            super::strip_terminal_hostile(&serde_json::to_string(o).unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{build_row, format_field, scalar_to_string};

    /// CSV fields must not carry ANSI escapes / bidi controls to a terminal;
    /// ordinary whitespace survives (the csv crate quotes it correctly).
    #[test]
    fn scalar_to_string_strips_terminal_hostile_bytes() {
        assert_eq!(
            scalar_to_string(&json!("a\u{1b}[2Jb\u{200b}c\nd")),
            "a[2Jbc\nd"
        );
    }

    // ── Room-message `parts` rendering ──────────────────────────────────

    /// Mirror of the table surface: the CSV fallback sanitizes too, so a peer
    /// cannot opt out of sanitization by malforming their own `parts` array.
    #[test]
    fn parts_field_fallback_is_sanitized_so_malforming_cannot_opt_out() {
        let malformed = json!([{"type": "mention", "label": "a\u{202e}b\u{200b}c"}]);
        let field = format_field("parts", &malformed);

        assert!(
            !field.contains('\u{202e}') && !field.contains('\u{200b}'),
            "fallback must strip bidi/zero-width, got: {field:?}"
        );
        assert_eq!(
            field,
            super::super::strip_terminal_hostile(
                &serde_json::to_string(&malformed).unwrap_or_default()
            ),
            "fallback must equal today's rendering, sanitized"
        );
    }

    /// A file reference between two text parts — the shape the server sends
    /// for `@[file:…]` markup inside a message body.
    fn interleaved_parts() -> Value {
        json!([
            {"type": "text", "value": "see "},
            {
                "type": "reference",
                "reference_type": 5,
                "id": "2yxh5-ojakx-r3mwz",
                "text": "report.pdf"
            },
            {"type": "text", "value": " for the numbers"}
        ])
    }

    #[test]
    fn parts_field_renders_reference_label_and_id() {
        let field = format_field("parts", &interleaved_parts());
        assert_eq!(field, "report.pdf (2yxh5-ojakx-r3mwz)");
        // The JSON scaffolding this field used to carry must be gone entirely.
        assert!(!field.contains('{'), "{field}");
        assert!(!field.contains("\"type\""), "{field}");
        assert!(!field.contains("reference_type"), "{field}");
    }

    #[test]
    fn parts_field_preserves_reference_order() {
        let parts = json!([
            {"type": "reference", "reference_type": 5, "id": "id-a", "text": "a.pdf"},
            {"type": "text", "value": " and "},
            {"type": "reference", "reference_type": 5, "id": "id-b", "text": "b.txt"},
            {"type": "reference", "reference_type": 5, "id": "id-c", "text": "c.csv"}
        ]);
        assert_eq!(
            format_field("parts", &parts),
            "a.pdf (id-a); b.txt (id-b); c.csv (id-c)"
        );
    }

    #[test]
    fn parts_field_sanitizes_hostile_reference_strings() {
        // A reference label is peer-authored text a `--format csv` run prints
        // straight to a terminal: C0 controls, the ANSI ESC and bidi overrides
        // are stripped; `|` and backticks survive.
        let parts = json!([{
            "type": "reference",
            "reference_type": 5,
            "id": "2yxh5\u{202e}-ojakx",
            "text": "re|p\u{0007}ort\u{1b}[31m`x`\u{202e}.pdf"
        }]);
        assert_eq!(
            format_field("parts", &parts),
            "re|port[31m`x`.pdf (2yxh5-ojakx)"
        );
    }

    #[test]
    fn parts_field_degrades_to_todays_rendering_when_unrecognized() {
        // Each of these renders EXACTLY as it did before parts rendering
        // existed — no part silently dropped, no half-rendered field.
        let cases = [
            // An element that is not an object.
            json!([{"type": "text", "value": "a"}, "oops"]),
            // Missing `type`.
            json!([{"value": "a"}]),
            // Unknown `type`.
            json!([{"type": "mention", "id": "u1"}]),
            // Reference missing `id`.
            json!([{"type": "reference", "reference_type": 5, "text": "report.pdf"}]),
            // Reference missing `text`.
            json!([{"type": "reference", "reference_type": 5, "id": "i"}]),
            // Text part with no `value`, and with a non-string `value`.
            json!([{"type": "text"}, {"type": "reference", "id": "n1", "text": "x"}]),
            json!([{"type": "text", "value": 7}, {"type": "reference", "id": "n1", "text": "x"}]),
            // Text parts only — no reference for the field to show.
            json!([{"type": "text", "value": "just text"}]),
            // Empty array.
            json!([]),
            // Not an array at all.
            json!({"type": "reference"}),
            json!("plain"),
            json!(null),
        ];
        for case in &cases {
            assert_eq!(
                format_field("parts", case),
                scalar_to_string(case),
                "{case}"
            );
        }
        // Pin the byte shape the array cases fall back to.
        assert_eq!(
            format_field("parts", &json!([{"type": "text", "value": "just text"}])),
            r#"[{"type":"text","value":"just text"}]"#
        );
        assert_eq!(format_field("parts", &json!([])), "[]");
    }

    #[test]
    fn parts_field_renders_every_reference_kind_including_unknown_ones() {
        // `reference_type` names the KIND of target, not the validity of the
        // reference. An unknown number, or none at all, must still render —
        // otherwise the field would collapse to JSON for exactly the readers
        // served the newest kinds (e.g. a reference they may not see).
        let unknown_kind = json!([
            {"type": "reference", "reference_type": 99, "id": "id-a", "text": "a.pdf"}
        ]);
        assert_eq!(format_field("parts", &unknown_kind), "a.pdf (id-a)");

        let no_kind = json!([{"type": "reference", "id": "id-b", "text": "b.txt"}]);
        assert_eq!(format_field("parts", &no_kind), "b.txt (id-b)");

        // Mixed kinds render uniformly, in order.
        let mixed = json!([
            {"type": "reference", "reference_type": 5, "id": "id-a", "text": "a.pdf"},
            {"type": "text", "value": " and "},
            {"type": "reference", "reference_type": 99, "id": "id-b", "text": "b.txt"},
            {"type": "reference", "id": "id-c", "text": "c.csv"}
        ]);
        assert_eq!(
            format_field("parts", &mixed),
            "a.pdf (id-a); b.txt (id-b); c.csv (id-c)"
        );
    }

    #[test]
    fn parts_field_renders_a_placeholder_label_like_any_other() {
        // The label is whatever the server chose to disclose to THIS reader.
        // This renderer never interprets it — it has no notion of a redaction
        // placeholder and must not acquire one. (The literal below is test data
        // standing in for a server-side placeholder, not a real constant.)
        let parts = json!([
            {"type": "text", "value": "see "},
            {"type": "reference", "reference_type": 99, "id": "id-a", "text": "[unavailable]"}
        ]);
        assert_eq!(format_field("parts", &parts), "[unavailable] (id-a)");
    }

    #[test]
    fn only_the_parts_column_is_reinterpreted() {
        // Key-scoped by design: an identically shaped array under any other
        // column keeps today's JSON rendering.
        let parts = interleaved_parts();
        let as_json = serde_json::to_string(&parts).unwrap();
        for key in ["references", "body", "Parts", "message_parts"] {
            assert_eq!(format_field(key, &parts), as_json, "key: {key}");
        }
    }

    #[test]
    fn message_row_renders_parts_beside_an_untouched_body() {
        let headers = vec![
            "message_id".to_owned(),
            "body".to_owned(),
            "parts".to_owned(),
        ];
        let item = json!({
            "message_id": "m1",
            "body": "see @[file:2yxh5-ojakx-r3mwz:report.pdf] for the numbers",
            "parts": interleaved_parts()
        });
        assert_eq!(
            build_row(&item, &headers),
            vec![
                "m1".to_owned(),
                "see @[file:2yxh5-ojakx-r3mwz:report.pdf] for the numbers".to_owned(),
                "report.pdf (2yxh5-ojakx-r3mwz)".to_owned(),
            ]
        );
    }

    #[test]
    fn parts_free_row_renders_unchanged() {
        // Regression guard: a row without a `parts` column, including an
        // ordinary array cell and an absent column, renders exactly as before.
        let headers = vec![
            "message_id".to_owned(),
            "body".to_owned(),
            "tags".to_owned(),
            "absent".to_owned(),
        ];
        let item = json!({
            "message_id": "m2",
            "body": "no references here",
            "tags": ["a", "b"]
        });
        assert_eq!(
            build_row(&item, &headers),
            vec![
                "m2".to_owned(),
                "no references here".to_owned(),
                r#"["a","b"]"#.to_owned(),
                String::new(),
            ]
        );
    }

    #[test]
    fn parts_field_survives_csv_quoting() {
        // A label carrying a comma and a double quote must round-trip through
        // the real CSV writer as ONE quoted field, not two columns.
        let headers = vec!["parts".to_owned()];
        let item = json!({"parts": [
            {"type": "reference", "reference_type": 5, "id": "id-a", "text": "a,b\"c.pdf"},
            {"type": "reference", "reference_type": 5, "id": "id-b", "text": "b.txt"}
        ]});
        let mut wtr = csv::Writer::from_writer(vec![]);
        wtr.write_record(build_row(&item, &headers)).unwrap();
        let out = String::from_utf8(wtr.into_inner().unwrap()).unwrap();
        assert_eq!(out, "\"a,b\"\"c.pdf (id-a); b.txt (id-b)\"\n");
    }
}
