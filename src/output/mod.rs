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
/// Terminal markdown renderer for `fastio view`.
pub mod view;

use std::io::{IsTerminal, Write};

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

/// Server-side response-verbosity level, selected by the global `--detail`
/// flag and threaded into envelope GET requests as `?output=<detail>`.
///
/// This is **orthogonal** to [`OutputFormat`]: `--detail` controls how much
/// data the *server* returns (smaller payloads, fewer tokens), while
/// `--format` controls how the client *renders* whatever it received. The
/// tokens map 1:1 onto the documented server `output=` detail levels
/// (`terse`/`standard`/`full`); `full` is the server default and equivalent
/// to omitting the parameter.
///
/// `#[non_exhaustive]` because the server may add detail levels without an
/// API-version bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputDetail {
    /// Smallest useful shape: identifiers and navigation fields only.
    Terse,
    /// `terse` plus the operational context most list/detail views render.
    Standard,
    /// The complete resource shape (server default).
    Full,
}

impl OutputDetail {
    /// The server query token for this detail level.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terse => "terse",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }

    /// Parse a `--detail` flag value, returning `None` for an unrecognized or
    /// absent token (the caller then injects nothing and the server applies
    /// its `full` default).
    #[must_use]
    pub fn from_flag(s: Option<&str>) -> Option<Self> {
        match s {
            Some("terse") => Some(Self::Terse),
            Some("standard") => Some(Self::Standard),
            Some("full") => Some(Self::Full),
            _ => None,
        }
    }
}

impl std::fmt::Display for OutputDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
    /// The output format to use (how the client renders the data).
    pub format: OutputFormat,
    /// Optional field filter (comma-separated field names).
    pub fields: Option<Vec<String>>,
    /// Disable colored output.
    pub no_color: bool,
    /// Suppress all output.
    pub quiet: bool,
    /// Optional server-side verbosity (`--detail`); threaded into envelope
    /// GETs as `?output=<detail>`. Orthogonal to [`OutputConfig::format`].
    pub detail: Option<OutputDetail>,
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
        Self::from_flags_detail(format, fields, no_color, quiet, None)
    }

    /// Build an `OutputConfig` from CLI flags, including the `--detail`
    /// server-verbosity flag.
    #[must_use]
    pub fn from_flags_detail(
        format: Option<&str>,
        fields: Option<&str>,
        no_color: bool,
        quiet: bool,
        detail: Option<&str>,
    ) -> Self {
        Self {
            format: OutputFormat::from_str_or_default(format),
            fields: fields.map(|f| f.split(',').map(|s| s.trim().to_owned()).collect()),
            no_color,
            quiet,
            detail: OutputDetail::from_flag(detail),
        }
    }

    /// Render a JSON value to stdout using the configured format.
    pub fn render(&self, value: &Value) -> Result<(), std::io::Error> {
        if self.quiet {
            return Ok(());
        }

        let filtered = format::filter_fields(value, self.fields.as_deref());

        // Unified-search responses carry a top-level `buckets` map. The
        // default `flatten_response` path returns only the FIRST array it
        // finds, which would silently drop every bucket but one — so detect
        // the grouped shape and render each bucket as its own labelled
        // section. JSON passthrough is unchanged (the bucket structure is
        // already faithfully serialized).
        if self.format != OutputFormat::Json
            && let Value::Object(map) = &filtered
            && let Some(Value::Object(buckets)) = map.get("buckets")
        {
            return render_buckets(buckets, self.format, self.no_color);
        }

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

/// Render a unified-search `buckets` map (one bucket per result type) as a
/// sequence of labelled sections, one per bucket, in insertion order.
///
/// Each section emits a heading (bucket name + a human-readable pagination
/// summary), surfaces any `status == "degraded"` and `total_relation == "gte"`
/// conditions as visible notices, and then renders the bucket's `items` array
/// in the requested format. Every user-controlled string flows through
/// [`markdown::sanitize_inline`] so the bucket-aware path carries the same
/// Trojan-Source / control-character defenses as the main markdown renderer.
///
/// **CSV is special-cased:** labelled section headers are plain text and
/// per-bucket CSV fragments each carry their own header row, so emitting them
/// back to back yields output no CSV parser can read. For CSV this instead
/// renders ONE table — every bucket's items flattened into a single array with
/// a leading `bucket` and `status` column (and a sentinel row per empty/
/// degraded bucket) — so the whole stream is one valid CSV document. Table and
/// markdown keep the labelled-section layout.
///
/// This deliberately does NOT route through [`flatten_response`], which would
/// collapse the grouped structure to a single bucket's items.
fn render_buckets(
    buckets: &serde_json::Map<String, Value>,
    format: OutputFormat,
    no_color: bool,
) -> Result<(), std::io::Error> {
    // Headings + notices are written to the in-process buffer; each bucket's
    // items are then rendered by the existing table/CSV/markdown renderers,
    // which lock and write stdout themselves. The buffer is flushed before
    // each delegated render so section ordering stays deterministic.
    // Markdown is fully buffered (single string covering every bucket) so the
    // exact byte shape is unit-testable; table/CSV stream because their
    // renderers own stdout.
    if format == OutputFormat::Markdown {
        let mut stdout = std::io::stdout().lock();
        return stdout.write_all(buckets_to_markdown(buckets).as_bytes());
    }

    // CSV must be a SINGLE parseable stream — labelled section headers and
    // per-bucket CSV fragments (the old behavior) produce text interleaved
    // with multiple independent header rows, which no CSV parser can read. So
    // flatten every bucket into one array of records carrying a leading
    // `bucket` (and `status`) column and render it as one table.
    if format == OutputFormat::Csv {
        return csv_output::render(&buckets_to_csv_rows(buckets));
    }

    let mut stdout = std::io::stdout().lock();
    let mut first = true;
    for (name, bucket) in buckets {
        let mut header = String::new();
        write_bucket_header(&mut header, name, bucket, format, first);
        first = false;
        stdout.write_all(header.as_bytes())?;
        stdout.flush()?;

        let items = bucket.get("items").cloned().unwrap_or(Value::Array(vec![]));
        if items.as_array().is_some_and(Vec::is_empty) {
            stdout.write_all(b"(no results)\n")?;
            continue;
        }
        match format {
            OutputFormat::Table => table::render(&items, no_color)?,
            // CSV is handled by the single-stream path above; Markdown is
            // fully buffered above. Both branches are unreachable here, but we
            // degrade to a table render rather than panic so a future refactor
            // fails soft (no `unreachable!` in a production path).
            OutputFormat::Csv | OutputFormat::Markdown | OutputFormat::Json => {
                table::render(&items, no_color)?;
            }
        }
    }
    Ok(())
}

/// The reserved leading/metadata column names a bucket CSV row carries, which
/// item fields must never shadow. Kept in one place so the shadow guard and the
/// column writers can't drift apart.
const BUCKET_CSV_RESERVED_COLUMNS: &[&str] = &[
    "bucket",
    "status",
    "note",
    "bucket_total",
    "bucket_total_relation",
    "bucket_has_more",
    "bucket_offset",
    "bucket_limit",
];

/// Insert the per-bucket pagination/metadata columns into a CSV `row`, in a
/// fixed order so the single CSV table keeps consistent columns across every
/// bucket. Emits `bucket_total`, `bucket_total_relation`, `bucket_has_more`,
/// `bucket_offset`, and `bucket_limit` whenever the bucket carries them. The
/// `bucket_total_relation` value (`gte` vs `eq`) is the lower-bound signal a CSV
/// consumer otherwise could not see, since the human-readable `bucket_notices`
/// path is markdown/table-only.
fn insert_bucket_metadata_columns(row: &mut serde_json::Map<String, Value>, bucket: &Value) {
    let Value::Object(map) = bucket else {
        return;
    };
    if let Some(total) = map.get("total").and_then(Value::as_u64) {
        row.insert("bucket_total".to_owned(), Value::from(total));
    }
    if let Some(rel) = map.get("total_relation").and_then(Value::as_str) {
        row.insert(
            "bucket_total_relation".to_owned(),
            Value::String(rel.to_owned()),
        );
    }
    if let Some(has_more) = map.get("has_more").and_then(Value::as_bool) {
        row.insert("bucket_has_more".to_owned(), Value::Bool(has_more));
    }
    if let Some(offset) = map.get("offset").and_then(Value::as_u64) {
        row.insert("bucket_offset".to_owned(), Value::from(offset));
    }
    if let Some(limit) = map.get("limit").and_then(Value::as_u64) {
        row.insert("bucket_limit".to_owned(), Value::from(limit));
    }
}

/// Flatten a unified-search `buckets` map into a single array of CSV records:
/// one record per item, each prefixed with a `bucket` column (the bucket name),
/// a `status` column (the bucket's `status`, defaulting to `ok`), and the
/// per-bucket pagination metadata (`bucket_total`, `bucket_total_relation`,
/// `bucket_has_more`, `bucket_offset`, `bucket_limit`) so a CSV consumer can
/// tell a total is approximate (`bucket_total_relation == "gte"`) or that more
/// results exist (`bucket_has_more == true`) — signals that the non-CSV
/// `bucket_notices` header path otherwise keeps to itself. Empty or degraded
/// buckets still contribute a single sentinel row (no `id`/item fields, just the
/// `bucket`/`status`/metadata columns and a `note`) so the CSV faithfully
/// reports every bucket — including ones the server returned empty or degraded —
/// in one coherent table. Insertion order puts `bucket`/`status` first, then the
/// `bucket_*` metadata columns, then item fields.
fn buckets_to_csv_rows(buckets: &serde_json::Map<String, Value>) -> Value {
    let mut rows = Vec::new();
    for (name, bucket) in buckets {
        let status = bucket
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("ok")
            .to_owned();
        let degraded = status == "degraded";
        let items = bucket.get("items").and_then(Value::as_array);
        let non_empty = items.is_some_and(|a| !a.is_empty());
        if non_empty {
            for item in items.into_iter().flatten() {
                let mut row = serde_json::Map::new();
                row.insert("bucket".to_owned(), Value::String(name.clone()));
                row.insert("status".to_owned(), Value::String(status.clone()));
                insert_bucket_metadata_columns(&mut row, bucket);
                if let Value::Object(obj) = item {
                    for (k, v) in obj {
                        // Don't let item fields shadow the leading/metadata cols.
                        if !BUCKET_CSV_RESERVED_COLUMNS.contains(&k.as_str()) {
                            row.insert(k.clone(), v.clone());
                        }
                    }
                }
                rows.push(Value::Object(row));
            }
        } else {
            // Sentinel row for an empty (or empty-degraded) bucket.
            let mut row = serde_json::Map::new();
            row.insert("bucket".to_owned(), Value::String(name.clone()));
            row.insert("status".to_owned(), Value::String(status.clone()));
            insert_bucket_metadata_columns(&mut row, bucket);
            let note = if degraded {
                "degraded: backend temporarily unavailable; results may be incomplete"
            } else {
                "no results"
            };
            row.insert("note".to_owned(), Value::String(note.to_owned()));
            rows.push(Value::Object(row));
        }
    }
    Value::Array(rows)
}

/// Render every bucket as a single markdown string: an `## <bucket>` heading
/// (with pagination summary + notices) followed by that bucket's items as a
/// GFM table (or `_No results._`). This is the buffered, fully-testable path
/// for [`OutputFormat::Markdown`] and is the regression guard that ALL buckets
/// are rendered — never just the first (the `flatten_response` bug).
fn buckets_to_markdown(buckets: &serde_json::Map<String, Value>) -> String {
    let mut out = String::new();
    let mut first = true;
    for (name, bucket) in buckets {
        write_bucket_header(&mut out, name, bucket, OutputFormat::Markdown, first);
        first = false;
        let items = bucket.get("items").cloned().unwrap_or(Value::Array(vec![]));
        if items.as_array().is_some_and(Vec::is_empty) {
            out.push_str("_No results._\n");
        } else {
            out.push_str(&markdown::to_markdown(&items));
        }
    }
    out
}

/// Write a single bucket's heading line (plus pagination summary and any
/// notices) into `out`. Separated from [`render_buckets`] so its formatting is
/// unit-testable without capturing stdout. `first` controls the blank-line
/// separator that precedes every bucket except the first.
fn write_bucket_header(
    out: &mut String,
    name: &str,
    bucket: &Value,
    format: OutputFormat,
    first: bool,
) {
    use std::fmt::Write as _;
    if !first {
        out.push('\n');
    }
    let name = markdown::sanitize_inline(name);
    let summary = bucket_summary(bucket);
    // Writing into a String via `fmt::Write` is infallible.
    let _ = match format {
        OutputFormat::Markdown => write!(out, "## {name}{summary}\n\n"),
        _ => writeln!(out, "=== {name}{summary} ==="),
    };
    for notice in bucket_notices(bucket) {
        out.push_str(&notice);
        out.push('\n');
    }
}

/// Build the trailing pagination summary appended to a bucket heading, e.g.
/// ` (total 1, offset 0, limit 10)`. Returns an empty string when no
/// pagination fields are present.
fn bucket_summary(bucket: &Value) -> String {
    let Value::Object(map) = bucket else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(t) = map.get("total").and_then(Value::as_u64) {
        parts.push(format!("total {t}"));
    }
    if let Some(o) = map.get("offset").and_then(Value::as_u64) {
        parts.push(format!("offset {o}"));
    }
    if let Some(l) = map.get("limit").and_then(Value::as_u64) {
        parts.push(format!("limit {l}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// Collect the human-readable per-bucket notices (degraded backend,
/// lower-bound count, more-results-available) to print under a bucket heading.
fn bucket_notices(bucket: &Value) -> Vec<String> {
    let Value::Object(map) = bucket else {
        return Vec::new();
    };
    let mut notices = Vec::new();
    if map.get("status").and_then(Value::as_str) == Some("degraded") {
        notices.push(
            "! degraded: this bucket's backend was temporarily unavailable; \
             results may be incomplete (safe to retry)."
                .to_owned(),
        );
    }
    if map.get("total_relation").and_then(Value::as_str) == Some("gte") {
        let total = map.get("total").and_then(Value::as_u64).unwrap_or(0);
        notices.push(format!(
            "~ total is a lower bound (≥ {total}); more matches may exist beyond the searched window."
        ));
    }
    notices
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
///
/// `pub(crate)` so search-path tests can assert the end-to-end flatten shape
/// of a normalized search response; it is otherwise an internal helper.
pub(crate) fn flatten_response(value: &Value) -> Value {
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
    fn output_detail_parses_known_tokens() {
        assert_eq!(
            OutputDetail::from_flag(Some("terse")),
            Some(OutputDetail::Terse)
        );
        assert_eq!(
            OutputDetail::from_flag(Some("standard")),
            Some(OutputDetail::Standard)
        );
        assert_eq!(
            OutputDetail::from_flag(Some("full")),
            Some(OutputDetail::Full)
        );
    }

    #[test]
    fn output_detail_unknown_or_absent_is_none() {
        assert_eq!(OutputDetail::from_flag(None), None);
        assert_eq!(OutputDetail::from_flag(Some("verbose")), None);
        assert_eq!(OutputDetail::from_flag(Some("")), None);
    }

    #[test]
    fn output_detail_round_trips_token() {
        assert_eq!(OutputDetail::Terse.as_str(), "terse");
        assert_eq!(OutputDetail::Standard.as_str(), "standard");
        assert_eq!(OutputDetail::Full.as_str(), "full");
        assert_eq!(OutputDetail::Standard.to_string(), "standard");
    }

    // ── Bucket-aware unified-search renderer ────────────────────────────

    /// A representative multi-bucket unified-search response: every applicable
    /// bucket present, one with `status: degraded`, one with
    /// `total_relation: gte`.
    fn multi_bucket_fixture() -> serde_json::Map<String, Value> {
        let v = json!({
            "files": {
                "items": [{"node_id": "f1", "name": "Q4 Report.pdf", "relevance_score": 0.93}],
                "offset": 0, "limit": 10, "total": 1, "total_relation": "eq",
                "has_more": false, "status": "ok"
            },
            "metadata": {
                "items": [{"node_id": "m1", "name": "Invoice.pdf", "template_ids": ["t1"]}],
                "offset": 0, "limit": 25, "total": 50, "total_relation": "gte",
                "has_more": true, "status": "ok"
            },
            "comments": {
                "items": [{"comment_id": "c1", "snippet": "double-check totals"}],
                "offset": 0, "limit": 5, "total": 1, "total_relation": "eq",
                "has_more": false, "status": "ok"
            },
            "workflows": {
                "items": [],
                "offset": 0, "limit": 25, "total": 0, "total_relation": "eq",
                "has_more": false, "status": "degraded"
            }
        });
        v.as_object().unwrap().clone()
    }

    #[test]
    fn buckets_markdown_renders_all_buckets_not_just_first() {
        // Regression guard for the `flatten_response` bug: the lossy flattener
        // returns only the first array, which would silently drop every
        // bucket but `files`. The bucket-aware renderer must emit ALL of them.
        let buckets = multi_bucket_fixture();
        let md = buckets_to_markdown(&buckets);
        for name in ["files", "metadata", "comments", "workflows"] {
            assert!(
                md.contains(&format!("## {name}")),
                "missing bucket: {name}\n{md}"
            );
        }
        // Items from each non-empty bucket appear.
        assert!(md.contains("Q4 Report.pdf"), "{md}");
        assert!(md.contains("Invoice.pdf"), "{md}");
        assert!(md.contains("double-check totals"), "{md}");
    }

    #[test]
    fn buckets_markdown_surfaces_degraded_and_gte() {
        let buckets = multi_bucket_fixture();
        let md = buckets_to_markdown(&buckets);
        assert!(md.contains("degraded"), "degraded notice missing:\n{md}");
        assert!(md.contains("lower bound"), "gte notice missing:\n{md}");
        // The empty degraded bucket still renders its heading + a no-results line.
        assert!(md.contains("## workflows"), "{md}");
        assert!(md.contains("_No results._"), "{md}");
    }

    #[test]
    fn flatten_response_would_collapse_buckets_documenting_the_bug() {
        // This is why unified search must NOT route through `flatten_response`:
        // given the `{result, buckets}` envelope, the flattener returns the
        // single `buckets` object (pass 2's nested-object fallback) — which a
        // table/CSV renderer would then treat as ONE row of bucket→object
        // columns, not four labelled result sections. The bucket-aware path
        // exists precisely to avoid this. Locked here so a future refactor
        // can't silently re-route bucket output through the flattener.
        let buckets = multi_bucket_fixture();
        let envelope = json!({ "result": true, "buckets": Value::Object(buckets) });
        let flattened = flatten_response(&envelope);
        // It collapses to the bucket map as a single object — NOT a sequence
        // of per-bucket renderable sections.
        assert!(flattened.is_object(), "got: {flattened}");
        let obj = flattened.as_object().unwrap();
        // The four bucket keys survive as object keys, but as a single flat
        // object the renderer cannot label/paginate them as sections; the
        // dedicated bucket renderer (asserted above) is required.
        assert!(obj.contains_key("files") && obj.contains_key("workflows"));
    }

    #[test]
    fn bucket_summary_formats_pagination() {
        let bucket = json!({"total": 50, "offset": 0, "limit": 25});
        assert_eq!(bucket_summary(&bucket), " (total 50, offset 0, limit 25)");
        assert_eq!(bucket_summary(&json!({})), "");
        assert_eq!(bucket_summary(&json!("x")), "");
    }

    #[test]
    fn bucket_notices_reports_degraded_and_gte() {
        let degraded = json!({"status": "degraded"});
        assert!(
            bucket_notices(&degraded)
                .iter()
                .any(|n| n.contains("degraded"))
        );
        let gte = json!({"total_relation": "gte", "total": 99});
        let notices = bucket_notices(&gte);
        assert!(
            notices
                .iter()
                .any(|n| n.contains("lower bound") && n.contains("99"))
        );
        let ok = json!({"status": "ok", "total_relation": "eq"});
        assert!(bucket_notices(&ok).is_empty());
    }

    #[test]
    fn every_bucket_gets_a_header_even_when_empty() {
        // The streaming table/CSV path and the buffered markdown path both
        // iterate EVERY bucket. `write_bucket_header` is the shared, stdout-
        // free entry point; assert a header is produced for each bucket
        // (including the empty degraded one) — the regression guard against
        // dropping all-but-one bucket.
        let buckets = multi_bucket_fixture();
        for (name, bucket) in &buckets {
            let mut header = String::new();
            write_bucket_header(&mut header, name, bucket, OutputFormat::Csv, false);
            assert!(
                header.contains(name),
                "no header for bucket {name}: {header}"
            );
        }
    }

    #[test]
    fn bucket_header_sanitizes_hostile_bucket_name() {
        // A bidi-override / control char in a bucket key must be stripped.
        let mut out = String::new();
        write_bucket_header(
            &mut out,
            "files\u{202E}\u{0007}",
            &json!({"total": 1}),
            OutputFormat::Markdown,
            true,
        );
        assert!(!out.contains('\u{202E}'), "bidi override leaked: {out:?}");
        assert!(!out.contains('\u{0007}'), "control char leaked: {out:?}");
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

    #[test]
    fn buckets_csv_is_one_parseable_table_with_bucket_column() {
        // FIX 2 regression guard: the CSV path must be a SINGLE valid CSV
        // document — one header row, a leading `bucket` column — not text
        // headers interleaved with multiple CSV fragments.
        let buckets = multi_bucket_fixture();
        let rows = buckets_to_csv_rows(&buckets);
        let arr = rows.as_array().expect("array of rows");

        // Every bucket is represented: files/metadata/comments have one item
        // each; the empty degraded `workflows` bucket gets a sentinel row.
        let bucket_names: Vec<&str> = arr
            .iter()
            .filter_map(|r| r.get("bucket").and_then(Value::as_str))
            .collect();
        for name in ["files", "metadata", "comments", "workflows"] {
            assert!(bucket_names.contains(&name), "missing bucket {name}");
        }

        // Leading columns are present and ordered first on every row.
        for row in arr {
            let obj = row.as_object().unwrap();
            let first_two: Vec<&String> = obj.keys().take(2).collect();
            assert_eq!(first_two, vec!["bucket", "status"], "row: {row}");
        }

        // The degraded empty bucket carries a status + note sentinel.
        let workflows = arr
            .iter()
            .find(|r| r.get("bucket").and_then(Value::as_str) == Some("workflows"))
            .expect("workflows row");
        assert_eq!(workflows["status"], "degraded");
        assert!(
            workflows["note"].as_str().unwrap().contains("degraded"),
            "{workflows}"
        );

        // Render through the real CSV writer and re-parse it: exactly ONE
        // header row, and every data row resolves a `bucket` field.
        csv_output::render(&rows).expect("csv render");
        let headers = csv_output::collect_headers(arr);
        assert_eq!(headers[0], "bucket");
        assert_eq!(headers[1], "status");
        // A non-empty bucket's item fields are present as columns.
        assert!(headers.iter().any(|h| h == "name"), "headers: {headers:?}");
    }

    #[test]
    fn buckets_csv_item_fields_do_not_shadow_leading_columns() {
        // An item that itself carries reserved column keys (`bucket`/`status`
        // or any `bucket_*` metadata column) must not overwrite the synthesized
        // leading/metadata columns.
        let mut buckets = serde_json::Map::new();
        buckets.insert(
            "files".to_owned(),
            json!({
                "status": "ok",
                "total": 7, "total_relation": "gte", "has_more": true,
                "offset": 0, "limit": 10,
                "items": [{
                    "bucket": "EVIL", "status": "EVIL",
                    "bucket_total": 999, "bucket_total_relation": "EVIL",
                    "bucket_has_more": false, "bucket_offset": 999, "bucket_limit": 999,
                    "name": "x"
                }]
            }),
        );
        let rows = buckets_to_csv_rows(&buckets);
        let row = &rows.as_array().unwrap()[0];
        assert_eq!(row["bucket"], "files");
        assert_eq!(row["status"], "ok");
        // Metadata columns reflect the BUCKET, not the item's spoofed values.
        assert_eq!(row["bucket_total"], 7);
        assert_eq!(row["bucket_total_relation"], "gte");
        assert_eq!(row["bucket_has_more"], true);
        assert_eq!(row["bucket_offset"], 0);
        assert_eq!(row["bucket_limit"], 10);
        assert_eq!(row["name"], "x");
    }

    #[test]
    fn buckets_csv_surfaces_gte_and_has_more_metadata() {
        // FIX 2 regression guard: a `total_relation: "gte"` / `has_more: true`
        // bucket must surface those lower-bound signals in the CSV, since the
        // human-readable `bucket_notices` path is markdown/table-only. Assert
        // via the real CSV writer + header collection so the columns are proven
        // to land in one parseable table.
        let buckets = multi_bucket_fixture();
        let rows = buckets_to_csv_rows(&buckets);
        let arr = rows.as_array().expect("array of rows");

        // The `metadata` bucket is the gte/has_more one in the fixture.
        let meta = arr
            .iter()
            .find(|r| r.get("bucket").and_then(Value::as_str) == Some("metadata"))
            .expect("metadata row");
        assert_eq!(meta["bucket_total"], 50);
        assert_eq!(meta["bucket_total_relation"], "gte");
        assert_eq!(meta["bucket_has_more"], true);

        // The `files` bucket is the eq/no-more one.
        let files = arr
            .iter()
            .find(|r| r.get("bucket").and_then(Value::as_str) == Some("files"))
            .expect("files row");
        assert_eq!(files["bucket_total_relation"], "eq");
        assert_eq!(files["bucket_has_more"], false);

        // The metadata columns appear as headers in the single CSV table, and
        // CSV rendering succeeds (one parseable document).
        csv_output::render(&rows).expect("csv render");
        let headers = csv_output::collect_headers(arr);
        for col in [
            "bucket_total",
            "bucket_total_relation",
            "bucket_has_more",
            "bucket_offset",
            "bucket_limit",
        ] {
            assert!(
                headers.iter().any(|h| h == col),
                "missing metadata column {col}, headers: {headers:?}"
            );
        }
    }

    #[test]
    fn flatten_files_object_is_not_specially_converted() {
        // Regression guard: `flatten_response` is a GENERIC helper used by
        // every command. The storage-search files-MAP → rows conversion is
        // handled in the search command path (see `commands::files` /
        // `commands::ai` `normalize_search_response`), NOT here, so a future
        // endpoint that legitimately returns a top-level `files` object is not
        // silently restructured. Pass 2 returns the lone nested object as-is.
        let input = json!({
            "files": {
                "f1": {"name": "File 1", "type": "file"},
                "f2": {"name": "File 2", "type": "file"}
            }
        });
        let result = flatten_response(&input);
        assert!(result.is_object(), "expected object, got: {result}");
        assert!(result.as_object().unwrap().contains_key("f1"));
    }
}
