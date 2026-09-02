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

    // Display copies are sanitized; lookups keep the ORIGINAL key strings so a
    // key containing stripped characters still resolves its column values.
    let display_headers: Vec<String> = headers
        .iter()
        .map(|h| super::strip_terminal_hostile(h))
        .collect();
    table.set_header(&display_headers);

    for item in items {
        table.add_row(build_row(item, &headers));
    }
}

/// Build one table row: the cell for every header, in column order. A header
/// the item does not carry renders as an empty cell.
fn build_row(item: &Value, headers: &[String]) -> Vec<String> {
    headers
        .iter()
        .map(|h| {
            item.get(h.as_str())
                .map_or_else(String::new, |v| format_cell(h, v))
        })
        .collect()
}

/// Format one cell, given the column `key` it belongs to.
///
/// A room message's `parts` column carries an ordered array of message parts;
/// its file references render as `label (id)` so the cell is legible rather
/// than a JSON blob. Every other column — and any `parts` value this renderer
/// does not recognize — falls through to [`format_scalar`].
fn format_cell(key: &str, value: &Value) -> String {
    if key == super::PARTS_KEY
        && let Some(rendered) = super::format_reference_parts(value)
    {
        return rendered;
    }
    format_scalar(value)
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
///
/// Strings are stripped of terminal-hostile bytes (ANSI-escape injection
/// defense — see [`crate::output::strip_terminal_hostile`]); arrays/objects
/// are JSON-serialized and then sanitized as well: `to_string` escapes C0
/// controls but emits bidi / zero-width / BOM code points verbatim.
fn format_scalar(value: &Value) -> String {
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

    use super::{build_row, format_cell, format_scalar};

    /// An untrusted string (e.g. a peer-authored room message body) must not
    /// reach the terminal carrying ANSI escapes or bidi controls; ordinary
    /// whitespace survives.
    #[test]
    fn format_scalar_strips_terminal_hostile_bytes() {
        assert_eq!(
            format_scalar(&json!(
                "safe \u{1b}[31mred\u{1b}[0m \u{202e}bidi\r end\nnext\ttab"
            )),
            "safe [31mred[0m bidi end\nnext\ttab"
        );
        assert_eq!(format_scalar(&json!("plain")), "plain");
    }

    // ── Room-message `parts` rendering ──────────────────────────────────

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
    fn parts_cell_renders_reference_label_and_id() {
        let cell = format_cell("parts", &interleaved_parts());
        assert_eq!(cell, "report.pdf (2yxh5-ojakx-r3mwz)");
        // The JSON scaffolding this cell used to carry must be gone entirely.
        assert!(!cell.contains('{'), "{cell}");
        assert!(!cell.contains("\"type\""), "{cell}");
        assert!(!cell.contains("reference_type"), "{cell}");
    }

    #[test]
    fn parts_cell_preserves_reference_order() {
        let parts = json!([
            {"type": "reference", "reference_type": 5, "id": "id-a", "text": "a.pdf"},
            {"type": "text", "value": " and "},
            {"type": "reference", "reference_type": 5, "id": "id-b", "text": "b.txt"},
            {"type": "reference", "reference_type": 5, "id": "id-c", "text": "c.csv"}
        ]);
        assert_eq!(
            format_cell("parts", &parts),
            "a.pdf (id-a); b.txt (id-b); c.csv (id-c)"
        );
    }

    #[test]
    fn parts_cell_sanitizes_hostile_reference_strings() {
        // A reference label is peer-authored text printed straight to a
        // terminal: C0 controls, the ANSI ESC and bidi overrides are stripped,
        // while `|` and backticks — harmless in a table cell — survive.
        let parts = json!([{
            "type": "reference",
            "reference_type": 5,
            "id": "2yxh5\u{202e}-ojakx",
            "text": "re|p\u{0007}ort\u{1b}[31m`x`\u{202e}.pdf"
        }]);
        assert_eq!(
            format_cell("parts", &parts),
            "re|port[31m`x`.pdf (2yxh5-ojakx)"
        );
    }

    /// The fallback path sanitizes too — otherwise a peer could opt OUT of
    /// sanitization by deliberately malforming their own `parts` array.
    ///
    /// `serde_json::to_string` escapes C0 controls (so a raw ESC cannot survive)
    /// but emits bidi / zero-width / BOM code points verbatim. Those are exactly
    /// what `strip_terminal_hostile` exists to remove, and the author of a room
    /// message controls both the label content AND whether the array is
    /// well-formed — so the bypass was reachable on demand.
    /// `value` is never used as a label — pinned, because the wire shape lets a
    /// reference part carry BOTH `text` and `value`, and only `text` is the
    /// label. If a future change ever prefers `value`, this fails loudly:
    /// the fixture puts the redacted placeholder in `text` and a filename in
    /// `value`, which is exactly the shape a redacted reference has.
    #[test]
    fn parts_cell_never_uses_value_as_the_label() {
        let cell = format_cell(
            "parts",
            &json!([{
                "type": "reference",
                "reference_type": 5,
                "id": "n1",
                "text": "[unavailable]",
                "value": "quarterly-layoffs.pdf"
            }]),
        );
        assert_eq!(cell, "[unavailable] (n1)");
        assert!(
            !cell.contains("quarterly-layoffs"),
            "the label must come from `text`, never `value`: {cell}"
        );
    }

    #[test]
    fn parts_cell_fallback_is_sanitized_so_malforming_cannot_opt_out() {
        // Unknown `type` forces the JSON fallback; the label carries a bidi
        // override and a zero-width space.
        let malformed = json!([
            {"type": "mention", "label": "a\u{202e}b\u{200b}c"}
        ]);
        let cell = format_cell("parts", &malformed);

        assert!(
            !cell.contains('\u{202e}') && !cell.contains('\u{200b}'),
            "fallback must strip bidi/zero-width, got: {cell:?}"
        );
        // Still the same JSON shape otherwise — this is a degrade, not a rewrite.
        assert!(cell.contains("mention"), "{cell:?}");
        assert_eq!(
            cell,
            super::super::strip_terminal_hostile(
                &serde_json::to_string(&malformed).unwrap_or_default()
            ),
            "fallback must equal today's rendering, sanitized"
        );
    }

    #[test]
    fn parts_cell_degrades_to_todays_rendering_when_unrecognized() {
        // Each of these renders EXACTLY as it did before parts rendering
        // existed — no part silently dropped, no half-rendered cell.
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
            // Text parts only — no reference for the cell to show.
            json!([{"type": "text", "value": "just text"}]),
            // Text part with NO `value` — malformed, even though the value is
            // ignored when present. Rejecting a malformed reference while
            // accepting a malformed text part would be inconsistent.
            json!([{"type": "text"}, {"type": "reference", "id": "n1", "text": "x"}]),
            // Text part whose `value` is not a string.
            json!([{"type": "text", "value": 7}, {"type": "reference", "id": "n1", "text": "x"}]),
            // Empty array.
            json!([]),
            // Not an array at all.
            json!({"type": "reference"}),
            json!("plain"),
            json!(null),
        ];
        for case in &cases {
            assert_eq!(format_cell("parts", case), format_scalar(case), "{case}");
        }
        // Pin the byte shape the array cases fall back to.
        assert_eq!(
            format_cell("parts", &json!([{"type": "text", "value": "just text"}])),
            r#"[{"type":"text","value":"just text"}]"#
        );
        assert_eq!(format_cell("parts", &json!([])), "[]");
    }

    #[test]
    fn parts_cell_renders_every_reference_kind_including_unknown_ones() {
        // `reference_type` names the KIND of target, not the validity of the
        // reference. An unknown number, or none at all, must still render —
        // otherwise the cell would collapse to JSON for exactly the readers
        // served the newest kinds (e.g. a reference they may not see).
        let unknown_kind = json!([
            {"type": "reference", "reference_type": 99, "id": "id-a", "text": "a.pdf"}
        ]);
        assert_eq!(format_cell("parts", &unknown_kind), "a.pdf (id-a)");

        let no_kind = json!([{"type": "reference", "id": "id-b", "text": "b.txt"}]);
        assert_eq!(format_cell("parts", &no_kind), "b.txt (id-b)");

        // Mixed kinds render uniformly, in order.
        let mixed = json!([
            {"type": "reference", "reference_type": 5, "id": "id-a", "text": "a.pdf"},
            {"type": "text", "value": " and "},
            {"type": "reference", "reference_type": 99, "id": "id-b", "text": "b.txt"},
            {"type": "reference", "id": "id-c", "text": "c.csv"}
        ]);
        assert_eq!(
            format_cell("parts", &mixed),
            "a.pdf (id-a); b.txt (id-b); c.csv (id-c)"
        );
    }

    #[test]
    fn parts_cell_renders_a_placeholder_label_like_any_other() {
        // The label is whatever the server chose to disclose to THIS reader.
        // This renderer never interprets it — it has no notion of a redaction
        // placeholder and must not acquire one. (The literal below is test data
        // standing in for a server-side placeholder, not a real constant.)
        let parts = json!([
            {"type": "text", "value": "see "},
            {"type": "reference", "reference_type": 99, "id": "id-a", "text": "[unavailable]"}
        ]);
        assert_eq!(format_cell("parts", &parts), "[unavailable] (id-a)");
    }

    #[test]
    fn only_the_parts_column_is_reinterpreted() {
        // Key-scoped by design: an identically shaped array under any other
        // column keeps today's JSON rendering, so unrelated endpoints that
        // return tagged objects are untouched.
        let parts = interleaved_parts();
        let as_json = serde_json::to_string(&parts).unwrap();
        for key in ["references", "body", "Parts", "message_parts"] {
            assert_eq!(format_cell(key, &parts), as_json, "key: {key}");
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
}
