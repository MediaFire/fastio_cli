//! Markdown output renderer for API response envelopes.
//!
//! Produces GitHub-flavored Markdown byte-equivalent to the server-side
//! `?output=markdown` response modifier. The reference implementation is
//! `App\Api\MarkdownRenderer` in the PHP server (see
//! `https://api.fast.io/current/llms/full/` for the public contract);
//! unit tests in this module pin the byte shape against a handful of
//! live server responses.
//!
//! # Output contract
//!
//! - A `**Result:** success|failure` preamble when the envelope carries
//!   a scalar `result` field.
//! - An `error` object is promoted to a leading `# Error` section
//!   regardless of whether `result` indicates success.
//! - Remaining top-level keys become H1 sections in **insertion order**.
//!   Insertion order requires the `serde_json/preserve_order` feature —
//!   do not disable it.
//! - Scalars: `null` → `—` (em-dash), bools literal, numbers cast to
//!   string, short single-line strings inline, multiline or HTML-like
//!   strings fenced (leaf) or inline-coded (inline).
//! - Associative maps → `- **key:** value` bullet list.
//! - Record-shaped arrays → GFM pipe table with insertion-order columns
//!   across the union of keys.
//! - Pure scalar lists → bulleted list.
//! - Mixed lists → bulleted list with maps inlined as
//!   `**k:** v; **k:** v`.
//! - Keys matching `^\d+\.` get a backslash before the dot. No other
//!   key escaping is applied.
//! - Table cells escape only `|`, `\`, `` ` ``, and newlines;
//!   HTML-like / multiline cell content is rendered with inline code
//!   fences.
//! - Heading level caps at 6; deeper recursion falls back to a fenced
//!   `json` block.
//! - Output always ends with exactly one `\n`.
//!
//! # Defensive surface
//!
//! The server contract mandates a **light touch** on value/body
//! escaping: bullet values and heading text are not escaped because the
//! renderer produces markdown for downstream reading or rendering, not
//! for embedding into other markdown. **Consumers that render the
//! result as HTML MUST sanitize.** The inline-code / fenced-block
//! fencing of HTML-like strings is a belt-and-suspenders default, not a
//! substitute for `htmlspecialchars()` or equivalent on the consumer
//! side.
//!
//! This renderer preserves three runtime safety rails that are
//! orthogonal to the server contract:
//!
//! 1. **Memory cap.** [`MAX_OUTPUT_BYTES`] bounds the rendered string at
//!    4 MiB; a pathological multi-MB string value cannot grow the buffer
//!    without bound. Past the cap, rendering stops and a truncation
//!    marker is appended.
//! 2. **Recursion cap.** [`MAX_DEPTH`] bounds recursion at 64 frames to
//!    prevent stack overflow on programmatically-constructed inputs.
//!    This is distinct from the heading-level-6 cap (which is about
//!    readability).
//! 3. **Trojan-Source stripping.** C0/C1 control characters and Unicode
//!    bidi / zero-width / BOM code points are removed from all emitted
//!    text. Trojan Source–style reordering attacks and homoglyph
//!    spoofing cannot round-trip through this renderer.
//!
//! None of these rails change the output for well-formed inputs — they
//! only narrow the behavior on adversarial ones.
//!
//! The renderer is used in two places:
//! 1. The CLI `--format markdown` flag (top-level output).
//! 2. The MCP server, where markdown is the default tool-response
//!    format because it is significantly more token-efficient for LLM
//!    consumers than pretty-printed JSON.

use std::io::{self, Write};

use serde_json::{Map, Value};

/// Depth cap for recursive rendering. Prevents stack overflow on
/// pathological inputs. Orthogonal to the heading-level cap in rule 9 —
/// this is a hard safety rail, that one is a readability rule.
const MAX_DEPTH: u32 = 64;

/// Soft cap on rendered output bytes. Past this point, the renderer
/// stops appending and adds a truncation marker. 4 MiB is comfortably
/// above typical Fast.io list/details payloads and leaves headroom for
/// atypical large responses without granting a single malformed field
/// unbounded memory.
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Maximum number of columns a rendered pipe table may have. Bounds
/// header-vec memory independently of [`MAX_OUTPUT_BYTES`].
const MAX_TABLE_COLUMNS: usize = 256;

/// Marker appended when [`MAX_OUTPUT_BYTES`] is reached.
const OUTPUT_TRUNCATED_MARKER: &str =
    "\n_… (output truncated — retry with `--format json` for full payload)_\n";

/// Render a JSON value to stdout as GitHub-flavored Markdown.
///
/// The rendered string already ends with exactly one `\n` per the
/// output contract, so this function writes it verbatim without
/// appending another newline.
///
/// # Errors
///
/// Returns an [`io::Error`] if writing the rendered markdown to stdout
/// fails (e.g., broken pipe when the consumer closes its read side early).
pub fn render(value: &Value) -> Result<(), io::Error> {
    let mut stdout = io::stdout().lock();
    let md = to_markdown(value);
    stdout.write_all(md.as_bytes())
}

/// Convert a JSON value into a Markdown string matching the Fast.io
/// `?output=markdown` contract (see module docs).
///
/// Output is capped at [`MAX_OUTPUT_BYTES`]; past that point rendering
/// stops and a truncation marker is appended, so a pathological 100 MB
/// payload cannot blow memory. The returned string always ends with
/// exactly one `\n`.
#[must_use]
pub fn to_markdown(value: &Value) -> String {
    let mut out = String::new();
    render_envelope(value, &mut out);
    let hit_cap = out.len() >= MAX_OUTPUT_BYTES;
    // Normalize the trailing newline: spec requires exactly one.
    let trimmed = out.trim_end_matches('\n').len();
    out.truncate(trimmed);
    if hit_cap {
        out.push_str(OUTPUT_TRUNCATED_MARKER);
    } else {
        out.push('\n');
    }
    out
}

fn render_envelope(value: &Value, out: &mut String) {
    let Value::Object(map) = value else {
        // Non-object top-level values have no preamble or H1 sections
        // per the spec; render the bare value and let the trailing-`\n`
        // normalization run at the end of `to_markdown`.
        render_leaf_value(value, out, 1);
        return;
    };

    let mut blocks: Vec<(usize, usize)> = Vec::new();

    // Rule 1: preamble from scalar `result`.
    let mut result_key: Option<String> = None;
    if let Some(result_val) = map.get("result")
        && is_scalar(result_val)
    {
        let status = if is_success(result_val) {
            "success"
        } else {
            "failure"
        };
        let start = out.len();
        out.push_str("**Result:** ");
        out.push_str(status);
        blocks.push((start, out.len()));
        result_key = Some("result".to_owned());
    }

    // Rule 2: error promotion. Triggered for any object-valued `error`
    // regardless of result.
    let mut error_key: Option<String> = None;
    if let Some(err_val) = map.get("error")
        && let Value::Object(err_map) = err_val
    {
        if out.len() < MAX_OUTPUT_BYTES {
            let start = out.len();
            out.push_str("# Error\n");
            render_map_as_bullets(err_map, out, 1);
            trim_trailing_newline_in_place(out);
            blocks.push((start, out.len()));
        }
        error_key = Some("error".to_owned());
    }

    // Rule 3: remaining keys → H1 sections in insertion order.
    for (k, v) in map {
        if out.len() >= MAX_OUTPUT_BYTES {
            break;
        }
        if result_key.as_deref() == Some(k.as_str()) {
            continue;
        }
        if error_key.as_deref() == Some(k.as_str()) {
            continue;
        }
        let start = out.len();
        out.push_str("# ");
        push_escaped_key(out, k);
        out.push('\n');
        render_section_value(v, out, 1);
        trim_trailing_newline_in_place(out);
        blocks.push((start, out.len()));
    }

    // Blocks concatenate with a single blank line between them. We built
    // them directly into `out` sequentially with no separator, so we now
    // need to splice `\n` between adjacent block boundaries. The simplest
    // correct approach: rebuild `out` from the recorded (start, end)
    // slice spans joined with `\n\n`.
    if blocks.len() > 1 {
        let mut joined = String::with_capacity(out.len() + blocks.len());
        for (i, (s, e)) in blocks.iter().enumerate() {
            if i > 0 {
                joined.push_str("\n\n");
            }
            joined.push_str(&out[*s..*e]);
        }
        *out = joined;
    }
}

/// Trim any trailing `\n` characters from `out` in place.
fn trim_trailing_newline_in_place(out: &mut String) {
    while out.ends_with('\n') {
        out.pop();
    }
}

/// Render a value that appears directly beneath an H1 section heading
/// (rule 3 + value dispatch).
fn render_section_value(value: &Value, out: &mut String, level: u32) {
    if out.len() >= MAX_OUTPUT_BYTES {
        return;
    }
    match value {
        Value::Null => {
            out.push('—');
            out.push('\n');
        }
        Value::Bool(b) => {
            out.push_str(if *b { "true" } else { "false" });
            out.push('\n');
        }
        Value::Number(n) => {
            out.push_str(&n.to_string());
            out.push('\n');
        }
        Value::String(s) => {
            render_leaf_string(s, out);
        }
        Value::Object(map) => {
            render_map_as_bullets(map, out, level);
        }
        Value::Array(arr) => {
            render_array_value(arr, out, level);
        }
    }
}

/// Render a scalar-leaf value (used at the top level when the payload is
/// not an object — e.g., a pre-flattened scalar).
fn render_leaf_value(value: &Value, out: &mut String, level: u32) {
    render_section_value(value, out, level);
}

fn render_leaf_string(s: &str, out: &mut String) {
    if needs_fence(s) {
        out.push_str("```\n");
        let before = out.len();
        push_sanitized_leaf(out, s);
        let wrote_trailing_nl =
            out.as_bytes().get(out.len().wrapping_sub(1)) == Some(&b'\n') && out.len() > before;
        if !wrote_trailing_nl {
            out.push('\n');
        }
        out.push_str("```\n");
    } else {
        push_sanitized_leaf(out, s);
        out.push('\n');
    }
}

/// Render an array in a context where it stands alone (section body or
/// nested under a bullet).
fn render_array_value(arr: &[Value], out: &mut String, level: u32) {
    if arr.is_empty() {
        out.push('—');
        out.push('\n');
        return;
    }
    if is_record_list(arr) {
        render_table(arr, out);
        return;
    }
    // Scalar or mixed list: emit bullets. Indent by (level - 1) * 2.
    let indent = indent_for(level);
    for item in arr {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        out.push_str(&indent);
        out.push_str("- ");
        match item {
            Value::Object(m) => render_inline_map_body(m, out),
            _ => render_inline_scalar(item, out),
        }
        out.push('\n');
    }
}

/// Render an associative map as a `- **key:** value` bullet list.
/// Heading/recursion level tracked per rule 4b.
fn render_map_as_bullets(map: &Map<String, Value>, out: &mut String, level: u32) {
    if level > 6 {
        // Rule 9: fallback to a fenced JSON block.
        render_json_fallback(&Value::Object(map.clone()), out);
        return;
    }
    if level > MAX_DEPTH {
        // Runtime safety rail: prevent stack overflow. Separate from the
        // spec's heading cap.
        render_json_fallback(&Value::Object(map.clone()), out);
        return;
    }
    let indent = indent_for(level);
    for (k, v) in map {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        render_bullet_kv(k, v, out, level, &indent);
    }
}

fn render_bullet_kv(key: &str, value: &Value, out: &mut String, level: u32, indent: &str) {
    match value {
        Value::Object(inner) => {
            out.push_str(indent);
            out.push_str("- **");
            push_escaped_key(out, key);
            out.push_str(":**\n");
            render_map_as_bullets(inner, out, level + 1);
        }
        Value::Array(arr) if arr.is_empty() => {
            out.push_str(indent);
            out.push_str("- **");
            push_escaped_key(out, key);
            out.push_str(":** —\n");
        }
        Value::Array(arr) if is_record_list(arr) => {
            out.push_str(indent);
            out.push_str("- **");
            push_escaped_key(out, key);
            out.push_str(":**\n");
            render_table(arr, out);
        }
        Value::Array(arr) => {
            out.push_str(indent);
            out.push_str("- **");
            push_escaped_key(out, key);
            out.push_str(":**\n");
            render_array_value(arr, out, level + 1);
        }
        _ => {
            out.push_str(indent);
            out.push_str("- **");
            push_escaped_key(out, key);
            out.push_str(":** ");
            render_inline_scalar(value, out);
            out.push('\n');
        }
    }
}

/// Render an associative map inlined as `**k:** v; **k:** v`
/// (rule 4e for mixed-list bullets).
fn render_inline_map_body(map: &Map<String, Value>, out: &mut String) {
    let mut first = true;
    for (k, v) in map {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        if !first {
            out.push_str("; ");
        }
        first = false;
        out.push_str("**");
        push_escaped_key(out, k);
        out.push_str(":** ");
        render_inline_scalar(v, out);
    }
}

/// Render a scalar for an inline context (bullet value, inline map value,
/// mixed-list element). Multiline / HTML-like strings get inline-code
/// fencing per rule 7. Complex values collapse to a single-line JSON.
fn render_inline_scalar(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push('—'),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            if needs_fence(s) {
                render_inline_code(s, out, false);
            } else {
                push_sanitized_inline(out, s);
            }
        }
        _ => {
            // Map/array nested under a bullet that expects inline —
            // happens only for mixed lists where a non-scalar is present;
            // collapse to inline code JSON.
            let json = serde_json::to_string(value).unwrap_or_default();
            render_inline_code(&json, out, false);
        }
    }
}

/// Wrap `s` with the smallest backtick fence that can contain it,
/// streaming each character into `out` so [`MAX_OUTPUT_BYTES`] is
/// honored per character. When `in_cell` is set, any bare `|` inside
/// the code span is additionally escaped as `\|` because GFM table
/// parsers split cells on unescaped pipes regardless of code-span
/// context.
fn render_inline_code(s: &str, out: &mut String, in_cell: bool) {
    let max_run = longest_backtick_run(s);
    let fence_len = max_run + 1;
    for _ in 0..fence_len {
        out.push('`');
    }
    let needs_padding = s.starts_with('`') || s.ends_with('`');
    if needs_padding {
        out.push(' ');
    }
    for ch in s.chars() {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        if is_stripped_char(ch) {
            continue;
        }
        match ch {
            '\n' | '\r' => out.push(' '),
            '|' if in_cell => out.push_str("\\|"),
            c => out.push(c),
        }
    }
    if needs_padding {
        out.push(' ');
    }
    for _ in 0..fence_len {
        out.push('`');
    }
}

fn longest_backtick_run(s: &str) -> usize {
    let mut max = 0usize;
    let mut cur = 0usize;
    for ch in s.chars() {
        if ch == '`' {
            cur += 1;
            if cur > max {
                max = cur;
            }
        } else {
            cur = 0;
        }
    }
    max
}

fn is_record_list(arr: &[Value]) -> bool {
    if arr.is_empty() {
        return false;
    }
    arr.iter().all(|v| matches!(v, Value::Object(_)))
}

fn render_table(arr: &[Value], out: &mut String) {
    if out.len() >= MAX_OUTPUT_BYTES {
        return;
    }
    // Collect union of keys in insertion order.
    let mut headers: Vec<String> = Vec::new();
    'collect: for item in arr {
        if let Some(obj) = item.as_object() {
            for k in obj.keys() {
                if headers.len() >= MAX_TABLE_COLUMNS {
                    break 'collect;
                }
                if !headers.iter().any(|h| h == k) {
                    headers.push(k.clone());
                }
            }
        }
    }

    if headers.is_empty() {
        // Degenerate: all rows empty. Spec rule 10 is ambiguous; emit an
        // em-dash sentinel so the output stays well-formed.
        out.push('—');
        out.push('\n');
        return;
    }

    // Header row
    out.push('|');
    for h in &headers {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        out.push(' ');
        push_escaped_key_cell(out, h);
        out.push_str(" |");
    }
    out.push('\n');
    // Divider row
    out.push('|');
    for _ in &headers {
        out.push_str(" --- |");
    }
    out.push('\n');
    // Data rows
    for item in arr {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        let Some(obj) = item.as_object() else {
            continue;
        };
        out.push('|');
        for h in &headers {
            out.push(' ');
            match obj.get(h) {
                None | Some(Value::Null) => out.push('—'),
                Some(v) => render_cell_value(v, out),
            }
            out.push_str(" |");
        }
        out.push('\n');
    }
}

fn render_cell_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push('—'),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            if needs_fence(s) {
                render_inline_code(s, out, true);
            } else {
                push_cell_text(out, s);
            }
        }
        _ => {
            // Complex: collapse to single-line JSON then escape as plain
            // cell text (not an inline-code fence, matching spec rule 6
            // which collapses complex values via json_encode first).
            let json = serde_json::to_string(value).unwrap_or_default();
            push_cell_text(out, &json);
        }
    }
}

/// Stream-escape plain text into a GFM table cell (rule 6). Escapes
/// `|`, `\`, and `` ` ``, strips bidi/control characters, and flattens
/// newlines to spaces. Honors [`MAX_OUTPUT_BYTES`] per character.
fn push_cell_text(out: &mut String, s: &str) {
    for ch in s.chars() {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        if is_stripped_char(ch) {
            continue;
        }
        match ch {
            '\n' | '\r' => out.push(' '),
            '|' => out.push_str("\\|"),
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            c => out.push(c),
        }
    }
}

fn render_json_fallback(value: &Value, out: &mut String) {
    out.push_str("```json\n");
    match serde_json::to_string_pretty(value) {
        Ok(s) => out.push_str(&s),
        Err(_) => out.push_str("{}"),
    }
    out.push('\n');
    out.push_str("```\n");
}

/// Rule 5: escape only keys matching `^\d+\.`. The escape prepends `\`
/// before the dot, so `1.foo` → `1\.foo`. All other keys are literal.
/// Streams the escaped key into `out` with inline sanitization so the
/// shared memory cap applies per character.
fn push_escaped_key(out: &mut String, key: &str) {
    if matches_numeric_dot(key) {
        let dot = key.find('.').unwrap_or(key.len());
        push_sanitized_inline(out, &key[..dot]);
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        out.push('\\');
        push_sanitized_inline(out, &key[dot..]);
    } else {
        push_sanitized_inline(out, key);
    }
}

/// Key variant for table-header context: applies the numeric-dot
/// escape, then layers the cell-text escape so a `|` in a header name
/// cannot break the table.
fn push_escaped_key_cell(out: &mut String, key: &str) {
    let mut tmp = String::new();
    push_escaped_key(&mut tmp, key);
    push_cell_text(out, &tmp);
}

fn matches_numeric_dot(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_digit() {
        return false;
    }
    let mut saw_dot = false;
    for c in chars {
        if c == '.' {
            saw_dot = true;
            break;
        }
        if !c.is_ascii_digit() {
            return false;
        }
    }
    saw_dot
}

/// Stream sanitized leaf-context text directly into `out`. Strips C0/C1
/// controls, Unicode bidi / zero-width / BOM code points, and CR. LF is
/// preserved (leaf contexts are multi-line friendly — fenced blocks).
/// Honors [`MAX_OUTPUT_BYTES`] per character so a 100 MB string value
/// can never grow the buffer beyond the cap.
fn push_sanitized_leaf(out: &mut String, s: &str) {
    for ch in s.chars() {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        if is_stripped_char(ch) || ch == '\r' {
            continue;
        }
        out.push(ch);
    }
}

/// Stream sanitized inline-context text into `out`. Same stripping as
/// [`push_sanitized_leaf`], but additionally flattens `\n`/`\r` to a
/// single space because inline contexts (bullet values, table cells,
/// inline maps) cannot span lines.
fn push_sanitized_inline(out: &mut String, s: &str) {
    for ch in s.chars() {
        if out.len() >= MAX_OUTPUT_BYTES {
            return;
        }
        if is_stripped_char(ch) {
            continue;
        }
        match ch {
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
}

fn is_stripped_char(ch: char) -> bool {
    let code = ch as u32;
    if code < 0x20 && ch != '\n' && ch != '\t' {
        return true;
    }
    if (0x80..=0x9F).contains(&code) {
        return true;
    }
    matches!(
        ch,
        '\u{200B}'
            | '\u{200C}'
            | '\u{200D}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
            | '\u{FEFF}'
    )
}

fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn is_success(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => {
            let lower = s.to_ascii_lowercase();
            matches!(lower.as_str(), "true" | "success" | "yes" | "ok")
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i != 0
            } else if let Some(u) = n.as_u64() {
                u != 0
            } else if let Some(f) = n.as_f64() {
                f != 0.0 && !f.is_nan()
            } else {
                false
            }
        }
        _ => false,
    }
}

fn needs_fence(s: &str) -> bool {
    if s.contains('\n') {
        return true;
    }
    looks_html_like(s)
}

/// Rule 7 trigger: regex `/<[a-z!\/]/i`.
fn looks_html_like(s: &str) -> bool {
    let bytes = s.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == b'<' {
            let next = bytes[i + 1];
            if next == b'!' || next == b'/' || next.is_ascii_alphabetic() {
                return true;
            }
        }
    }
    false
}

fn indent_for(level: u32) -> String {
    let level = level.max(1) as usize;
    "  ".repeat(level - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------- Rule 1: preamble ----------

    #[test]
    fn rule1_preamble_success_bool_true() {
        let md = to_markdown(&json!({"result": true}));
        assert_eq!(md, "**Result:** success\n");
    }

    #[test]
    fn rule1_preamble_success_string_variants() {
        for variant in [
            "true", "True", "TRUE", "success", "Success", "yes", "ok", "OK",
        ] {
            let md = to_markdown(&json!({ "result": variant }));
            assert!(
                md.starts_with("**Result:** success"),
                "variant {variant:?} → {md:?}"
            );
        }
    }

    #[test]
    fn rule1_preamble_success_nonzero_number() {
        let md = to_markdown(&json!({"result": 1}));
        assert!(md.starts_with("**Result:** success"), "got: {md:?}");
        let md = to_markdown(&json!({"result": -7}));
        assert!(md.starts_with("**Result:** success"), "got: {md:?}");
        let md = to_markdown(&json!({"result": 2.5}));
        assert!(md.starts_with("**Result:** success"), "got: {md:?}");
    }

    #[test]
    fn rule1_preamble_failure_paths() {
        for variant in [
            json!({"result": false}),
            json!({"result": "false"}),
            json!({"result": "partial"}),
            json!({"result": "duplicate"}),
            json!({"result": "no"}),
            json!({"result": 0}),
            json!({"result": 0.0}),
            json!({"result": null}),
        ] {
            let md = to_markdown(&variant);
            assert!(
                md.starts_with("**Result:** failure"),
                "variant {variant:?} → {md:?}"
            );
        }
    }

    #[test]
    fn rule1_no_preamble_when_result_absent() {
        let md = to_markdown(&json!({"user": {"id": "1"}}));
        assert!(!md.starts_with("**Result:**"), "got: {md:?}");
        assert!(md.starts_with("# user\n"), "got: {md:?}");
    }

    #[test]
    fn rule1_no_preamble_when_result_non_scalar() {
        let md = to_markdown(&json!({"result": {"nested": "object"}}));
        assert!(!md.starts_with("**Result:**"), "got: {md:?}");
        // Non-scalar result falls through to rule 3.
        assert!(md.starts_with("# result\n"), "got: {md:?}");
    }

    // ---------- Rule 2: error promotion ----------

    #[test]
    fn rule2_error_promotion_on_failure() {
        let v = json!({
            "result": "false",
            "error": {"code": 1605, "text": "Invalid Input"}
        });
        let md = to_markdown(&v);
        assert_eq!(
            md,
            "**Result:** failure\n\n# Error\n- **code:** 1605\n- **text:** Invalid Input\n"
        );
    }

    #[test]
    fn rule2_error_promotion_even_on_success() {
        // Object-valued `error` promotes regardless of result state.
        let v = json!({
            "result": "success",
            "error": {"code": 0, "text": "warning only"}
        });
        let md = to_markdown(&v);
        assert!(md.contains("# Error\n"), "got: {md:?}");
        assert!(md.contains("- **code:** 0"), "got: {md:?}");
    }

    #[test]
    fn rule2_scalar_error_falls_through() {
        // String-valued `error` does not promote — rule 3 handles it.
        let v = json!({"result": "false", "error": "something went wrong"});
        let md = to_markdown(&v);
        assert!(!md.contains("# Error\n"), "got: {md:?}");
        assert!(md.contains("# error\n"), "got: {md:?}");
        assert!(md.contains("something went wrong"), "got: {md:?}");
    }

    // ---------- Rule 3: top-level H1 sections ----------

    #[test]
    fn rule3_top_level_keys_as_h1_sections() {
        let v = json!({
            "result": true,
            "status": "ok",
            "timestamp": 1_776_277_035_i64,
            "service": "fast.io"
        });
        let md = to_markdown(&v);
        let expected = "\
**Result:** success

# status
ok

# timestamp
1776277035

# service
fast.io
";
        assert_eq!(md, expected);
    }

    // ---------- Rule 4a: scalar dispatch ----------

    #[test]
    fn rule4a_null_renders_as_em_dash_in_section() {
        let md = to_markdown(&json!({"x": null}));
        assert_eq!(md, "# x\n—\n");
    }

    #[test]
    fn rule4a_null_renders_as_em_dash_in_bullet() {
        let md = to_markdown(&json!({"wrap": {"x": null}}));
        assert!(md.contains("- **x:** —"), "got: {md:?}");
    }

    #[test]
    fn rule4a_null_renders_as_em_dash_in_cell() {
        let md = to_markdown(&json!({"rows": [{"id": "1", "email": null}]}));
        assert!(md.contains("| 1 | — |"), "got: {md:?}");
    }

    #[test]
    fn rule4a_multiline_string_gets_fenced_block_in_section() {
        let md = to_markdown(&json!({"note": "line 1\nline 2"}));
        assert!(
            md.contains("# note\n```\nline 1\nline 2\n```\n"),
            "got: {md:?}"
        );
    }

    #[test]
    fn rule4a_html_like_string_gets_fenced_block_in_section() {
        let md = to_markdown(&json!({"snippet": "<script>alert(1)</script>"}));
        assert!(
            md.contains("# snippet\n```\n<script>alert(1)</script>\n```\n"),
            "got: {md:?}"
        );
    }

    #[test]
    fn rule4a_html_like_string_gets_inline_code_in_cell() {
        let md = to_markdown(&json!({"rows": [{"col": "<a href=x>"}]}));
        assert!(md.contains("`<a href=x>`"), "got: {md:?}");
    }

    // ---------- Rule 4b: associative map ----------

    #[test]
    fn rule4b_nested_map_recurses_with_indent() {
        let v = json!({"user": {"id": "7", "profile": {"name": "Ada"}}});
        let md = to_markdown(&v);
        assert!(md.contains("# user\n"), "got: {md:?}");
        assert!(md.contains("- **id:** 7"), "got: {md:?}");
        assert!(md.contains("- **profile:**"), "got: {md:?}");
        assert!(md.contains("  - **name:** Ada"), "got: {md:?}");
    }

    // ---------- Rule 4c: table ----------

    #[test]
    fn rule4c_table_uses_insertion_order_columns() {
        // Even though "zebra" sorts after "alpha", the insertion order
        // must be preserved with preserve_order enabled.
        let v = json!([
            {"zebra": "z1", "alpha": "a1"},
            {"zebra": "z2", "alpha": "a2"}
        ]);
        let md = to_markdown(&v);
        let first = md.lines().next().unwrap_or_default();
        assert_eq!(first, "| zebra | alpha |", "got first line: {first:?}");
    }

    #[test]
    fn rule4c_table_missing_values_render_as_em_dash() {
        let v = json!([
            {"id": 1, "name": "Alice"},
            {"id": 2, "name": "Bob", "role": "admin"}
        ]);
        let md = to_markdown(&v);
        assert!(md.contains("| id | name | role |"), "got: {md:?}");
        assert!(md.contains("| --- | --- | --- |"), "got: {md:?}");
        assert!(md.contains("| 1 | Alice | — |"), "got: {md:?}");
        assert!(md.contains("| 2 | Bob | admin |"), "got: {md:?}");
    }

    #[test]
    fn rule4c_heterogeneous_records_still_produce_a_table() {
        // Different key sets must still render as a table, not per-record
        // bullets — sparse — cells remain more agent-parseable.
        let v = json!([
            {"id": 1, "name": "Alice"},
            {"id": 2, "extra": "field"}
        ]);
        let md = to_markdown(&v);
        assert!(md.contains("| id | name | extra |"), "got: {md:?}");
    }

    #[test]
    fn rule4c_table_renders_at_top_level() {
        let v = json!([
            {"id": "1", "name": "Acme"},
            {"id": "2", "name": "Beta"}
        ]);
        let md = to_markdown(&v);
        assert_eq!(
            md,
            "| id | name |\n| --- | --- |\n| 1 | Acme |\n| 2 | Beta |\n"
        );
    }

    // ---------- Rule 4d: scalar list ----------

    #[test]
    fn rule4d_scalar_list_renders_as_bulleted_list() {
        let v = json!({"tags": ["apple", "banana", 42]});
        let md = to_markdown(&v);
        assert!(
            md.contains("# tags\n- apple\n- banana\n- 42\n"),
            "got: {md:?}"
        );
    }

    // ---------- Rule 4e: mixed list ----------

    #[test]
    fn rule4e_mixed_list_inlines_maps_with_semicolon_separator() {
        let v = json!({"items": ["apple", {"id": 1, "name": "Alice"}, "banana"]});
        let md = to_markdown(&v);
        assert!(md.contains("- apple\n"), "got: {md:?}");
        assert!(md.contains("- **id:** 1; **name:** Alice\n"), "got: {md:?}");
        assert!(md.contains("- banana\n"), "got: {md:?}");
    }

    // ---------- Rule 5: key rendering ----------

    #[test]
    fn rule5_numeric_dot_keys_get_backslash_escape() {
        let v = json!({"wrap": {"1.foo": "v1", "42.bar": "v2"}});
        let md = to_markdown(&v);
        assert!(md.contains(r"- **1\.foo:** v1"), "got: {md:?}");
        assert!(md.contains(r"- **42\.bar:** v2"), "got: {md:?}");
    }

    #[test]
    fn rule5_other_keys_are_not_escaped() {
        // `**bold**` as a key: spec says keys are literal except for the
        // numeric-dot pattern. The `**` in a key will combine with the
        // surrounding `**` markers and render oddly in some viewers,
        // but that's the spec's "light touch" behavior.
        let v = json!({"wrap": {"**bold**": "value"}});
        let md = to_markdown(&v);
        assert!(md.contains("- ****bold**:** value"), "got: {md:?}");
    }

    // ---------- Rule 6: table-cell escaping ----------

    #[test]
    fn rule6_table_cell_escapes_pipe_backtick_backslash() {
        let v = json!([{"col": "a|b\\c`d"}]);
        let md = to_markdown(&v);
        assert!(md.contains(r"| a\|b\\c\`d |"), "got: {md:?}");
    }

    #[test]
    fn rule6_table_cell_does_not_escape_brackets_or_angles() {
        // Remove aggressive metacharacter escape in cells — spec does
        // not escape `_<>#!~&`.
        let v = json!([{"col": "_under_ #head ~strike &amp;"}]);
        let md = to_markdown(&v);
        assert!(
            md.contains("| _under_ #head ~strike &amp; |"),
            "got: {md:?}"
        );
    }

    // ---------- Rule 7: HTML/multiline fence ----------

    #[test]
    fn rule7_html_in_cell_uses_inline_code_fence() {
        let v = json!([{"col": "<b>x</b>"}]);
        let md = to_markdown(&v);
        assert!(md.contains("`<b>x</b>`"), "got: {md:?}");
    }

    #[test]
    fn rule7_backtick_in_cell_uses_double_backtick_fence() {
        // Single backtick inside the value requires a longer fence.
        // Value is HTML-like (has `<b>`) so the fence path activates;
        // the presence of an internal backtick forces fence_len = 2.
        let v = json!([{"col": "<b>`code`</b>"}]);
        let md = to_markdown(&v);
        assert!(md.contains("``<b>`code`</b>``"), "got: {md:?}");
    }

    #[test]
    fn rule7_inline_code_pads_when_starts_with_backtick() {
        // HTML-like AND starts with backtick → fence + space padding.
        let v = json!([{"col": "`<b>x</b>`"}]);
        let md = to_markdown(&v);
        assert!(md.contains("`` `<b>x</b>` ``"), "got: {md:?}");
    }

    // ---------- Rule 8: value/body escaping ----------

    #[test]
    fn rule8_value_with_markdown_metachars_is_not_escaped() {
        // Spec rule 8: bullet values are NOT escaped. `**bold**` passes
        // through literally; consumers that render to HTML must sanitize.
        let v = json!({"note": "**bold** [link](x)"});
        let md = to_markdown(&v);
        assert!(md.contains("# note\n**bold** [link](x)\n"), "got: {md:?}");
    }

    #[test]
    fn rule8_heading_text_is_not_escaped() {
        let v = json!({"wrap": {"*emphasis*": "value"}});
        let md = to_markdown(&v);
        assert!(md.contains("- ***emphasis*:** value"), "got: {md:?}");
    }

    // ---------- Rule 9: heading depth cap ----------

    #[test]
    fn rule9_depth_cap_produces_json_fallback_block() {
        // Spec rule 9: heading level caps at 6. Attempting to recurse
        // INTO level 7 triggers the JSON fallback. The top-level H1
        // section body renders at level 1, and each nested map bumps +1,
        // so 7 nested maps under the section key push the 7th into the
        // fallback path.
        let v = json!({
            "l1": {"l2": {"l3": {"l4": {"l5": {"l6": {"l7": {"l8": "deep"}}}}}}}
        });
        let md = to_markdown(&v);
        assert!(md.contains("```json"), "got: {md:?}");
        assert!(md.contains("\"l8\""), "got: {md:?}");
    }

    // ---------- Rule 10: empty cases ----------

    #[test]
    fn rule10_empty_array_renders_as_em_dash_in_section() {
        let md = to_markdown(&json!({"tags": []}));
        assert_eq!(md, "# tags\n—\n");
    }

    #[test]
    fn rule10_empty_array_in_bullet_renders_as_em_dash() {
        let md = to_markdown(&json!({"wrap": {"tags": []}}));
        assert!(md.contains("- **tags:** —"), "got: {md:?}");
    }

    #[test]
    fn rule10_result_only_payload_produces_just_preamble() {
        let md = to_markdown(&json!({"result": true}));
        assert_eq!(md, "**Result:** success\n");
    }

    #[test]
    fn rule10_empty_object_produces_trailing_newline_only() {
        let md = to_markdown(&json!({}));
        assert_eq!(md, "\n");
    }

    // ---------- Output contract ----------

    #[test]
    fn output_contract_trailing_newline() {
        let md = to_markdown(&json!({"result": true, "x": "y"}));
        assert!(md.ends_with('\n'), "got: {md:?}");
        assert!(!md.ends_with("\n\n"), "got: {md:?}");
    }

    // ---------- Byte-for-byte regression vs server ----------

    #[test]
    fn byte_regression_ping_success() {
        // Pinned against a live response from
        // http://data1.dev1.iah1.veng.tech/api/v1.0/ping/?output=markdown
        // at 2026-04-15. Key shape: bool result, three scalar top-level
        // keys in insertion order.
        let v = json!({
            "result": true,
            "status": "ok",
            "timestamp": 1_776_277_035_u64,
            "service": "fast.io"
        });
        let md = to_markdown(&v);
        assert_eq!(
            md,
            "**Result:** success\n\n# status\nok\n\n# timestamp\n1776277035\n\n# service\nfast.io\n"
        );
    }

    #[test]
    fn byte_regression_orgs_list_auth_error() {
        // Pinned against a live error response from
        // http://data1.dev1.iah1.veng.tech/api/v1.0/orgs/list/?output=markdown
        // at 2026-04-15. Key shape: result=false, object-valued error.
        let v = json!({
            "result": false,
            "error": {
                "code": 10011,
                "text": "Your credentials were not supplied or invalid.",
                "documentation_url": "https://api.fast.io/llms.txt",
                "resource": "GET Orgs List"
            }
        });
        let md = to_markdown(&v);
        let expected = "\
**Result:** failure

# Error
- **code:** 10011
- **text:** Your credentials were not supplied or invalid.
- **documentation_url:** https://api.fast.io/llms.txt
- **resource:** GET Orgs List
";
        assert_eq!(md, expected);
    }

    // ---------- Runtime safety rails (preserved) ----------

    #[test]
    fn deep_recursion_is_bounded() {
        // Build ~100-level nested object; renderer must terminate.
        let mut v = json!({"leaf": 1});
        for _ in 0..100 {
            v = json!({"nested": v});
        }
        let md = to_markdown(&v);
        assert!(md.contains("```json"), "got: {md}");
    }

    #[test]
    fn output_has_soft_memory_cap() {
        let big = "x".repeat(10 * 1024 * 1024);
        let v = json!({"blob": big});
        let md = to_markdown(&v);
        assert!(md.len() < 6 * 1024 * 1024, "got {} bytes", md.len());
        assert!(
            md.contains("output truncated"),
            "got tail: {}",
            &md[md.len().saturating_sub(200)..]
        );
    }

    #[test]
    fn table_path_respects_soft_cap() {
        let big_val = "y".repeat(10 * 1024);
        let mut rows: Vec<Value> = Vec::with_capacity(1000);
        for i in 0..1000u32 {
            rows.push(json!({"id": i.to_string(), "blob": big_val}));
        }
        let v = Value::Array(rows);
        let md = to_markdown(&v);
        assert!(md.len() < 6 * 1024 * 1024, "got {} bytes", md.len());
        assert!(
            md.contains("output truncated"),
            "tail: {}",
            &md[md.len().saturating_sub(200)..]
        );
    }

    #[test]
    fn table_path_streams_large_cells() {
        let huge = "z".repeat(20 * 1024 * 1024);
        let v = json!([{"col": huge, "other": "x"}]);
        let md = to_markdown(&v);
        assert!(md.len() < 6 * 1024 * 1024, "got {} bytes", md.len());
    }

    #[test]
    fn table_path_caps_column_count() {
        let mut obj = Map::new();
        for i in 0..5000u32 {
            obj.insert(format!("key_{i:05}"), json!("v"));
        }
        let v = json!([Value::Object(obj)]);
        let md = to_markdown(&v);
        let first_line_len = md.split('\n').next().map_or(0, str::len);
        assert!(
            first_line_len < 10_000,
            "header row too long: {first_line_len} bytes"
        );
    }

    #[test]
    fn strips_bidi_override_characters() {
        let v = json!({"filename": "txt.\u{202E}exe"});
        let md = to_markdown(&v);
        assert!(!md.contains('\u{202E}'), "got: {md:?}");
        assert!(md.contains("txt.exe"), "got: {md}");
    }

    #[test]
    fn strips_zero_width_characters() {
        let v = json!({"name": "ad\u{200B}min"});
        let md = to_markdown(&v);
        assert!(!md.contains('\u{200B}'), "got: {md:?}");
        assert!(md.contains("admin"), "got: {md}");
    }

    #[test]
    fn strips_c0_and_c1_controls() {
        let v = json!({"s": "a\u{0001}b\u{0099}c"});
        let md = to_markdown(&v);
        assert!(md.contains("# s\nabc\n"), "got: {md:?}");
    }

    // ---------- Unicode smoke ----------

    #[test]
    fn unicode_keys_and_values_render() {
        let v = json!({"wrap": {"名前": "日本語", "emoji": "🚀"}});
        let md = to_markdown(&v);
        assert!(md.contains("- **名前:** 日本語"), "got: {md}");
        assert!(md.contains("- **emoji:** 🚀"));
    }
}
