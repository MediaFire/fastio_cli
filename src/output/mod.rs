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

use std::borrow::Cow;
use std::io::{IsTerminal, Write};

use serde_json::Value;

/// Strip terminal-hostile bytes from a display string: C0/C1 control
/// characters (except `\n`/`\t`) and Unicode bidi / zero-width / BOM code
/// points — the exact set the markdown formatter strips (Trojan-Source
/// defense, [`markdown::is_stripped_char`]).
///
/// The table and CSV formatters call this on every emitted string: they render
/// server- and peer-authored text RAW (table is the TTY default), where an
/// unstripped ESC would let untrusted content — e.g. a room message body
/// authored by another agent — inject ANSI sequences into the viewer's
/// terminal. JSON output is unaffected (serde escapes control characters).
pub(crate) fn strip_terminal_hostile(s: &str) -> String {
    s.chars()
        .filter(|&c| !markdown::is_stripped_char(c))
        .collect()
}

/// The column key whose value carries an ordered `parts` array.
///
/// Coordination Rooms were removed (2026-08-25) and were the only producer of
/// this column, so the renderer is now INERT — deliberately retained, not
/// overlooked. Its tests assert a GENERAL property that is still live: the
/// fallback path sanitizes bidi / zero-width codepoints, so a malformed array
/// cannot opt out of Trojan-Source stripping. Deleting the renderer would take
/// that coverage with it for the sake of a dead column name.
///
/// Reference rendering is scoped to this exact key. A generic "array of tagged
/// objects" detector would silently restyle unrelated endpoints that happen to
/// return the same shape.
///
/// Lives here, beside [`format_reference_parts`], so the table and CSV
/// renderers scope on one shared constant instead of one peer reaching into
/// the other.
pub(crate) const PARTS_KEY: &str = "parts";

/// Render the file references inside a room message's `parts` array as
/// `label (id)`, in wire order, joined by `"; "`.
///
/// Text parts contribute nothing: the message `body` is already its own
/// column, so reconstructing the prose here would only duplicate it and blow
/// up the cell.
///
/// Returns `None` — leaving the caller on its existing JSON stringification —
/// whenever the value is not a parts array this renderer fully understands: a
/// non-array, an element that is not an object, a part with a missing or
/// unknown `type`, a reference without both `id` and `text`, or an array
/// carrying no reference at all. Degrading the whole cell (rather than the
/// offending part) is deliberate: dropping a part silently would desync the
/// rendered references from the `body` they belong to.
///
/// **`reference_type` is never read.** It names the KIND of target, not the
/// validity of the reference, so every reference renders the same way whatever
/// it points at — including kinds this build predates, such as the marker for a
/// reference the reader is not allowed to see. Branching on a particular value
/// would break the instant another kind ships, and would blank out exactly the
/// readers who see redacted content.
///
/// `text` is the label field. A reference may also carry `value`; both are
/// server-redacted per reader, and this renderer never re-derives a label from
/// anywhere else. Rendering an unrecognized kind is safe precisely because the
/// label is displayed verbatim: it is the server's own disclosure decision for
/// this reader, never interpreted here, never compared against a constant,
/// never reconstructed from `body`.
///
/// Lives here beside [`strip_terminal_hostile`] rather than in either renderer:
/// the table and CSV formatters are peers, so the one that owned it would be
/// reached into by the other. The strings it emits are peer-authored and
/// terminal-bound, and two copies of that logic could drift apart.
pub(crate) fn format_reference_parts(value: &Value) -> Option<String> {
    let parts = value.as_array()?;
    let mut references = Vec::new();

    for part in parts {
        let part = part.as_object()?;
        match part.get("type").and_then(Value::as_str)? {
            // A text part must still be well-formed: `value` is required and
            // must be a string. Its content is ignored (the `body` column
            // already carries the prose), but accepting a malformed text part
            // while rejecting a malformed reference part would be inconsistent
            // — the whole point of degrading is that the array is trustworthy
            // as a unit or not at all.
            "text" => {
                part.get("value").and_then(Value::as_str)?;
            }
            // A reference is a reference regardless of what it points at, so
            // `reference_type` is deliberately not consulted — see above.
            "reference" => {
                let id = part.get("id").and_then(Value::as_str)?;
                let label = part.get("text").and_then(Value::as_str)?;
                references.push(format!(
                    "{} ({})",
                    strip_terminal_hostile(label),
                    strip_terminal_hostile(id)
                ));
            }
            _ => return None,
        }
    }

    if references.is_empty() {
        return None;
    }
    Some(references.join("; "))
}

/// Keys to skip when searching for the primary data array in an API
/// response object. Includes both classic pagination/metadata wrappers
/// and the envelope-level `result` field, which flows through the
/// client now that markdown rendering needs it for the `**Result:**`
/// preamble.
const METADATA_KEYS: &[&str] = &["pagination", "meta", "links", "result"];

/// Pagination / count siblings that [`flatten_response`] may drop without
/// warning.
///
/// These are the "mild" class of the flatten defect: losing `count` off a
/// listing costs a number the caller can recompute or re-request, and warning
/// about it on every ordinary `files list` would bury the warnings that matter
/// under noise on the most-used commands.
///
/// Used in exactly TWO places, and the second one is deliberate:
///
/// 1. [`discarded_payload_keys`] — suppressed from the warning, per above.
/// 2. [`flatten_response`]'s data-bearing test — a sidecar does NOT count as a
///    data-bearing sibling, so `{items: [], total: 500}` still returns the
///    empty array and renders as an empty listing rather than a one-row object.
///    That is the behaviour the pass-1b guard exists to preserve.
///
/// **Neither use ever strips a key from an emitted value.** Use 1 only builds
/// the warning text; use 2 only selects WHICH payload is rendered. A sidecar
/// that survives into the chosen payload is emitted intact. (Stated this way
/// because use 2 *does* influence what is emitted, so the tempting shorter
/// claim — "never affects output" — would be false.)
///
/// Every entry here is a key OBSERVED on a real response, never a guessed
/// variant — an invented name reads as a contract and silences nothing.
/// `page_size` was added 2026-08-26 after the warning fired spuriously on
/// `metadata eligible` against a live server; it was the only false positive
/// across five live list commands (`workspace list`, `org list`, `files list`,
/// `share list`, `metadata eligible`).
const PAGINATION_SIDECAR_KEYS: &[&str] = &[
    "count",
    "total",
    "total_count",
    "offset",
    "limit",
    "page_size",
    "cursor",
    "next_cursor",
    "has_more",
    "more",
];

/// Per-response DIAGNOSTIC blocks that describe how the REQUEST was handled,
/// never payload the caller asked for.
///
/// They must be excluded on both sides of [`flatten_response`]: they are not
/// data-bearing siblings (so a zero-result listing beside one still renders as
/// an empty array), and they are not discarded payload (so a healthy response
/// carrying one does not warn that a field "was not shown").
///
/// 🔑 **Why this list exists separately from [`PAGINATION_SIDECAR_KEYS`]:** that
/// list was ENUMERATED from five commands that were being looked at at the time
/// — `workspace list`, `org list`, `files list`, `share list`,
/// `metadata eligible` — and **none of them is a search**. So the moment
/// `search_metadata` / `metadata_filter` appeared, a zero-match
/// `fastio files search` rendered its whole envelope as a one-row table and
/// every successful one printed a spurious "fields were not shown" warning.
/// The defining property is *"describes the request, not the payload"* — decide
/// membership by that, not by which commands happen to be in view.
const RESPONSE_ANNOTATION_KEYS: &[&str] = &["search_metadata", "metadata_filter"];

/// Whether a value carries a payload a caller would miss if it were dropped.
///
/// `null`, empty strings, empty arrays and empty objects carry nothing, so
/// their loss is not worth a warning and must not make a "no results" listing
/// render as an object. Numbers and booleans always count — `false` and `0`
/// are answers (`allowed: false` is precisely the field the flatten defect was
/// measured destroying on `comment list`).
fn is_data_bearing(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

/// Is `value` a **collection wrapper** — an object whose entire payload is an
/// `items` array plus pagination/envelope sidecars, e.g.
/// `{"count": 0, "items": []}`?
///
/// Such a wrapper is the CONTAINER of a collection candidate, never an
/// independent payload sibling of it, and [`flatten_response`]'s pass 1b must
/// not count it as one. It previously did: for
/// `{"nodes": {"count": 0, "items": []}, "pagination": {…}}` the `nodes` wrapper
/// was treated as a data-bearing sibling of its own empty `items`, pass 1b was
/// skipped, and an empty listing rendered as a **one-row object** — the exact
/// regression pass 1b exists to prevent, surviving only because the fixture set
/// covered flat-and-empty and nested-and-populated but not nested-and-EMPTY.
///
/// Deliberately conservative: a wrapper carrying any OTHER real payload beside
/// `items` is **not** matched here and still counts as data-bearing, so the
/// documented `{thread{}, turns{items[]}}` case is unaffected.
fn is_items_wrapper(value: &Value) -> bool {
    let Value::Object(inner) = value else {
        return false;
    };
    matches!(inner.get("items"), Some(Value::Array(_)))
        && inner.keys().all(|k| {
            k == "items"
                || PAGINATION_SIDECAR_KEYS.contains(&k.as_str())
                || METADATA_KEYS.contains(&k.as_str())
        })
}

/// Names of data-bearing payload keys that `flattened` does NOT represent.
///
/// `flatten_response` must pick ONE payload out of an envelope, and for table
/// and CSV that choice is irreducible: `{stats{}, workspaces[]}` (return the
/// array) and `{thread{}, turns{items[]}}` (return the object) are the SAME
/// SHAPE and want OPPOSITE answers, so no shape-only rule can be right for
/// both. What is fixable is the SILENCE — a caller who is told what was left
/// out can go get it with `--format json`, and one who is not has no way to
/// know the answer they are reading is partial.
///
/// Envelope keys ([`METADATA_KEYS`]), pagination sidecars
/// ([`PAGINATION_SIDECAR_KEYS`]) and per-response annotations
/// ([`RESPONSE_ANNOTATION_KEYS`]) are excluded so the warning stays
/// high-signal — otherwise every healthy `files search` on an AI-enabled
/// workspace warns about its own `search_metadata`.
pub(crate) fn discarded_payload_keys(original: &Value, flattened: &Value) -> Vec<String> {
    let Value::Object(map) = original else {
        return Vec::new();
    };
    // The whole object rendered — nothing was dropped.
    if flattened == original {
        return Vec::new();
    }
    map.iter()
        .filter(|(key, val)| {
            let key = key.as_str();
            if METADATA_KEYS.contains(&key)
                || PAGINATION_SIDECAR_KEYS.contains(&key)
                || RESPONSE_ANNOTATION_KEYS.contains(&key)
            {
                return false;
            }
            if !is_data_bearing(val) {
                return false;
            }
            // The key that produced the rendered payload, either directly or
            // via its nested `items` array, is not "discarded".
            *val != flattened && val.get("items") != Some(flattened)
        })
        .map(|(key, _)| key.clone())
        .collect()
}

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

        // Borrow when there is nothing to filter. `filter_fields` returns
        // `value.clone()` on both of its early-return paths, so calling it
        // unconditionally made EVERY render deep-clone the entire response body
        // for nothing — and the no-`--fields` case is the common one, on
        // listings that are the largest payloads the CLI handles.
        let filtered: Cow<'_, Value> = match self.fields.as_deref() {
            Some(f) if !f.is_empty() => Cow::Owned(format::filter_fields(value, Some(f))),
            _ => Cow::Borrowed(value),
        };
        let filtered = filtered.as_ref();

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
            OutputFormat::Json => json::render(filtered),
            OutputFormat::Table => {
                let flattened = flatten_response(filtered);
                warn_discarded_payload(filtered, &flattened, "table");
                table::render(&flattened, self.no_color)
            }
            OutputFormat::Csv => {
                let flattened = flatten_response(filtered);
                warn_discarded_payload(filtered, &flattened, "csv");
                csv_output::render(&flattened)
            }
            // Markdown renders the full envelope (preamble + H1
            // sections), so it MUST NOT go through `flatten_response`;
            // flattening strips the envelope and breaks rules 1 and 3
            // of the server contract.
            OutputFormat::Markdown => markdown::render(filtered),
        }
    }
}

/// Warn on stderr that the flattened render is showing only part of the
/// response, naming the payload fields it left out.
///
/// This is the [`crate::output`] instance of the pattern
/// `warn_if_content_search_unavailable` already ships in the command layer:
/// read the shortfall BEFORE rendering, report it on **stderr**, and leave
/// stdout byte-clean so `--format csv` stays machine-parseable. It works in
/// every format and needs no per-endpoint knowledge.
///
/// Silent by design when nothing data-bearing was dropped, so ordinary
/// listings stay quiet.
fn warn_discarded_payload(original: &Value, flattened: &Value, format: &str) {
    let dropped = discarded_payload_keys(original, flattened);
    if dropped.is_empty() {
        return;
    }
    eprintln!(
        "warning: `--format {format}` can render only one payload, so these response fields were \
         not shown: {}. Use `--format json` for the complete response.",
        dropped.join(", ")
    );
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
    //
    // NON-EMPTY only. An empty array is the NORMAL state of many payload
    // siblings (`comments` on an uncommented file, `topics_used` on a simple
    // question, `restricted_extensions` on a default instance,
    // `template_metadata` on a post-cutover node), and returning one renders
    // ZERO BYTES while discarding every data-bearing sibling — an empty file
    // with a success exit code. Skipping empties preserves the original
    // array-preference intent and removes the total-loss case.
    for (key, val) in map {
        if METADATA_KEYS.contains(&key.as_str()) {
            continue;
        }
        match val {
            Value::Array(items) if !items.is_empty() => return val.clone(),
            Value::Object(inner) => {
                if let Some(Value::Array(items)) = inner.get("items")
                    && !items.is_empty()
                {
                    return inner["items"].clone();
                }
            }
            _ => {}
        }
    }

    // Pass 1b: every candidate array is EMPTY. If nothing else in the envelope
    // carries data, return the empty array exactly as before — a genuine "no
    // results" listing must keep rendering as an empty table / headerless CSV,
    // never as a one-row object, or every scripted consumer of an empty
    // `files list` breaks. Only when a data-bearing sibling exists do we fall
    // through and let the whole object render instead.
    let has_data_bearing_sibling = map.iter().any(|(key, val)| {
        !METADATA_KEYS.contains(&key.as_str())
            && !PAGINATION_SIDECAR_KEYS.contains(&key.as_str())
            && !RESPONSE_ANNOTATION_KEYS.contains(&key.as_str())
            && !matches!(val, Value::Array(_))
            && !is_items_wrapper(val)
            && is_data_bearing(val)
    });
    if !has_data_bearing_sibling {
        for (key, val) in map {
            if METADATA_KEYS.contains(&key.as_str()) {
                continue;
            }
            match val {
                Value::Array(_) => return val.clone(),
                Value::Object(inner) => {
                    if let Some(items @ Value::Array(_)) = inner.get("items") {
                        return items.clone();
                    }
                }
                _ => {}
            }
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
    fn flatten_empty_first_array_no_longer_destroys_the_response() {
        // MEASURED (`comment list`, 2026-08-22): `{result, comments[], count,
        // allowed, remaining}` with an EMPTY `comments` rendered 0 bytes of CSV
        // with exit 0, discarding `allowed`/`remaining` — so a user on a file
        // with no comments never learned whether commenting was permitted.
        let input = json!({
            "result": "yes",
            "comments": [],
            "count": 0,
            "allowed": false,
            "remaining": 5
        });
        let result = flatten_response(&input);
        assert!(
            result.is_object(),
            "an empty array must not win over data-bearing siblings, got: {result}"
        );
        assert_eq!(result["allowed"], json!(false));
        assert_eq!(result["remaining"], json!(5));
    }

    #[test]
    fn flatten_keeps_a_genuine_empty_listing_as_an_empty_array() {
        // The other side of the same rule, and the regression that would hurt
        // most: a listing that is legitimately empty must keep rendering as an
        // empty table / headerless CSV. Turning "no results" into a one-row
        // object would break every scripted consumer of an empty `files list`.
        let input = json!({"shares": [], "pagination": {"offset": 0, "limit": 25}});
        let result = flatten_response(&input);
        assert!(result.is_array(), "got: {result}");
        assert_eq!(result.as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn flatten_keeps_a_nested_empty_listing_as_an_empty_array() {
        // THE CELL THE FIXTURE SET WAS MISSING, and where the bug actually was.
        // The matrix is {flat, nested} x {empty, populated}: flat-empty is the
        // test above, nested-populated is `flatten_extracts_nested_items_array`,
        // and nested-EMPTY had no test at all — so pass 1b counting the `nodes`
        // wrapper as a data-bearing SIBLING of its own empty `items` went
        // unnoticed and turned an empty listing into a one-row object.
        let input = json!({
            "nodes": {"count": 0, "items": []},
            "pagination": {"offset": 0, "limit": 25}
        });
        let result = flatten_response(&input);
        assert!(result.is_array(), "got: {result}");
        assert_eq!(result.as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn flatten_keeps_a_zero_match_search_as_an_empty_array() {
        // The REAL `files search` shape, and the case `is_items_wrapper` does
        // NOT cover: `search_metadata` is a genuine annotation object with no
        // `items` child, so it is not a collection wrapper — it was counted as
        // a data-bearing SIBLING and a zero-match search rendered the WHOLE
        // ENVELOPE as a one-row table/CSV.
        //
        // The output SHAPE therefore depended on whether there were hits:
        // per-file rows when something matched, `result,files,search_metadata`
        // when nothing did. The API emits `search_metadata` whenever the
        // response is hybrid (AI enabled) OR `search_in` was supplied, so this
        // is the ordinary path, not an edge case.
        let input = json!({
            "result": true,
            "files": [],
            "search_metadata": {
                "intelligence_enabled": true,
                "semantic_available": true,
                "scoped": false
            }
        });
        let result = flatten_response(&input);
        assert!(result.is_array(), "got: {result}");
        assert_eq!(result.as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn flatten_keeps_a_zero_match_filtered_search_as_an_empty_array() {
        // Same branch via the NEW `--filters` path: a filtered search always
        // carries a top-level `metadata_filter` annotation.
        let input = json!({
            "result": true,
            "files": [],
            "metadata_filter": {"applied": true, "matched": 0}
        });
        let result = flatten_response(&input);
        assert!(result.is_array(), "got: {result}");
        assert_eq!(result.as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn successful_search_does_not_warn_about_its_own_annotation_blocks() {
        // The opposite branch of the same root cause: on a search WITH hits,
        // `search_metadata` / `metadata_filter` were reported as payload the
        // user "was not shown", printing a spurious warning on EVERY healthy
        // `files search` in table or CSV. They describe the REQUEST, not
        // payload, so they are never a discarded field.
        let original = json!({
            "result": true,
            "files": [{"id": "a"}],
            "search_metadata": {"intelligence_enabled": true},
            "metadata_filter": {"applied": true, "matched": 1}
        });
        let flattened = json!([{"id": "a"}]);
        assert!(
            discarded_payload_keys(&original, &flattened).is_empty(),
            "annotation blocks must not be reported as discarded payload"
        );
    }

    #[test]
    fn flatten_still_prefers_a_real_sibling_over_an_empty_items_wrapper() {
        // The guard above must NOT swallow the documented opposite case. A
        // wrapper carrying real payload beside `items` is not a collection
        // container, so `thread` still wins and this stays an object.
        let input = json!({
            "thread": {"id": "t1", "subject": "hi"},
            "turns": {"count": 0, "items": []}
        });
        let result = flatten_response(&input);
        assert!(result.is_object(), "got: {result}");
    }

    #[test]
    fn flatten_prefers_a_populated_array_over_an_empty_one() {
        // MEASURED (`upload extensions`): the first array was empty and
        // `archive_extensions` was invisible in table/CSV.
        let input = json!({
            "restricted_extensions": [],
            "archive_extensions": ["zip", "tar"]
        });
        let result = flatten_response(&input);
        assert_eq!(result, json!(["zip", "tar"]));
    }

    #[test]
    fn discarded_keys_name_the_scalar_payload_the_array_displaced() {
        // MEASURED (`how-to`): the whole point of the command is the scalar
        // `answer`, and pass 1 walks past it to `topics_used[]`. The choice
        // itself is irreducible; being SILENT about it is what is fixable.
        let input = json!({
            "result": "yes",
            "status": "answered",
            "answer": "Use `fastio share create`.",
            "escalated": false,
            "topics_used": ["shares"]
        });
        let flattened = flatten_response(&input);
        assert_eq!(flattened, json!(["shares"]));
        let dropped = discarded_payload_keys(&input, &flattened);
        assert!(dropped.contains(&"answer".to_owned()), "got: {dropped:?}");
        assert!(
            dropped.contains(&"escalated".to_owned()),
            "got: {dropped:?}"
        );
        // `result` is envelope, never reported.
        assert!(!dropped.contains(&"result".to_owned()), "got: {dropped:?}");
    }

    #[test]
    fn discarded_keys_stay_quiet_on_a_cursor_paginated_listing() {
        // MEASURED against a live `metadata eligible` response, whose envelope
        // is exactly this shape. Before `page_size` joined the sidecar list,
        // every `--format table|csv` render of it emitted a spurious warning
        // naming `page_size` — the precise noise the sidecar list exists to
        // prevent, on a command that is pure pagination plumbing.
        let input = json!({
            "result": "yes",
            "count": 100,
            "page_size": 100,
            "cursor": "YzEwODlmOWRk",
            "has_more": true,
            "items": [{"node_id": "2jlez", "name": "probe.txt"}]
        });
        let flattened = flatten_response(&input);
        assert!(flattened.is_array(), "got: {flattened}");
        assert!(
            discarded_payload_keys(&input, &flattened).is_empty(),
            "cursor-paginated listing must not warn, got: {:?}",
            discarded_payload_keys(&input, &flattened)
        );
    }

    #[test]
    fn discarded_keys_stay_quiet_on_an_ordinary_listing() {
        // Pagination sidecars are the MILD class; warning about them on every
        // `files list` would bury the warnings that matter.
        let input = json!({
            "files": [{"id": "1"}],
            "count": 1,
            "total": 1,
            "pagination": {"offset": 0}
        });
        let flattened = flatten_response(&input);
        assert!(
            discarded_payload_keys(&input, &flattened).is_empty(),
            "ordinary listing must not warn"
        );
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
            // A synthetic extra bucket exercising the degraded + empty render
            // path — the renderer is bucket-name-agnostic.
            "extra": {
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
        for name in ["files", "metadata", "comments", "extra"] {
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
        assert!(md.contains("## extra"), "{md}");
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
        assert!(obj.contains_key("files") && obj.contains_key("extra"));
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
        // the 2026-04-15 review.
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
        // each; the empty degraded `extra` bucket gets a sentinel row.
        let bucket_names: Vec<&str> = arr
            .iter()
            .filter_map(|r| r.get("bucket").and_then(Value::as_str))
            .collect();
        for name in ["files", "metadata", "comments", "extra"] {
            assert!(bucket_names.contains(&name), "missing bucket {name}");
        }

        // Leading columns are present and ordered first on every row.
        for row in arr {
            let obj = row.as_object().unwrap();
            let first_two: Vec<&String> = obj.keys().take(2).collect();
            assert_eq!(first_two, vec!["bucket", "status"], "row: {row}");
        }

        // The degraded empty bucket carries a status + note sentinel.
        let extra = arr
            .iter()
            .find(|r| r.get("bucket").and_then(Value::as_str) == Some("extra"))
            .expect("extra row");
        assert_eq!(extra["status"], "degraded");
        assert!(
            extra["note"].as_str().unwrap().contains("degraded"),
            "{extra}"
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
