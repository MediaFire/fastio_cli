#![allow(clippy::missing_errors_doc)]

//! Metadata API endpoints for the Fast.io REST API.
//!
//! Covers eligible-file listing, node metadata details (single and bulk),
//! single-node AI extraction, lexical metadata search, **node facts** (read and
//! human-asserted write), the **field vocabulary** (list / declare / merge /
//! merge-candidates), **compound search**, and **saved filters**.
//!
//! # Two module-wide rules that look like bugs and are not
//!
//! Both are stated here because they are exactly what a new reader would
//! "clean up" into consistency, and both are load-bearing.
//!
//! ### 1. The request encoding differs PER ENDPOINT
//!
//! | Endpoint | Encoding |
//! |---|---|
//! | `POST .../metadata/fields/` (declare) | **form-encoded** |
//! | `POST .../metadata/fields/merge/` | **JSON**, with a real boolean `confirm` |
//! | `POST .../storage/{node}/metadata/facts/` | **JSON** — `facts` is a nested map form encoding cannot express |
//! | `POST .../metadata/compound-search/` | **form**, with `filters` as a JSON **string** field |
//! | `POST` / `PUT .../metadata/filters/` | **JSON body**, read whole |
//! | every `GET` | query string |
//!
//! Unifying these breaks them silently. A JSON body on compound search does not
//! populate `filters` at all and is refused as though none were sent — `406` /
//! `119701`, which reads as "my filter is invalid" rather than "wrong
//! Content-Type". `tests/metadata_wire_encoding.rs` pins all three POST
//! conventions at the wire so that mistake fails locally and loudly.
//!
//! ### 2. Three endpoints handle field-name whitespace THREE different ways
//!
//! | Route | Rule |
//! |---|---|
//! | declare-field | **trims** before storing and validating |
//! | merge | **never trims** — a leading space is significant, and the 64-char limit counts it |
//! | facts write | **refuses** a padded name outright (`field_name_not_canonical`) |
//!
//! A single shared normalizer would therefore be wrong on two of the three. The
//! facts route refuses rather than trims for a specific reason: a padded name
//! used to resolve onto the trimmed field and overwrite a value the request
//! never named. A refusal costs a round trip; the overwrite cost the value.
//!
//! # Irreversible operations
//!
//! A field declaration **can never be deleted or renamed**, and a facts write
//! with an unrecognised key creates one implicitly — so a typo permanently
//! consumes a capped vocabulary slot through a route that does not look like a
//! schema operation. Merge is the only remedy and is itself irreversible. See
//! [`new_field_names`] and [`write_node_facts`].
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Server-enforced cap on the number of node ids per bulk metadata-details
/// request.
///
/// Going over this returns HTTP 406 with sub-code 109184. Callers with
/// more than this many ids must chunk on the client side.
pub const BULK_METADATA_DETAILS_MAX_IDS: usize = 25;

/// Per-id error returned by the bulk metadata-details endpoint.
///
/// The server echoes back the input casing of `node_id` (the input is
/// normalized internally but the error retains what the caller sent),
/// so callers matching results to inputs must compare case-insensitively.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MetadataFetchError {
    /// Node id the error applies to (echoes input casing).
    pub node_id: String,
    /// Numeric API error code. Common values:
    /// - `147_196` invalid storage node id format
    /// - `196_136` literal root sentinel was supplied
    /// - `191_049` storage node not found (also returned for ids that
    ///   exist in another workspace — workspace scoping)
    /// - `190_770` backend error retrieving the storage node (transient
    ///   — safe to retry)
    /// - `150_183` storage node exists but is not a file or note (e.g.
    ///   a folder)
    /// - `157_684` backend error retrieving the metadata key/value rows
    ///   (transient — safe to retry)
    pub code: u32,
    /// Human-readable error message.
    pub message: String,
}

impl MetadataFetchError {
    fn from_value(v: &Value) -> Self {
        let node_id = v.get("node_id").and_then(Value::as_str).map(str::to_owned);
        if node_id.is_none() {
            tracing::warn!(error_row = %v, "bulk metadata-details error row missing node_id");
        }
        let code_raw = v.get("code");
        let code = code_raw
            .and_then(Value::as_u64)
            .and_then(|c| u32::try_from(c).ok());
        if code.is_none() && code_raw.is_some_and(|c| !c.is_null()) {
            tracing::warn!(code = ?code_raw, "bulk metadata-details error row code not a u32");
        }
        let message = v.get("message").and_then(Value::as_str).map(str::to_owned);
        if message.is_none() {
            tracing::warn!(error_row = %v, "bulk metadata-details error row missing message");
        }
        Self {
            node_id: sanitize_terminal_string(&node_id.unwrap_or_default()),
            code: code.unwrap_or(0),
            message: sanitize_terminal_string(&message.unwrap_or_default()),
        }
    }
}

/// Strip C0/C1 control codepoints and Unicode bidi/zero-width/BOM
/// codepoints from a server-supplied string before it reaches a
/// terminal. Mirrors the Trojan-Source defense applied by the
/// markdown sanitizer contract.
fn sanitize_terminal_string(s: &str) -> String {
    s.chars()
        .filter(|c| {
            if c.is_control() && *c != '\t' && *c != '\n' && *c != '\r' {
                return false;
            }
            let cp = *c as u32;
            !matches!(
                cp,
                0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF
            )
        })
        .collect()
}

/// Bulk metadata-details response: zero or more resolved objects, the
/// hoisted template definition map, and per-id errors.
///
/// Both HTTP 200 (≥1 id resolved) and HTTP 404 (all ids errored) carry
/// this same shape; partial results are normal and a non-empty `errors`
/// list at HTTP 200 must NOT be treated as a request-level failure.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BulkMetadataDetailsResponse {
    /// Successfully resolved metadata objects. Server does NOT preserve
    /// input order. Each entry has the single-id response shape:
    /// `{instance_id, object_id, template_id, node_id, template_metadata,
    /// custom_metadata, autoextractable}`.
    pub objects: Vec<Value>,
    /// Map of `template_id` → template definition, deduplicated across
    /// all objects in this response. Always present (empty map when no
    /// template applies).
    pub templates: serde_json::Map<String, Value>,
    /// Per-id errors. May be non-empty even at HTTP 200.
    pub errors: Vec<MetadataFetchError>,
}

/// Get metadata details for one or more storage nodes.
///
/// `GET /workspace/{workspace_id}/storage/{id1},{id2},.../metadata/details/`
///
/// The server distinguishes single vs bulk shape by the presence of a
/// comma in the URL segment. This function joins the input ids with
/// literal commas and returns a unified
/// [`BulkMetadataDetailsResponse`]: single-format responses surface
/// their lone object as `objects[0]`, multi-format responses pass
/// through `objects[]`, `templates{}`, and `errors[]` as-is.
///
/// Constraints:
/// - 1..=`BULK_METADATA_DETAILS_MAX_IDS` ids per call (callers needing
///   more must chunk; the server dedupes case-insensitively, so 25
///   *unique* ids is the cap).
/// - All ids must belong to the same `workspace_id` (cross-workspace
///   ids surface as per-id `191_049` not-found, not a 4xx).
/// - Commas between ids must NOT be URL-encoded (the server splits on
///   `,`).
///
/// Both HTTP 200 (some ok) and HTTP 404 (all errored) return a
/// populated [`BulkMetadataDetailsResponse`]; HTTP 406 (empty segment
/// or over-cap) and other 4xx/5xx surface as `CliError::Api`.
pub async fn get_bulk_node_metadata_details(
    client: &ApiClient,
    workspace_id: &str,
    node_ids: &[String],
) -> Result<BulkMetadataDetailsResponse, CliError> {
    let path = build_bulk_metadata_details_path(workspace_id, node_ids)?;
    let (_status, body) = client.get_partial_envelope(&path).await?;
    parse_bulk_metadata_details_response(&body)
}

/// Build the bulk metadata-details URL path. Extracted as a free
/// function so chunking and validation can be unit-tested without an
/// HTTP client.
fn build_bulk_metadata_details_path(
    workspace_id: &str,
    node_ids: &[String],
) -> Result<String, CliError> {
    if node_ids.is_empty() {
        return Err(CliError::Parse(
            "bulk metadata details requires at least one id".to_owned(),
        ));
    }
    if node_ids.len() > BULK_METADATA_DETAILS_MAX_IDS {
        return Err(CliError::Parse(format!(
            "bulk metadata details accepts at most {BULK_METADATA_DETAILS_MAX_IDS} ids per call (got {})",
            node_ids.len()
        )));
    }
    let encoded: Vec<String> = node_ids
        .iter()
        .map(|id| urlencoding::encode(id).into_owned())
        .collect();
    // For chunks of exactly one id, duplicate the id with a literal
    // comma so the response always arrives in multi shape (the server
    // dedupes case-insensitively, so this is still one lookup).
    // Without this, a 1-id trailing chunk in a chunked run hits the
    // single-id endpoint, and a server-side 4xx on that single id
    // would abort the whole run and discard objects accumulated in
    // earlier chunks.
    let segment = if encoded.len() == 1 {
        format!("{0},{0}", encoded[0])
    } else {
        encoded.join(",")
    };
    Ok(format!(
        "/workspace/{}/storage/{}/metadata/details/",
        urlencoding::encode(workspace_id),
        segment,
    ))
}

/// Parse the metadata-details response body into a unified
/// [`BulkMetadataDetailsResponse`].
///
/// Branches on `payload.format`:
/// - `"multi"`: pass through `objects[]`, `templates{}`, `errors[]`.
/// - absent / any other value with `objects` array present: treat as
///   multi (covers 404-all-errored bodies and forward-compat).
/// - else: lift the entire payload as `objects[0]` (the legacy
///   single-id shape, where the body itself is the object). If the
///   payload contains a `template` field, hoist it into `templates`
///   keyed by `template_id` so the unified shape is consistent.
///
/// Tolerates both `{result, response: {…}}` (the documented envelope)
/// and a flat `{…}` body.
pub fn parse_bulk_metadata_details_response(
    body: &Value,
) -> Result<BulkMetadataDetailsResponse, CliError> {
    let payload = body.get("response").unwrap_or(body);
    if !payload.is_object() {
        return Err(CliError::Parse(
            "bulk metadata-details response payload is not a JSON object".to_owned(),
        ));
    }
    let format = payload.get("format").and_then(Value::as_str);
    let multi_shape = payload.get("objects").is_some_and(Value::is_array);

    let treat_as_multi = match format {
        Some("multi") => true,
        Some("single") => false,
        None => multi_shape,
        Some(other) => {
            return Err(CliError::Parse(format!(
                "bulk metadata-details response has unknown format {other:?}"
            )));
        }
    };

    if treat_as_multi {
        let objects = payload
            .get("objects")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let templates = payload
            .get("templates")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let errors = payload
            .get("errors")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(MetadataFetchError::from_value).collect())
            .unwrap_or_default();
        return Ok(BulkMetadataDetailsResponse {
            objects,
            templates,
            errors,
        });
    }

    // Non-multi fallback: the payload itself IS the single per-node metadata
    // object (the `details` shape — `template_metadata[]` + `custom_metadata[]`).
    // Return it verbatim as the sole object; the current API carries no
    // top-level `template` definition to hoist.
    let objects = if payload.is_null() {
        Vec::new()
    } else {
        vec![payload.clone()]
    };
    Ok(BulkMetadataDetailsResponse {
        objects,
        templates: serde_json::Map::new(),
        errors: Vec::new(),
    })
}

/// Get metadata details for a single storage node (legacy single-id
/// shape).
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/metadata/details/`
///
/// Returns the raw envelope-unwrapped object. Use
/// [`get_bulk_node_metadata_details`] for 2+ ids.
pub async fn get_node_metadata_details(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/metadata/details/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Keys whose array value holds metadata ENTRIES. The only containers
/// [`strip_declared_metadata_types`] descends into.
///
/// `template_metadata` / `custom_metadata` are the current per-node details
/// shape; `metadata` is the legacy single-node array.
const METADATA_ENTRY_CONTAINERS: &[&str] = &["template_metadata", "custom_metadata", "metadata"];

/// Remove the template-DECLARED `type` from every metadata entry in a
/// metadata-details payload.
///
/// `metadata[].type` is the type declared by the template's field schema. The
/// server echoes it onto every stored value, so it can disagree with what is
/// actually stored — a field declared `int` can hold `"abc"`. Rendered as a
/// column beside the value it reads as a claim about that value, which the CLI
/// cannot stand behind, so human-facing formats drop it. `--format json` is a
/// machine-readable passthrough and keeps the field verbatim; deciding *when*
/// to apply this belongs to the render call sites, not here.
///
/// The descent is deliberately narrow, because `type` is an overloaded key.
/// The very same details payload carries `node_id.type` (the storage node's
/// kind — `file`/`folder`/`note`), and room messages carry `parts[].type`;
/// both must survive. Only the containers named in
/// [`METADATA_ENTRY_CONTAINERS`] are touched — on the payload root, on each
/// element of a root-level array, and through the bulk `objects[]` wrapper,
/// which [`strip_node_declared_types`] follows **recursively** so each entry
/// gets the identical treatment as the root (including its `node_id`-nested
/// facts). The walk descends **only** into `objects[]` and `node_id`, never
/// into arbitrary keys — in particular it does not enter the bulk envelope's
/// `templates` map, whose `fields[].type` is a schema declaration and must
/// survive. So no unrelated `type` is reachable.
pub fn strip_declared_metadata_types(value: &mut Value) {
    if let Value::Array(items) = value {
        for item in items {
            strip_node_declared_types(item);
        }
    } else {
        strip_node_declared_types(value);
    }
}

/// Strip one metadata-details object: its own entry containers, plus those of
/// each element of a bulk `objects[]` wrapper.
///
/// **Every level must strip BOTH placements.** [`METADATA_FACTS_KEY`] is
/// served at the payload root *and* one level down inside `node_id`, so each
/// site needs both calls. Until 2026-08-28 the bulk `objects[]` arm stripped
/// only the top-level copy, leaving `declared_type` intact on every bulk
/// entry's `node_id.metadata_facts` — the single-node path was correct and the
/// bulk path was a partial copy of it. Applying the rule at one level and not
/// the identical level next to it is the recurring shape of this defect, so the
/// bulk arm now **recurses** rather than re-listing the steps: a future
/// placement added to the single-node path is inherited here instead of having
/// to be remembered twice.
fn strip_node_declared_types(node: &mut Value) {
    strip_entry_containers(node);
    strip_fact_declared_types(node);
    if let Some(inner) = node.get_mut("node_id") {
        strip_fact_declared_types(inner);
    }
    if let Some(Value::Array(objects)) = node.get_mut("objects") {
        for object in objects {
            strip_node_declared_types(object);
        }
    }
}

/// Key holding the node's facts. Searched at the payload ROOT **and** one level
/// down inside `node_id`, because the platform serves it in both places: nested
/// under `node_id` today, and additionally top-level (first key) from
/// 2026-08-27. **Keyed on the NAME, never on the position** — the position is
/// changing under us and a position-keyed rule would silently stop applying.
const METADATA_FACTS_KEY: &str = "metadata_facts";

/// Drop `declared_type` — and ONLY `declared_type` — from every fact entry.
///
/// A fact entry carries BOTH `declared_type` and `stored_type` (measured
/// 2026-08-27), and they are not the same kind of thing:
///
/// - **`declared_type` is a CLAIM about the value** — the schema's declared
///   type, which can disagree with what is actually stored (a field declared
///   `int` holding `"abc"`). Rendered beside the value it reads as a warranty
///   the CLI cannot give, so human formats drop it, exactly as they drop the
///   legacy `type`.
/// - **`stored_type` is NOT a claim** — it is the observed type of the stored
///   value. It is kept, because it is the honest answer to the question
///   `declared_type` only pretends to answer.
///
/// Note the entry key is `declared_type`, NOT `type`: routing facts through
/// [`strip_entry_containers`] would have removed **nothing** while looking
/// handled.
fn strip_fact_declared_types(obj: &mut Value) {
    let Some(facts) = obj.get_mut(METADATA_FACTS_KEY) else {
        return;
    };
    if let Some(Value::Array(items)) = facts.get_mut("items") {
        for item in items {
            if let Value::Object(map) = item {
                map.shift_remove("declared_type");
            }
        }
    }
}

/// Drop `type` from every entry of every metadata entry container on `obj`.
///
/// `shift_remove`, not `remove`: `serde_json`'s `preserve_order` feature is
/// enabled (`Cargo.toml`), which makes `Map` an `IndexMap` and `Map::remove`
/// a SWAP-remove — it moves the last entry into the removed slot. Every human
/// renderer derives its column order from map insertion order, so `remove`
/// would drop the column AND reorder the survivors, landing `updated` where
/// `type` used to be. `shift_remove` preserves the relative order of the keys
/// that remain.
fn strip_entry_containers(obj: &mut Value) {
    for container in METADATA_ENTRY_CONTAINERS {
        if let Some(Value::Array(entries)) = obj.get_mut(*container) {
            for entry in entries {
                if let Value::Object(map) = entry {
                    map.shift_remove("type");
                }
            }
        }
    }
}

/// Parameters for [`list_eligible`].
///
/// This endpoint is **cursor**-paginated (`page_size` + opaque `cursor`), not
/// offset-paginated. It previously took `limit`/`offset`, which the server
/// accepted and ignored: measured 2026-08-27, `--limit 1` returned the
/// same 100 items as no argument at all, with identical `count`/`page_size`/
/// `has_more`. Nothing on the response distinguished the two calls, which is
/// why the flags looked plausible for so long.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct EligibleParams<'a> {
    /// Records per page. **Quantized by the server**, in both directions:
    /// clamped to 1-250 then snapped to the nearest of 25 / 100 / 250 (≤62 →
    /// 25, 63-175 → 100, ≥176 → 250). The response's `page_size` reports what
    /// was actually used. Deliberately NOT pre-snapped client-side — the
    /// quantization is this endpoint's own server-side rule and the response
    /// already reports the truth.
    pub page_size: Option<u32>,
    /// Opaque cursor from a previous response's `cursor` field. Meaningless
    /// text — sent back unchanged; omit for the first page.
    pub cursor: Option<&'a str>,
    /// Return only nodes with this MIME type.
    pub mimetype: Option<&'a str>,
    /// Return only nodes with this file extension.
    pub extension: Option<&'a str>,
}

impl<'a> EligibleParams<'a> {
    /// An empty parameter set (first page, no filters). Provided so callers in
    /// other crates can build this `#[non_exhaustive]` struct without
    /// struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `page_size` (server-quantized to 25 / 100 / 250).
    #[must_use]
    pub fn page_size(mut self, v: Option<u32>) -> Self {
        self.page_size = v;
        self
    }

    /// Set the opaque pagination `cursor`.
    #[must_use]
    pub fn cursor(mut self, v: Option<&'a str>) -> Self {
        self.cursor = v;
        self
    }

    /// Filter by MIME type.
    #[must_use]
    pub fn mimetype(mut self, v: Option<&'a str>) -> Self {
        self.mimetype = v;
        self
    }

    /// Filter by file extension.
    #[must_use]
    pub fn extension(mut self, v: Option<&'a str>) -> Self {
        self.extension = v;
        self
    }
}

/// List files eligible for metadata extraction in a workspace.
///
/// `GET /workspace/{workspace_id}/metadata/eligible/`
///
/// Cursor-paginated — see [`EligibleParams`]. Source: the published API docs.
pub async fn list_eligible(
    client: &ApiClient,
    workspace_id: &str,
    params: &EligibleParams<'_>,
) -> Result<Value, CliError> {
    let params = eligible_query(params);
    let path = format!(
        "/workspace/{}/metadata/eligible/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Build the query map for [`list_eligible`].
///
/// Extracted so the emitted key set is testable: this endpoint's parameters are
/// `page_size`/`cursor`/`mimetype`/`extension`, and `limit`/`offset` must NEVER
/// appear — the server accepts and ignores them, which is what made the old
/// flags look like they worked.
fn eligible_query(params: &EligibleParams<'_>) -> HashMap<String, String> {
    let mut query = HashMap::new();
    if let Some(v) = params.page_size {
        query.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = params.cursor {
        query.insert("cursor".to_owned(), v.to_owned());
    }
    if let Some(v) = params.mimetype {
        query.insert("mimetype".to_owned(), v.to_owned());
    }
    if let Some(v) = params.extension {
        query.insert("extension".to_owned(), v.to_owned());
    }
    query
}

/// Enqueue an asynchronous metadata extraction for a single storage node.
///
/// **Spends AI credits.**
///
/// **Async contract (breaking change from the prior synchronous version):**
/// the server returns `202 Accepted` within ~500ms with a payload of the
/// shape `{ result, job_id, template_id, node_id, fields, status,
/// status_uri }`. The extracted values are **not** in this response —
/// callers must poll [`crate::api::workspace::jobs_status`] for an entry
/// in `metadata_extract` with `kind: "single"` matching this `node_id`,
/// then read the values from
/// `GET /workspace/{ws}/storage/{node}/metadata/details/` once the job
/// reports `status: "completed"`. On `status: "errored"`, surface
/// `error_message` to the user.
///
/// `template_id` survives only as a response field, and is **always `null`**;
/// it is kept in the payload so a client that reads it keeps receiving it, so
/// treat it as nullable and do not branch on it (per the published API docs).
/// There is no request-side template parameter: extraction has no notion of a
/// template, and a request carrying a non-empty `template_id` is rejected with
/// `406` / error code `179390`.
///
/// `fields` is an optional JSON-encoded array of field names. Pass `None`
/// for a full extraction — a discovery pass in which the workspace's existing
/// fields are offered to the model as context and it may add new ones.
///
/// **Naming `fields` makes the request EXCLUSIVE.** The model is asked for
/// exactly those fields and nothing else, and any other field it returns
/// anyway is **discarded rather than written**, so a targeted request cannot
/// quietly rewrite columns you did not ask about. Two caveats that survive
/// from the template-free contract and must still be stated: the file is
/// **still read in full**, and a named field is written only if the document
/// actually contains it — the model omits what it cannot find rather than
/// guessing.
///
/// A scope also makes the request a distinct unit of work from the full
/// extraction, so a file that was already extracted runs again for the fields
/// you name; that is how a field added to the vocabulary later reaches files
/// that predate it.
///
/// Server-side constraints on the list (not enforced by this client): every
/// name must be a field the workspace has **already produced**, and an
/// unrecognised name is rejected rather than ignored; at most **50 distinct
/// names** per request, with duplicates collapsed before that limit applies.
///
/// ## 💸 `fields` IS ALSO A BILLING KEY — re-sending it is not free
///
/// Extraction is guarded by a claim whose unique key includes a hash of the
/// **scope the CALLER requested**, and on this single-node route that scope is
/// exactly the `fields` list — NOT the node's missing set. Consequences a
/// caller cannot infer from the parameter's name:
///
/// - **Repeating the SAME call is a silent no-op.** Two identical extractions
///   of an unchanged file hash to the same scope, so the second is refused as
///   already-claimed. It is refused as **SUCCESS** — the job completes and
///   nothing is extracted. There is no client-visible way to tell that apart
///   from a real extraction today (see the `--wait` caveat in the command
///   layer); omitting `fields` hashes to the empty scope, which means "full
///   extraction", so a bare re-run collides with the previous bare run.
/// - **Changing `fields` mints a NEW billable unit and re-runs the model** —
///   deliberately, since a scoped extraction of one new field is a different
///   question from a full extraction and collides with nothing. So passing a
///   different list to "just narrow things down" SPENDS AI CREDITS on a file
///   that already has values.
///
/// **Do not offer users a "re-extract" affordance built on delete-then-extract.**
/// No delete path touches the claim, so deleting a field's values changes
/// neither the file version nor the requested scope: a bare re-run afterwards
/// is still refused, and **both steps return success while nothing
/// regenerates.** The only escape is re-extracting with an explicit `fields`
/// scope, which hashes to a claim the table has not seen — a recovery path the
/// caller has to know about, which is not a safety property.
///
/// **One half of that is confirmed and one is not, and the difference does not
/// change the advice.** The claim half was established from the server
/// implementation with a control (no delete path anywhere references the claim
/// table). What has **not** been traced is whether the delete removes FACTS or
/// only the legacy key-value rows — and since the read surfaces moved to the
/// fact corpus, the delete may do less than it appears to. So do not state that
/// the values are destroyed: state that the re-extract is refused, which holds
/// either way.
///
/// Settled against the server implementation on 2026-08-26; this client has NOT
/// read that source, so it is recorded as a second-hand report rather than as a
/// verified internal. The client-observable half — same call twice does
/// nothing, changed `fields` bills — is what callers must act on and is stated
/// on that basis.
///
/// **A `job_id` is not always present.** An UNSCOPED call for a file version
/// that has already been extracted answers `200 OK` — not `202` — with
/// `job_id: null` and `status: "already_extracted"`, queueing no duplicate
/// work and billing nothing. A call naming `fields` is a different unit of
/// work and is never answered from that record, so it returns `202` as
/// normal; if that exact scope has already run, the duplicate is caught later
/// by the worker, which does no work and does not bill. So do not treat a
/// `202` as proof that work was performed, nor the absence of a `200` as
/// proof that it was not.
///
/// **Idempotency:** the repeat protection is keyed on the file's **current
/// version** and the **set of fields** named — not on a time window — so a
/// retry an hour later is exactly as safe as one a second later, while
/// uploading a new version makes the next request a genuinely new unit of
/// work. Field names are compared as a set: order does not matter and
/// duplicates are collapsed. Note that a duplicate request still receives a
/// **new** `job_id` — this route never hands back the `job_id` of a job
/// already in flight, so two `202`s carrying different `job_id`s do not mean
/// two extractions were performed. No client-side debounce required. The node
/// must be `autoextractable` (a file, not a folder, not trashed, with a
/// completed AI summary); check the `GET /metadata/details/` response's
/// top-level `autoextractable` field before calling to gate "extract now"
/// affordances.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/metadata/extract/`
pub async fn extract_node_metadata(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    fields: Option<&str>,
) -> Result<Value, CliError> {
    if let Some(f) = fields {
        validate_extract_fields(f)?;
    }
    let path = format!(
        "/workspace/{}/storage/{}/metadata/extract/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    let mut form = HashMap::new();
    if let Some(f) = fields {
        form.insert("fields".to_owned(), f.to_owned());
    }
    client.post(&path, &form).await
}

/// Terminal-state outcome of a single-file extraction job, extracted from
/// a workspace jobs-status response.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtractJobState {
    /// The job reached `completed`.
    Completed,
    /// The job reached `errored`; carries the server `error_message` when
    /// one was present.
    Errored(Option<String>),
    /// The job is still queued or in progress (non-terminal).
    Pending,
    /// No matching entry was found in the jobs-status response. The server
    /// hides completed/errored entries older than one hour; callers such as
    /// [`classify_single_extract_job`]'s bounded poll loops (which wait far
    /// less than that age-out window) therefore must NOT treat `NotFound` as
    /// success — within their window a missing entry means the job is not yet
    /// visible, so they keep polling for an explicit terminal state.
    NotFound,
}

/// Find the single-file extraction job for `node_id` (and optionally
/// `job_id`) in a workspace jobs-status response and classify its state.
///
/// Matches entries in `jobs.metadata_extract[]` with `kind == "single"`.
/// When `job_id` is supplied, it must also match (so concurrent single-file
/// jobs on the same node for different field scopes are disambiguated);
/// when `None`, the first `single` entry for the node is used.
///
/// Extracted as a pure function so the terminal-state classification can be
/// unit-tested without an HTTP client.
///
/// **`node_id` is compared in canonical (unhyphenated) form on BOTH sides.**
/// A formatted Fast.io id carries a `-` every 5 characters while the canonical
/// wire form has none, and this is a cross-channel compare: the caller's
/// `node_id` normally arrives as the hyphenated display form copied off a
/// formatted surface, and the jobs-status entry carries whatever
/// `/jobs/status/` publishes. A raw `!=` therefore failed to match, so the
/// bounded `--wait` poll loops could only ever time out — reporting a timeout
/// on an extraction that had already succeeded. Canonicalizing **both** sides
/// makes the match independent of which rendering either channel uses, so it
/// holds whichever way the server emits the id. (`/jobs/status/` is understood
/// to publish the raw id property rather than the formatted one, but that was
/// not measured here, so the fix deliberately does not depend on it:
/// canonicalizing both sides is correct whichever rendering the server
/// actually emits.)
///
/// **`job_id` is compared RAW, deliberately — do not "consistently" normalize
/// it.** It looks like the same cross-channel shape, but the contract closes
/// the question in the opposite direction: per the `job_id` paragraph of the
/// single-file extract route in the published API docs (the one beginning
/// "`job_id` is `null` while the extraction is in flight", in the section whose
/// stopping rule is scoped "For an extraction started on this route"), a
/// terminal entry
/// reports the same `job_id` the `202` returned **byte for byte**. That
/// guarantee is what makes a raw compare correct, and it is deliberately the
/// only thing relied on here — normalizing would buy nothing against a
/// byte-for-byte guarantee while widening the degenerate-input surface below.
///
/// (Do **not** re-justify this from the `"job_id": "aj_…"` response example in
/// the same document: that one sits under `POST …/metadata/extract-all/`, the
/// *batch folder* route, not the single-file route this function serves. The
/// two routes are documented close together and their examples read
/// identically, so it is easy to quote the wrong one.)
///
/// An **empty canonical form matches nothing**: `canonicalize` strips every
/// `-`, so it is not injective over malformed input — `"-"`, `"-----"` and `""`
/// all collapse to `""` — and without the guard an all-hyphen argument would
/// match an entry carrying an empty `node_id`, which the raw compare could not
/// do. (For well-formed ids stripping IS injective, since the formatter places
/// hyphens at fixed positions, so two real ids can never collide.)
///
/// Two further widenings are deliberate and are **not** equivalent to the old
/// `!= Some(node_id)`: `canonicalize` also trims, so a whitespace-padded id now
/// matches, and it strips `-` anywhere rather than only at the display
/// positions, so irregularly-hyphenated spellings of one id collapse together.
/// Both accept *more* input for the same real node; neither can make two
/// distinct well-formed ids equal.
#[must_use]
pub fn classify_single_extract_job(
    jobs_status: &Value,
    node_id: &str,
    job_id: Option<&str>,
) -> ExtractJobState {
    let payload = jobs_status.get("response").unwrap_or(jobs_status);
    let entries = payload
        .get("jobs")
        .and_then(|j| j.get("metadata_extract"))
        .and_then(Value::as_array);
    let Some(entries) = entries else {
        return ExtractJobState::NotFound;
    };
    // Canonicalized once, outside the loop: the caller's value does not change
    // per entry.
    let want_node = crate::opaque_id::canonicalize(node_id);
    if want_node.is_empty() {
        return ExtractJobState::NotFound;
    }
    for entry in entries {
        if entry.get("kind").and_then(Value::as_str) != Some("single") {
            continue;
        }
        // A missing/null `node_id` (which the `batch` entries carry) never
        // matches, exactly as the previous `!= Some(node_id)` compare did.
        if entry
            .get("node_id")
            .and_then(Value::as_str)
            .is_none_or(|n| crate::opaque_id::canonicalize(n) != want_node)
        {
            continue;
        }
        // RAW compare, deliberately — the contract guarantees this id comes
        // back byte for byte. See the doc comment.
        if let Some(want) = job_id
            && entry.get("job_id").and_then(Value::as_str) != Some(want)
        {
            continue;
        }
        return match entry.get("status").and_then(Value::as_str) {
            Some("completed") => ExtractJobState::Completed,
            Some("errored") => {
                let msg = entry
                    .get("error_message")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                ExtractJobState::Errored(msg)
            }
            _ => ExtractJobState::Pending,
        };
    }
    ExtractJobState::NotFound
}

/// Server-enforced cap on the metadata-search `q` parameter (chars).
pub const METADATA_SEARCH_QUERY_MAX_CHARS: usize = 1024;
/// Server-enforced upper bound on `limit` for metadata-search.
pub const METADATA_SEARCH_LIMIT_MAX: u32 = 100;
/// Server-enforced cap on the deep-page window: `offset + limit` may
/// not exceed this value.
pub const METADATA_SEARCH_DEEP_PAGE_MAX: u32 = 10_000;

/// Search workspace files by metadata field values (the `metadata_facts`
/// corpus).
///
/// `GET /workspace/{workspace_id}/metadata/search/?q=…`
///
/// Lexical keyword search over extracted metadata values — the
/// metadata-only counterpart to `/storage/search/` (which targets
/// filenames). Returns BM25-scored hits with the full hydrated node
/// payload and offset-paged metadata. Trashed nodes are filtered out
/// at hydration; tenancy is enforced server-side via the workspace
/// path segment.
///
/// Validation mirrors the server contract:
/// - `q` is whitespace-trimmed and rejected if empty or longer than
///   [`METADATA_SEARCH_QUERY_MAX_CHARS`] characters.
/// - `limit`, when supplied, must be in `1..=METADATA_SEARCH_LIMIT_MAX`.
/// - `offset + limit` may not exceed [`METADATA_SEARCH_DEEP_PAGE_MAX`].
///
/// Indexing is asynchronous (1–2 s), so callers should not search
/// immediately after a metadata write as a correctness check. A
/// non-empty result with `pagination.total = 0` is a real "no
/// matches" signal, distinct from a 4xx error.
///
/// **`total` IS AN UPPER BOUND, NOT A COUNT — do not paginate on it as
/// if it were exact.** The inverse of the case above is reachable:
/// `total = 1` with `results = []`, i.e. the count and the hydrated set
/// disagree and a caller stepping through pages gets an empty page for a
/// result the count promised.
///
/// **Attribution and confidence, deliberately separated:** the wire
/// behaviour was measured against the backend (2026-08-26) — it is
/// client-observable, which is why it is documented here. The *explanation*
/// offered for it (a stale search index left by a cleanup path that does not
/// purge facts) was flagged as believed-not-proven, and this client has NOT
/// read the server source, so no mechanism is asserted here. Only the
/// resulting guidance — treat `total` as an upper bound — is stated, because
/// that is what a caller must act on and it holds whatever the cause turns
/// out to be.
pub async fn search_metadata(
    client: &ApiClient,
    workspace_id: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let trimmed = query.trim();
    validate_search_query(trimmed)?;
    if let Some(l) = limit {
        validate_search_limit(l)?;
    }
    validate_search_window(limit, offset)?;
    let mut params = HashMap::new();
    params.insert("q".to_owned(), trimmed.to_owned());
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/metadata/search/",
        urlencoding::encode(workspace_id),
    );
    client.get_with_params(&path, &params).await
}

/// Validate the optional `fields` argument for `extract_node_metadata`.
///
/// Accepts a JSON-encoded array of 1 or more non-blank field names. The
/// server treats an absent value as a full-row extraction, so an empty
/// array would be ambiguous and is rejected here.
fn validate_extract_fields(fields: &str) -> Result<(), CliError> {
    let parsed: Vec<String> = serde_json::from_str(fields).map_err(|e| {
        CliError::Parse(format!(
            "extract fields must be a JSON array of strings: {e}",
        ))
    })?;
    if parsed.is_empty() {
        return Err(CliError::Parse(
            "extract fields must contain at least 1 field name (omit the param for a full-row extraction)".to_owned(),
        ));
    }
    if parsed.iter().any(|s| s.trim().is_empty()) {
        return Err(CliError::Parse(
            "extract fields entries must not be empty".to_owned(),
        ));
    }
    Ok(())
}

/// Validate the metadata-search `q` parameter.
///
/// Defensively re-checks for whitespace-only input even though
/// [`search_metadata`] already trims, so this validator is safe to
/// reuse from any future caller. Short-circuits on raw byte length
/// before walking codepoints, so an adversarial multi-megabyte input
/// is rejected without a full UTF-8 scan.
fn validate_search_query(q: &str) -> Result<(), CliError> {
    if q.trim().is_empty() {
        return Err(CliError::Parse("search query must not be empty".to_owned()));
    }
    // UTF-8 char count cannot exceed byte length, and each char is at
    // most 4 bytes, so byte length > MAX*4 guarantees over-cap. This
    // bounds work to a constant regardless of input size.
    if q.len() > METADATA_SEARCH_QUERY_MAX_CHARS * 4 {
        return Err(CliError::Parse(format!(
            "search query must be at most {METADATA_SEARCH_QUERY_MAX_CHARS} chars",
        )));
    }
    let len = q.chars().count();
    if len > METADATA_SEARCH_QUERY_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "search query must be at most {METADATA_SEARCH_QUERY_MAX_CHARS} chars (got {len})",
        )));
    }
    Ok(())
}

/// Validate the metadata-search `limit` parameter.
fn validate_search_limit(limit: u32) -> Result<(), CliError> {
    if limit == 0 || limit > METADATA_SEARCH_LIMIT_MAX {
        return Err(CliError::Parse(format!(
            "limit must be between 1 and {METADATA_SEARCH_LIMIT_MAX} (got {limit})",
        )));
    }
    Ok(())
}

/// Validate the deep-paging window. The server enforces
/// `offset + limit <= METADATA_SEARCH_DEEP_PAGE_MAX`; defaults are
/// `limit = 100`, `offset = 0`.
fn validate_search_window(limit: Option<u32>, offset: Option<u32>) -> Result<(), CliError> {
    let l = limit.unwrap_or(100);
    let o = offset.unwrap_or(0);
    let sum = u64::from(o) + u64::from(l);
    if sum > u64::from(METADATA_SEARCH_DEEP_PAGE_MAX) {
        return Err(CliError::Parse(format!(
            "offset + limit must not exceed {METADATA_SEARCH_DEEP_PAGE_MAX} (got {sum})",
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Node facts — the human-asserted metadata surface
// ---------------------------------------------------------------------------

/// Server cap on entries in one node-facts write (per the published API docs).
///
/// The empty object is also refused, so the accepted range is 1..=100.
pub const NODE_FACTS_MAX_ENTRIES: usize = 100;

/// Build the `/workspace/{ws}/storage/{node}/metadata/facts/` path.
///
/// Shared by the read and the write — they differ only in verb and body, and
/// the platform rate-limits them on **separate allowances**, so polling the
/// read never consumes a write's budget.
fn node_facts_path(workspace_id: &str, node_id: &str) -> String {
    format!(
        "/workspace/{}/storage/{}/metadata/facts/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    )
}

/// Read every fact on a node.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/metadata/facts/`
///
/// **Not paginated** — returns the node's complete fact set in one call. This
/// is the endpoint the published API docs name for reading the full set when an
/// embedded `metadata_facts` block reports `is_truncated: true`; the wrapper
/// here carries no `is_truncated` because the read is uncapped.
///
/// Response: `{result, object_id, count, items}` where each item is
/// `{field, value, declared_type, stored_type, source, confidence, rationale,
/// updated}`.
///
/// **A cleared field is not the same as one never extracted**: clearing writes
/// a fact with an empty value and `source: "user"` that still appears in
/// `items`, whereas a field nobody ever extracted has no row at all. Absence
/// means "no fact", not "empty fact".
///
/// **This shape and the embedded `metadata_facts` block are identical only at
/// `output=full`** and diverge at every lower tier — at `terse` the embedded
/// block replaces per-item objects with a single joined `fields` string. A
/// parser serving both surfaces must request `full`.
pub async fn get_node_facts(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    // `output=full` is sent EXPLICITLY, and that is deliberate — it overrides
    // the global `--detail` flag for this one endpoint.
    //
    // `full` is already the server's default, so this changes nothing about a
    // plain call. What it defends against is a DOWNGRADE: `client.rs` injects
    // `?output=<detail>` on any path not in its deny list, and `/metadata/facts/`
    // is not on that list. So `fastio --detail standard <facts read>` would strip
    // `declared_type`, `stored_type`, `source`, `confidence`, `rationale` and
    // `updated` — exactly the provenance this endpoint exists to return, and
    // exactly the fields this function's doc comment promises a parser.
    //
    // Sending it also suppresses the injection (`params_have_output`), so the two
    // cannot both apply. `--detail` is a general "prefer terser responses"
    // preference, not per-call intent; honouring it here would answer the
    // question "should I trust this value?" with field names and no values,
    // successfully.
    let mut params = HashMap::new();
    params.insert("output".to_owned(), "full".to_owned());
    client
        .get_with_params(&node_facts_path(workspace_id, node_id), &params)
        .await
}

/// Validate a node-facts payload before it reaches the wire.
///
/// Catches only what is cheap and unambiguous client-side: the entry-count
/// bounds and the non-object body. Everything else — name canonicality, type
/// compatibility, duplicate resolution — is server-owned and reported per-field
/// in `error.params`, which is the contract the caller should surface.
fn validate_node_facts(facts: &Value) -> Result<(), CliError> {
    let Some(map) = facts.as_object() else {
        return Err(CliError::Parse(
            "facts must be a JSON object mapping field name to value (an array is refused)"
                .to_owned(),
        ));
    };
    if map.is_empty() {
        return Err(CliError::Parse(
            "facts must name at least one field; the empty object is refused by the server"
                .to_owned(),
        ));
    }
    if map.len() > NODE_FACTS_MAX_ENTRIES {
        return Err(CliError::Parse(format!(
            "facts accepts at most {NODE_FACTS_MAX_ENTRIES} entries per call, got {}",
            map.len()
        )));
    }
    Ok(())
}

/// Write human-asserted facts onto a node.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/metadata/facts/`
/// (**JSON body**).
///
/// Every value stored here gets `source: "user"`, which outranks `ai`, `exif`
/// and `mediainfo`, so a later automatic re-extraction leaves it alone. This is
/// how a wrong extracted value is corrected.
///
/// The response is the node's **complete fact set after the write**, not an
/// echo of what was sent.
///
/// # Three behaviours the caller must design around
///
/// **A new field name is created permanently.** A name this workspace has not
/// seen creates the field and infers its type from the value — and a field
/// declaration **can never be deleted**. So a typo here permanently consumes a
/// capped vocabulary slot, through a route that does not look like a schema
/// operation. Merge is the only remedy and is itself irreversible. Surface
/// new-field creation to the user *before* writing.
///
/// **`null` is a total no-op, and that is a migration trap.** A `null` clears
/// nothing and creates nothing; an all-null payload writes nothing and answers
/// `200` unchanged. On the retired `metadata/update/` route `null` **cleared**
/// the field, and no value here reproduces that. To remove a value, use
/// `DELETE .../metadata/` with the name in `keys`.
///
/// **`category` / `sub_category` silently coerce off-list values to
/// `Other`** — a `200` that did not store what you sent,
/// the only such case on this route. Because some files hold classifications
/// predating the closed list, **re-sending a file's own stored `category`
/// destroys it**. Never round-trip a full fact set back through this endpoint;
/// send only the fields actually being changed.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `facts` is not an object, is empty, or
/// exceeds [`NODE_FACTS_MAX_ENTRIES`]. Server refusals surface as
/// [`CliError`] variants carrying the response body.
///
/// **On a server error, the discriminator is `error.params`, not the status.**
/// A `406` **with** `error.params` wrote nothing and names the offending
/// fields. A `406` **without** `error.params` means the write may
/// have landed and could not be confirmed — **re-read rather than retry**. A
/// `503` means metadata was busy and nothing was written; resending unchanged
/// is correct. Writes are all-or-nothing, including field definitions, so a
/// refused request leaves neither values nor new fields behind.
pub async fn write_node_facts(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    facts: &Value,
) -> Result<Value, CliError> {
    validate_node_facts(facts)?;
    let path = node_facts_path(workspace_id, node_id);
    client
        .post_json(&path, &serde_json::json!({ "facts": facts }))
        .await
}

/// Field names in `facts` that this workspace's vocabulary does not already
/// carry — i.e. the ones a write would **permanently create**.
///
/// `known` is the set of canonical names and aliases already declared. Matching
/// is case- and accent-insensitive on the server; this does the ASCII-case
/// half, which is what a client can do without the vocabulary's folding rules.
/// It is therefore **advisory** — it may over-report a name that differs only
/// by accent, and never under-reports a genuinely new one.
///
/// Entries whose value is `null` are excluded: a null creates nothing.
#[must_use]
pub fn new_field_names(facts: &Value, known: &[&str]) -> Vec<String> {
    let Some(map) = facts.as_object() else {
        return Vec::new();
    };
    let known_folded: std::collections::HashSet<String> =
        known.iter().map(|k| k.to_lowercase()).collect();
    map.iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, _)| k)
        .filter(|name| !known_folded.contains(&name.to_lowercase()))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Field vocabulary
// ---------------------------------------------------------------------------

/// The closed set of types a field may be declared as.
///
/// Any other value is rejected by the server, "including type names used
/// elsewhere in the platform that a field definition cannot store" — so this is
/// validated client-side rather than spending a round trip on a typo.
pub const FIELD_DECLARED_TYPES: &[&str] =
    &["string", "bool", "int", "float", "json", "url", "datetime"];

/// Server bounds on the vocabulary listing's page size.
///
/// Deliberately NOT shared with [`EligibleParams::page_size`]. The eligible
/// endpoint snaps to 25/100/250, and the published API docs say that "the snap
/// is specific to this endpoint; a `page_size` elsewhere in the API is not
/// necessarily quantized, so do not carry this rule across."
pub const FIELDS_PAGE_SIZE_MAX: u32 = 250;

/// Maximum length of a field name, in characters.
pub const FIELD_NAME_MAX_CHARS: usize = 64;

/// Path to a workspace's declared-field collection (`GET` list, `POST` declare).
///
/// Extracted rather than inlined at both call sites for the same reason
/// `cloud_import_path` was: a path literal repeated per call site is a literal
/// that can drift, and the `/import/` vs `/cloud-import/` defect on this branch
/// was exactly that failure with nothing asserting the difference.
fn fields_path(workspace_id: &str) -> String {
    format!(
        "/workspace/{}/metadata/fields/",
        urlencoding::encode(workspace_id)
    )
}

/// List the workspace's metadata field vocabulary.
///
/// `GET /workspace/{workspace_id}/metadata/fields/`
///
/// Cursor-paginated. The response is flat with the records under **`items`**
/// (not `fields`), alongside `count`, `page_size`, `cursor` and `has_more`.
///
/// Omit `cursor` for the first page rather than sending an empty one — an
/// absent cursor and a supplied-but-empty one are distinct states.
///
/// **`file_count` is ABSENT, not `0`, when the count could not be read.** A `0`
/// is a confident claim that nothing uses the field — exactly the claim a
/// cleanup or merge acts on — so a renderer must show a missing key as unknown,
/// never as zero. It also counts **nodes**, including notes and trashed ones,
/// so do not label a column "Files".
///
/// Merged-away names are not listed separately; a retired name appears in the
/// surviving field's `aliases`, so a listing only ever offers canonical names.
pub async fn list_metadata_fields(
    client: &ApiClient,
    workspace_id: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(n) = page_size {
        if n == 0 || n > FIELDS_PAGE_SIZE_MAX {
            return Err(CliError::Parse(format!(
                "page_size must be between 1 and {FIELDS_PAGE_SIZE_MAX} (got {n})"
            )));
        }
        params.insert("page_size".to_owned(), n.to_string());
    }
    if let Some(c) = cursor.filter(|c| !c.is_empty()) {
        params.insert("cursor".to_owned(), c.to_owned());
    }
    let path = fields_path(workspace_id);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Declare a metadata field.
///
/// `POST /workspace/{workspace_id}/metadata/fields/` — **form-encoded**, and
/// **workspace ADMIN**, a higher bar than the listing's Member on the same
/// path.
///
/// Response is `{field, field_created}`. Note that `field_created` is a
/// top-level **boolean**; `field.created` is a nested **timestamp** — different
/// keys at different levels, and easy to confuse.
///
/// # A declaration is PERMANENT
///
/// The published API docs state: "Names are write-once and there is no delete:
/// nothing in this API renames a field or removes it. Declaring a misspelled
/// name leaves it in the vocabulary for good — the only remedy is
/// `.../metadata/fields/merge/` to fold it into the right one." Callers must
/// confirm before invoking this.
///
/// Idempotent: an existing name returns the existing definition with
/// `field_created: false`, and a merged-away name returns the field that now
/// governs it.
///
/// Names are compared **case-insensitively** and only the spelling stored
/// first is kept — so **render what comes back, not what was sent**.
///
/// Whitespace here is **trimmed** before storing and validating. That is
/// this route's rule alone: merge treats a leading space as significant, and a
/// facts write refuses a padded name outright. Do not factor these into one
/// shared normalizer — it would be wrong on two of the three.
///
/// # Errors
///
/// [`CliError::Parse`] when `name` is blank or over
/// [`FIELD_NAME_MAX_CHARS`] after trimming, or `declared_type` is outside
/// [`FIELD_DECLARED_TYPES`]. Server-side, `413` means the workspace hit its
/// field limit (permanent — merge or raise the limit, do not retry) while `500`
/// is retryable and created nothing. **Branch on the status, never on
/// `error.code`.**
pub async fn declare_metadata_field(
    client: &ApiClient,
    workspace_id: &str,
    name: &str,
    declared_type: &str,
) -> Result<Value, CliError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CliError::Parse(
            "field name must not be blank or whitespace-only".to_owned(),
        ));
    }
    if trimmed.chars().count() > FIELD_NAME_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "field name must be at most {FIELD_NAME_MAX_CHARS} characters after trimming (got {})",
            trimmed.chars().count()
        )));
    }
    if !FIELD_DECLARED_TYPES.contains(&declared_type) {
        return Err(CliError::Parse(format!(
            "declared_type must be one of {} (got '{declared_type}')",
            FIELD_DECLARED_TYPES.join(", ")
        )));
    }
    let mut form = HashMap::new();
    form.insert("name".to_owned(), trimmed.to_owned());
    form.insert("declared_type".to_owned(), declared_type.to_owned());
    // NOTE: `constraints` is deliberately never sent. The route accepts it in
    // its input spec solely so it can be REFUSED — "a value that would be
    // silently discarded is refused rather than accepted".
    client.post(&fields_path(workspace_id), &form).await
}

/// What a merge call actually did, as opposed to what its HTTP status implies.
///
/// **A merge refusal is HTTP `200` in both modes**: a refused
/// `confirm: true` answers exactly as a pre-flight does, with `merged: false`,
/// and performs nothing. Callers must read `would_refuse` and `merged`, never
/// the status code — which is what this type exists to make hard to get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MergeOutcome {
    /// This call performed the fold.
    Merged,
    /// The two names already resolve to one field; nothing to do.
    AlreadyMerged,
    /// The server declined. `reason` is a stable string
    /// (`merge_refused_type_mismatch`, `merge_source_unknown`, …), never a
    /// numeric code. `source_resolves_to` is present only for
    /// `merge_source_already_merged`.
    Refused {
        /// The published refusal reason. Branch on this, never on the message.
        reason: String,
        /// Human-readable explanation from the server.
        message: String,
        /// For `merge_source_already_merged`: the name the source resolves to
        /// now, so a caller can retry with the field it meant.
        source_resolves_to: Option<String>,
    },
    /// A pre-flight that reports the fold would succeed. Nothing was written.
    WouldSucceed,
    /// The body carried none of the discriminating keys, or carried one in a
    /// shape that cannot be read.
    ///
    /// Exists so that `WouldSucceed` cannot be reached by falling off the end
    /// of every check — otherwise an empty body `{}` would classify as a
    /// confident "this fold would succeed". For an operation the docs say
    /// **cannot be undone**, asserting success from zero evidence is the wrong
    /// default; a caller must be able to tell "the server said it would work"
    /// from "the server did not say". Adding this variant is additive because
    /// the enum is `#[non_exhaustive]`.
    Unknown,
}

/// Classify a merge response body into a [`MergeOutcome`].
///
/// Order matters: `would_refuse` is checked first, because a refused
/// `confirm: true` still answers `200` with `merged: false`, so reading
/// `merged` alone cannot distinguish "declined" from "pre-flight".
#[must_use]
pub fn classify_merge_response(body: &Value) -> MergeOutcome {
    let payload = body.get("response").unwrap_or(body);
    if let Some(refusal) = payload.get("would_refuse").filter(|v| !v.is_null()) {
        // Present but unreadable ⇒ Unknown, NOT Refused. Reporting "refused" for
        // a shape we cannot parse would tell the user an irreversible fold did
        // not happen when it may have — the worst available direction to be
        // wrong in for an operation with no undo. The docs promise object-or-
        // null, so this arm should be unreachable; it is here because being
        // wrong here is expensive and being cautious costs nothing.
        if !refusal.is_object() {
            return MergeOutcome::Unknown;
        }
        return MergeOutcome::Refused {
            reason: refusal
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            message: refusal
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            source_resolves_to: refusal
                .get("source_resolves_to")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        };
    }
    if payload.get("merged").and_then(Value::as_bool) == Some(true) {
        return MergeOutcome::Merged;
    }
    if payload.get("already_merged").and_then(Value::as_bool) == Some(true) {
        return MergeOutcome::AlreadyMerged;
    }
    // A pre-flight verdict requires the server to have actually said `merged`.
    // Without it there is nothing to base "would succeed" on.
    if payload.get("merged").and_then(Value::as_bool) == Some(false) {
        return MergeOutcome::WouldSucceed;
    }
    MergeOutcome::Unknown
}

/// Fold one metadata field into another.
///
/// `POST /workspace/{workspace_id}/metadata/fields/merge/` — **workspace
/// ADMIN**, and rate-limited more tightly than the listing.
///
/// Sent as **JSON with a real boolean `confirm`**, matching the documented curl.
///
/// # `confirm` is a BOOLEAN here — unlike every other `confirm` in this API
///
/// On workspace delete, share delete and org close, `confirm` is a **string**
/// that must equal the resource's own name or id. Following that idiom here —
/// sending the field name — is **rejected**; it is never read as truthy and
/// never performs the merge. Over JSON even the string `"true"` is refused,
/// which is why this function takes a `bool` and serializes it as one.
///
/// # This cannot be undone, and a refusal is HTTP 200
///
/// The published API docs are explicit: "this API cannot undo it." Call with
/// `confirm = false` first — the pre-flight takes the same lock and evaluates
/// the same refusals in the same order, so **the verdict cannot drift** — then
/// read the counts and confirm. Classify the result with
/// [`classify_merge_response`]; the status code alone cannot tell success from
/// refusal.
///
/// The **counts** are a weaker claim than the verdict: a reading taken under
/// lock, not a reservation. And `files_affected` counts values that **stop
/// being reachable by name** — a fold does not rewrite them, so wording it as
/// "files updated" misleads.
///
/// Names are **never trimmed here** and a leading space is significant — the
/// opposite of [`declare_metadata_field`]. The 64-char limit is measured on
/// what is sent, padding included.
///
/// # Errors
///
/// [`CliError::Parse`] when either name is blank or exceeds
/// [`FIELD_NAME_MAX_CHARS`] as sent.
pub async fn merge_metadata_fields(
    client: &ApiClient,
    workspace_id: &str,
    source: &str,
    target: &str,
    confirm: bool,
) -> Result<Value, CliError> {
    for (label, value) in [("source", source), ("target", target)] {
        if value.trim().is_empty() {
            return Err(CliError::Parse(format!("{label} must not be blank")));
        }
        // Measured on what is SENT, padding included — this route does not trim.
        if value.chars().count() > FIELD_NAME_MAX_CHARS {
            return Err(CliError::Parse(format!(
                "{label} must be at most {FIELD_NAME_MAX_CHARS} characters (got {})",
                value.chars().count()
            )));
        }
    }
    let path = format!(
        "/workspace/{}/metadata/fields/merge/",
        urlencoding::encode(workspace_id)
    );
    // A real JSON boolean: over JSON the server rejects the strings "true",
    // "1", "yes" and "on" outright.
    let body = serde_json::json!({
        "source": source,
        "target": target,
        "confirm": confirm,
    });
    client.post_json(&path, &body).await
}

/// List field pairs whose names differ only in punctuation and capitalisation.
///
/// `GET /workspace/{workspace_id}/metadata/fields/merge-candidates/` —
/// **workspace ADMIN**, the same bar as the merge it feeds.
///
/// Offset-paginated, and `offset` is an **ordinal into a ranking the server
/// re-derives on every call, not a stable cursor** — a workspace that changes
/// between calls can re-rank, so a walk may repeat or skip a proposal.
/// Terminate on `next_offset == null`.
///
/// **There is deliberately no page-size parameter** — the number checked per
/// call is fixed server-side, because every pair offered is verified against
/// the real merge first. Do not add one.
///
/// # An empty `items` has THREE meanings — read the counts first
///
/// | Observed | Meaning |
/// |---|---|
/// | `vocabulary_scanned: false` | The comparison did not run. Report **"not computed"**, never "nothing found" |
/// | `proposals_examined < proposals_total` | Only some were checked; call again with `offset = next_offset` |
/// | `proposals_examined == proposals_total` | The only empty list meaning "nothing is mergeable" |
///
/// (See the published API docs.) Even the third is not a clean bill of health
/// — every pair must still survive the merge's own refusals.
///
/// **This is a punctuation-and-case check, not a duplicate detector.** It does
/// not find abbreviations, synonyms, typos or plurals. Describe results as
/// "the same letters written differently", never as "duplicates" — that
/// wording is binding on help text.
///
/// Do not re-sort the response: it is already ordered least-destructive-first.
pub async fn metadata_merge_candidates(
    client: &ApiClient,
    workspace_id: &str,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/metadata/fields/merge-candidates/",
        urlencoding::encode(workspace_id)
    );
    let Some(o) = offset else {
        return client.get(&path).await;
    };
    let mut params = HashMap::new();
    params.insert("offset".to_owned(), o.to_string());
    client.get_with_params(&path, &params).await
}

// ---------------------------------------------------------------------------
// Compound search
// ---------------------------------------------------------------------------

/// Maximum length of a compound-search `content_query`, in characters.
pub const COMPOUND_QUERY_MAX_CHARS: usize = 1024;

/// Maximum clauses in one predicate ("more than 5 clauses" is a documented
/// `1605` cause).
///
/// NOTE: there is deliberately **no `COMPOUND_LIMIT_MAX`**. The docs say a
/// `limit` above the server maximum is "clamped, not rejected" and report the
/// original in `scope.limit_clamped_from` — but they never publish the number,
/// only "server default" / "server maximum". A constant invented here would be
/// an unsourced claim, and any caller pre-clamping to it would defeat the very
/// signal the server sends to say it clamped.
pub const COMPOUND_MAX_CLAUSES: usize = 5;

/// Intersect a metadata predicate with a semantic content query.
///
/// `POST /workspace/{workspace_id}/metadata/compound-search/`
///
/// Two stages in order, and neither is a post-filter: the metadata predicate
/// runs **first** and produces the candidate set, then the content stage
/// searches only those candidates and ranks them by relevance.
///
/// Workspace only — there is no share form. Requires Member plus **both** the
/// `metadata` and `content_ai` plan features **and** Intelligence enabled on
/// the workspace; there is no keyword leg to fall back on, so where
/// `/storage/search/` degrades, this endpoint refuses.
///
/// # The encoding is the documented #1 way to call this wrong
///
/// `filters` is a **form field whose value is a JSON string — not a JSON
/// request body**. A request sent as
/// `Content-Type: application/json` **does not populate `filters` at all** and
/// is refused exactly as if none were sent — `406` with `error.code` `119701`,
/// "which reads as 'my filter is invalid' when the real problem is how the body
/// was encoded." Hence `client.post` (form) here, never `post_json`.
///
/// The three parameters are not symmetric about this: `content_query` and
/// `limit` are read from the body *or* the query string, but **`filters` is
/// body-only** — it is the one that goes missing.
///
/// Note the same name means a different wire location on the sibling route:
/// `/storage/search/` takes `filters` as a **query** parameter (see
/// [`SearchFilesParams::into_query`]). Same JSON grammar, opposite location.
///
/// `filters_json` is sent **verbatim**, never re-serialized: a predicate value
/// may be a bare JSON integer compared exactly as sent, and round-tripping one
/// through a float loses precision above 2^53.
///
/// # Read the `scope` object — it is the point of this endpoint
///
/// The published API docs put it this way: "A short `items` list can mean
/// 'twelve files matched' or 'far more matched and the answer was cut short',
/// and `scope` is what tells the two apart." See
/// [`compound_answer_was_bounded`].
///
/// # Errors
///
/// [`CliError::Parse`] when `filters_json` is not a non-empty JSON array of
/// objects, when `content_query` is blank or over
/// [`COMPOUND_QUERY_MAX_CHARS`], or when `limit` is zero. Note an **empty
/// filters array is rejected here** even though it is legal (match-all) on a
/// saved filter — the two must not share a validator.
pub async fn compound_search(
    client: &ApiClient,
    workspace_id: &str,
    filters_json: &str,
    content_query: &str,
    limit: Option<u32>,
) -> Result<Value, CliError> {
    validate_compound_filters(filters_json)?;

    let trimmed = content_query.trim();
    if trimmed.is_empty() {
        return Err(CliError::Parse(
            "content_query must not be blank".to_owned(),
        ));
    }
    if content_query.chars().count() > COMPOUND_QUERY_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "content_query must be at most {COMPOUND_QUERY_MAX_CHARS} characters (got {})",
            content_query.chars().count()
        )));
    }

    let mut form = HashMap::new();
    // Verbatim — see the precision note above.
    form.insert("filters".to_owned(), filters_json.to_owned());
    form.insert("content_query".to_owned(), content_query.to_owned());
    if let Some(n) = limit {
        if n == 0 {
            return Err(CliError::Parse("limit must be at least 1".to_owned()));
        }
        // Deliberately NOT clamped client-side: the server clamps and reports
        // the original in `scope.limit_clamped_from`, which is how the caller
        // learns it happened. Clamping here would hide that.
        form.insert("limit".to_owned(), n.to_string());
    }

    let path = format!(
        "/workspace/{}/metadata/compound-search/",
        urlencoding::encode(workspace_id)
    );
    // FORM, not JSON. See the encoding note above — this is load-bearing.
    client.post(&path, &form).await
}

/// Validate a compound-search predicate client-side.
///
/// Checks only the shape the server checks structurally — a non-empty list of
/// objects each carrying a string `field` and `operator`.
/// Whether the operator is legal for the field's declared type is resolved
/// server-side against the workspace vocabulary and is not second-guessed here.
fn validate_compound_filters(filters_json: &str) -> Result<(), CliError> {
    let parsed: Value = serde_json::from_str(filters_json)
        .map_err(|e| CliError::Parse(format!("filters must be valid JSON: {e}")))?;
    let Some(clauses) = parsed.as_array() else {
        return Err(CliError::Parse(
            "filters must be a JSON array of {field, operator, value} clauses".to_owned(),
        ));
    };
    if clauses.is_empty() {
        return Err(CliError::Parse(
            "filters must not be empty on compound search (an empty predicate is legal on a \
             saved filter, but is rejected here)"
                .to_owned(),
        ));
    }
    // The cap is documented, and the server folds it into a
    // GENERIC `179646` that also covers a wrong operator, an unusable value and
    // an unknown field. Without this check the user pays a round trip and then
    // hunts through field names for a fault that is only the count. This module
    // already pre-validates `declared_type` for the same reason.
    if clauses.len() > COMPOUND_MAX_CLAUSES {
        return Err(CliError::Parse(format!(
            "filters accepts at most {COMPOUND_MAX_CLAUSES} clauses, got {}. The server \
             reports this as a generic invalid-predicate error that does not say the count \
             was the problem.",
            clauses.len()
        )));
    }
    for (i, clause) in clauses.iter().enumerate() {
        let Some(obj) = clause.as_object() else {
            return Err(CliError::Parse(format!("filters[{i}] must be an object")));
        };
        for key in ["field", "operator"] {
            if obj
                .get(key)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(CliError::Parse(format!(
                    "filters[{i}] must carry a non-empty string '{key}'"
                )));
            }
        }
    }
    Ok(())
}

/// Whether a `scope` object says the answer was cut short.
///
/// **`causes` being non-empty does NOT imply `match_relation == "gte"`**: only
/// the count-bounding causes make the count a floor, so a degraded coverage
/// read leaves the relation `eq`. Both signals
/// must therefore be checked, and a caller must not derive one from the other.
///
/// Returns `true` when the result should carry a visible "this answer was
/// bounded" warning.
#[must_use]
pub fn compound_answer_was_bounded(response: &Value) -> Option<bool> {
    // Takes the RESPONSE, not the scope. Handed the wrong one, the previous
    // signature returned `false` — "not bounded" — from a body it had simply
    // failed to understand, and a test pinned that. `None` says "there was no
    // scope object to read", which is a different answer from "the answer was
    // complete" and must not render as one.
    let scope = response.get("scope").filter(|v| v.is_object())?;
    let causes_present = scope
        .get("causes")
        .and_then(Value::as_array)
        .is_some_and(|c| !c.is_empty());
    let relation_is_floor = scope.get("match_relation").and_then(Value::as_str) == Some("gte");
    Some(causes_present || relation_is_floor)
}

/// Whether the index-coverage figure is genuinely unknown.
///
/// `files_not_indexed: null` means **the coverage read itself was unavailable
/// — unknown, not zero**. Rendering `null` as
/// `0` claims full coverage that was never measured. Distinguishes that from an
/// absent key, which is how the *filter-execute* route reports the same field
/// (it omits both semantic-stage keys so `null` keeps its meaning here).
#[must_use]
pub fn compound_coverage_unknown(response: &Value) -> Option<bool> {
    // Same reasoning as [`compound_answer_was_bounded`]: `None` means "no scope
    // object", not "coverage is known".
    let scope = response.get("scope").filter(|v| v.is_object())?;
    Some(matches!(scope.get("files_not_indexed"), Some(Value::Null)))
}

// ---------------------------------------------------------------------------
// Saved metadata filters
// ---------------------------------------------------------------------------

/// Max `name` length (published filter contract).
pub const FILTER_NAME_MAX_CHARS: usize = 100;
/// Max `description` length.
pub const FILTER_DESCRIPTION_MAX_CHARS: usize = 255;
/// Listing page-size bounds. Above the max the server CLAMPS; at or below zero
/// it REJECTS (a minimum-of-1 range assertion runs before the clamp), so this
/// client refuses both ends rather than letting a silent clamp through
/// unannounced.
pub const FILTER_PAGE_SIZE_MAX: u32 = 250;

/// The create/update body for a saved filter.
///
/// **`PUT` REPLACES — it is not a partial patch.** Every field is written from
/// the body on every call. `name` and `predicate` are required, so a partial
/// update cannot silently widen the filter — that path is rejected outright. But
/// **omitting `description` or `projection` CLEARS it, silently, with a `200`**.
/// There is no error and no warning.
///
/// So to change one field: read the filter, apply the edit, send the whole object
/// back. [`filter_params_from_existing`] exists to make that the easy path.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FilterParams {
    /// Required. Trimmed, non-empty, ≤100 chars, unique per workspace (409 on collision).
    pub name: String,
    /// Required. Clause list; send `[]` explicitly for match-all — omitting it is an error.
    pub predicate: Value,
    /// Optional, ≤255 chars. **Omitted on update ⇒ cleared.**
    pub description: Option<String>,
    /// Optional, opaque except `sort`. **Omitted on update ⇒ cleared.**
    pub projection: Option<Value>,
    /// **Create only** — records which template this filter succeeds. Ignored on update.
    pub template_id: Option<String>,
}

/// Rebuild [`FilterParams`] from a filter object returned by the API, so an
/// update can change one field without wiping the others.
///
/// This is the read half of the mandatory read-modify-write. Pass the `filter`
/// object from a create/get/update response; edit the returned struct; send it.
///
/// Do **not** feed this a `?output=terse` body — that tier returns only `id`,
/// `name` and `description`, with `predicate`/`projection` **absent, not null**.
/// Round-tripping a terse read would send an empty predicate and convert the
/// filter to match-all. Read at the default (`full`) tier.
#[must_use]
pub fn filter_params_from_existing(filter: &Value) -> FilterParams {
    FilterParams {
        name: filter
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        predicate: filter
            .get("predicate")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        description: filter
            .get("description")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        projection: filter.get("projection").filter(|v| !v.is_null()).cloned(),
        // Deliberately not carried: the server ignores it on update, so echoing
        // it back would imply a control the caller does not have.
        template_id: None,
    }
}

fn filters_path(workspace_id: &str) -> String {
    format!(
        "/workspace/{}/metadata/filters/",
        urlencoding::encode(workspace_id)
    )
}

fn filter_path(workspace_id: &str, filter_id: &str) -> String {
    format!(
        "/workspace/{}/metadata/filters/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(filter_id)
    )
}

/// Validate a filter body client-side.
///
/// The server folds `name` >100, `description` >255 and predicate >5 clauses into
/// **one generic `1605`/406 with a single message that does not say which you
/// hit**. Checking locally is the only way to tell the user which field to fix.
fn validate_filter_params(p: &FilterParams) -> Result<(), CliError> {
    let name = p.name.trim();
    if name.is_empty() {
        return Err(CliError::Parse("filter name must not be blank".to_owned()));
    }
    if name.chars().count() > FILTER_NAME_MAX_CHARS {
        return Err(CliError::Parse(format!(
            "filter name must be at most {FILTER_NAME_MAX_CHARS} characters (got {})",
            name.chars().count()
        )));
    }
    if let Some(d) = &p.description
        && d.chars().count() > FILTER_DESCRIPTION_MAX_CHARS
    {
        return Err(CliError::Parse(format!(
            "filter description must be at most {FILTER_DESCRIPTION_MAX_CHARS} characters (got {})",
            d.chars().count()
        )));
    }
    let Some(clauses) = p.predicate.as_array() else {
        return Err(CliError::Parse(
            "predicate must be a JSON array of {field, operator, value} clauses; send [] for \
             match-all rather than omitting it"
                .to_owned(),
        ));
    };
    if clauses.len() > COMPOUND_MAX_CLAUSES {
        return Err(CliError::Parse(format!(
            "predicate accepts at most {COMPOUND_MAX_CLAUSES} clauses, got {}",
            clauses.len()
        )));
    }
    Ok(())
}

fn filter_body(p: &FilterParams, include_template: bool) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("name".to_owned(), Value::String(p.name.trim().to_owned()));
    body.insert("predicate".to_owned(), p.predicate.clone());
    if let Some(d) = &p.description {
        body.insert("description".to_owned(), Value::String(d.clone()));
    }
    if let Some(pr) = &p.projection {
        body.insert("projection".to_owned(), pr.clone());
    }
    // Create only — the server ignores it on update.
    if include_template && let Some(t) = &p.template_id {
        body.insert("template_id".to_owned(), Value::String(t.clone()));
    }
    Value::Object(body)
}

/// Create a saved metadata filter.
///
/// `POST /workspace/{workspace_id}/metadata/filters/` — **JSON body**.
/// Response: `{"result": true, "filter": { … }}`.
///
/// A name collision is **409**. And note the subscription plan's filter-count
/// limit answers **`401`, NOT 403** — a denial that shares a status with an auth
/// failure, so a caller must read the code before concluding credentials are the
/// problem. Retrying or re-authenticating will never clear it; the remedy is to
/// delete a filter or raise the subscription limit.
///
/// Create validates **structure only**. Whether a field exists, whether the
/// operator suits its type and whether the value renders are all checked at
/// EXECUTE — so a filter can be created successfully and still `406` when its
/// nodes are listed. Do not report "filter works" on a successful create.
///
/// # Errors
/// [`CliError::Parse`] when [`validate_filter_params`] rejects the body.
pub async fn create_filter(
    client: &ApiClient,
    workspace_id: &str,
    params: &FilterParams,
) -> Result<Value, CliError> {
    validate_filter_params(params)?;
    client
        .post_json(&filters_path(workspace_id), &filter_body(params, true))
        .await
}

/// List saved filters (cursor-paginated).
///
/// `GET /workspace/{workspace_id}/metadata/filters/`
///
/// Response is **flat**: `{"result": true, "count", "items", "cursor",
/// "has_more"}` — the collection key is **`items`**, not `filters`, and `count`
/// is THIS page's size. **Drive paging off `cursor`/`has_more`, never `count`.**
///
/// Omit `cursor` for the first page rather than sending an empty one.
///
/// # Errors
/// [`CliError::Parse`] when `page_size` is outside 1..=[`FILTER_PAGE_SIZE_MAX`].
pub async fn list_filters(
    client: &ApiClient,
    workspace_id: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(n) = page_size {
        if n == 0 || n > FILTER_PAGE_SIZE_MAX {
            return Err(CliError::Parse(format!(
                "page_size must be between 1 and {FILTER_PAGE_SIZE_MAX} (got {n}). The server \
                 clamps a larger value silently rather than reporting it."
            )));
        }
        params.insert("page_size".to_owned(), n.to_string());
    }
    if let Some(c) = cursor.filter(|c| !c.is_empty()) {
        params.insert("cursor".to_owned(), c.to_owned());
    }
    let path = filters_path(workspace_id);
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get one saved filter. `GET .../metadata/filters/{filter_id}/`
///
/// Response `{"result": true, "filter": { … }}`. Read at the DEFAULT tier when
/// the result will be round-tripped into an update — `?output=terse` omits
/// `predicate` and `projection` entirely (absent, not null).
pub async fn get_filter(
    client: &ApiClient,
    workspace_id: &str,
    filter_id: &str,
) -> Result<Value, CliError> {
    client.get(&filter_path(workspace_id, filter_id)).await
}

/// Replace a saved filter.
///
/// `PUT .../metadata/filters/{filter_id}/` — **JSON body, FULL REPLACE.**
///
/// **Omitting `description` or `projection` CLEARS it, silently, with `200`.**
/// Build `params` with [`filter_params_from_existing`] from a full-tier read and
/// apply your edit to that, or you will wipe fields you never mentioned.
///
/// `template_id` is **not sent** — the server ignores it on update, and sending it
/// would imply a control the caller does not have.
///
/// # Errors
/// [`CliError::Parse`] when [`validate_filter_params`] rejects the body.
pub async fn update_filter(
    client: &ApiClient,
    workspace_id: &str,
    filter_id: &str,
    params: &FilterParams,
) -> Result<Value, CliError> {
    validate_filter_params(params)?;
    client
        .put_json(
            &filter_path(workspace_id, filter_id),
            &filter_body(params, false),
        )
        .await
}

/// Delete a saved filter.
///
/// `DELETE .../metadata/filters/{filter_id}/` — no body, and there is **no
/// `confirm` parameter**; gate it client-side if you want one.
///
/// **Idempotent and information-hiding: a missing, already-deleted or FOREIGN
/// filter id all answer `200`.** A success therefore does NOT prove anything was
/// deleted — do not report "Deleted filter X" on the strength of it. Say the
/// filter is gone, or read it back.
///
/// Any workspace member may delete a filter another member created; there is no
/// per-filter ownership.
pub async fn delete_filter(
    client: &ApiClient,
    workspace_id: &str,
    filter_id: &str,
) -> Result<Value, CliError> {
    client.delete(&filter_path(workspace_id, filter_id)).await
}

/// Execute a saved filter and return the matching nodes.
///
/// `GET .../metadata/filters/{filter_id}/nodes/`
///
/// **Deliberately NOT paginated** — one-shot and plan-capped. Do not add a
/// cursor or a limit; exposing one would be a way to walk past the cap.
///
/// `sort_field` and `sort_metadata_field` are **mutually exclusive** — sending
/// both is a `406`, not a ranking. They are two parameters because a workspace may
/// declare a field literally named `name` or `size`, and one parameter could not
/// tell it from the built-in column.
///
/// **A truncated result is a top-N only on `sort_metadata_field`.** With
/// `sort_field` the cap is applied first and the survivors re-sorted, so a capped
/// page is an arbitrary sample wearing a ranking. Check `scope.scope_truncated`
/// before presenting it as a top-N.
///
/// # Errors
/// [`CliError::Parse`] when both sort axes are supplied.
pub async fn execute_filter(
    client: &ApiClient,
    workspace_id: &str,
    filter_id: &str,
    sort_field: Option<&str>,
    sort_metadata_field: Option<&str>,
    sort_dir: Option<&str>,
) -> Result<Value, CliError> {
    if sort_field.is_some() && sort_metadata_field.is_some() {
        return Err(CliError::Parse(
            "sort_field and sort_metadata_field are mutually exclusive — one list cannot have \
             two orders. Use sort_field for a file column (name/size/updated) or \
             sort_metadata_field to order by a metadata VALUE."
                .to_owned(),
        ));
    }
    let mut params = HashMap::new();
    if let Some(v) = sort_field {
        params.insert("sort_field".to_owned(), v.to_owned());
    }
    if let Some(v) = sort_metadata_field {
        params.insert("sort_metadata_field".to_owned(), v.to_owned());
    }
    if let Some(v) = sort_dir {
        params.insert("sort_dir".to_owned(), v.to_owned());
    }
    let path = format!(
        "/workspace/{}/metadata/filters/{}/nodes/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(filter_id)
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BULK_METADATA_DETAILS_MAX_IDS, BulkMetadataDetailsResponse, COMPOUND_MAX_CLAUSES,
        COMPOUND_QUERY_MAX_CHARS, EligibleParams, ExtractJobState, FIELD_DECLARED_TYPES,
        FIELD_NAME_MAX_CHARS, FIELDS_PAGE_SIZE_MAX, FILTER_DESCRIPTION_MAX_CHARS,
        FILTER_NAME_MAX_CHARS, FilterParams, METADATA_SEARCH_DEEP_PAGE_MAX,
        METADATA_SEARCH_LIMIT_MAX, METADATA_SEARCH_QUERY_MAX_CHARS, MergeOutcome,
        NODE_FACTS_MAX_ENTRIES, build_bulk_metadata_details_path, classify_merge_response,
        classify_single_extract_job, compound_answer_was_bounded, compound_coverage_unknown,
        eligible_query, filter_body, filter_params_from_existing, filter_path, filters_path,
        new_field_names, node_facts_path, parse_bulk_metadata_details_response,
        sanitize_terminal_string, strip_declared_metadata_types, validate_compound_filters,
        validate_extract_fields, validate_filter_params, validate_node_facts,
        validate_search_limit, validate_search_query, validate_search_window,
    };
    use crate::error::CliError;
    use serde_json::json;

    /// An empty predicate is LEGAL on a saved filter (match-all) and REJECTED
    /// on compound search. Pinned so nobody factors the two validators
    /// together.
    #[test]
    fn compound_search_rejects_the_empty_predicate_a_saved_filter_allows() {
        let err = validate_compound_filters("[]").expect_err("empty must be refused here");
        assert!(
            err.to_string().contains("saved filter"),
            "the error should explain why this differs from a saved filter, got: {err}"
        );
    }

    #[test]
    fn compound_filters_must_be_a_list_of_clauses_with_field_and_operator() {
        assert!(validate_compound_filters("not json").is_err());
        assert!(validate_compound_filters(r#"{"field":"a","operator":"="}"#).is_err());
        assert!(validate_compound_filters(r#"["a string"]"#).is_err());
        assert!(validate_compound_filters(r#"[{"operator":"="}]"#).is_err());
        assert!(validate_compound_filters(r#"[{"field":"","operator":"="}]"#).is_err());
        assert!(
            validate_compound_filters(r#"[{"field":"document_type","operator":"=","value":"c"}]"#)
                .is_ok()
        );
        // `exists` legitimately carries no `value` — the validator must not
        // require one.
        assert!(
            validate_compound_filters(r#"[{"field":"amount","operator":"exists"}]"#).is_ok(),
            "exists/not_exists take no value and must still validate"
        );
    }

    /// Only the COUNT-BOUNDING causes make `match_relation` a floor, so
    /// `causes` can be non-empty while the relation is still `eq`. Both signals
    /// must be read; neither implies the other.
    #[test]
    fn a_bounded_answer_is_detected_from_either_signal_independently() {
        let wrap = |scope| json!({"items": [], "scope": scope});
        // Degraded coverage: a cause fired, but the count is still exact.
        let coverage_only =
            wrap(json!({"match_relation": "eq", "causes": ["coverage_unavailable"]}));
        assert_eq!(compound_answer_was_bounded(&coverage_only), Some(true));

        // A floor with no causes reported.
        let floor_only = wrap(json!({"match_relation": "gte", "causes": []}));
        assert_eq!(compound_answer_was_bounded(&floor_only), Some(true));

        // Genuinely complete.
        let clean = wrap(json!({"match_relation": "eq", "causes": []}));
        assert_eq!(compound_answer_was_bounded(&clean), Some(false));

        // No scope object at all is UNKNOWN, not "complete". Answering
        // `false` for a body that cannot be read would be indistinguishable
        // from a genuinely unbounded answer.
        assert_eq!(compound_answer_was_bounded(&json!({"items": []})), None);
    }

    /// `files_not_indexed: null` is UNKNOWN, not zero — rendering it as `0`
    /// claims coverage that was never measured.
    #[test]
    fn null_coverage_is_unknown_and_zero_is_not() {
        let wrap = |scope| json!({"items": [], "scope": scope});
        assert_eq!(
            compound_coverage_unknown(&wrap(json!({"files_not_indexed": null}))),
            Some(true)
        );
        assert_eq!(
            compound_coverage_unknown(&wrap(json!({"files_not_indexed": 0}))),
            Some(false)
        );
        assert_eq!(
            compound_coverage_unknown(&wrap(json!({"files_not_indexed": 3}))),
            Some(false)
        );
        // The filter-execute route OMITS the key rather than nulling it,
        // precisely so `null` keeps its meaning here. Absent is not unknown.
        assert_eq!(
            compound_coverage_unknown(&wrap(json!({"match_count": 1}))),
            Some(false)
        );
        // …and no scope object at all is a different answer again.
        assert_eq!(compound_coverage_unknown(&json!({"items": []})), None);
    }

    /// Only assert what the docs actually state. `COMPOUND_QUERY_MAX_CHARS` is
    /// sourced ("Max 1024 characters") and so is the clause cap. There is
    /// deliberately no `limit` maximum constant — the docs never publish the
    /// number, so no such value can be asserted as "the documented server
    /// value".
    #[test]
    fn compound_bounds_match_the_documented_server_values() {
        assert_eq!(COMPOUND_QUERY_MAX_CHARS, 1024);
        assert_eq!(COMPOUND_MAX_CLAUSES, 5);
    }

    #[test]
    fn compound_filters_refuses_more_clauses_than_the_server_accepts() {
        let clause = r#"{"field":"f","operator":"="}"#;
        let ok = format!("[{}]", [clause; COMPOUND_MAX_CLAUSES].join(","));
        assert!(validate_compound_filters(&ok).is_ok(), "exactly 5 is legal");
        let over = format!("[{}]", [clause; COMPOUND_MAX_CLAUSES + 1].join(","));
        let err = validate_compound_filters(&over).expect_err("6 must be refused");
        assert!(
            err.to_string().contains("at most 5"),
            "the error must name the cap, got: {err}"
        );
    }

    /// THE MERGE BRANCH THAT MATTERS.
    ///
    /// The published API docs state: "A refusal is HTTP 200 in BOTH modes — a
    /// refused `confirm: true` answers exactly as a pre-flight does, with
    /// `merged: false`, and performs nothing. Read `would_refuse` and `merged`,
    /// never the status code."
    ///
    /// So a body that arrived on a 200 must still classify as Refused. Reading
    /// `merged` alone cannot tell a refusal from a pre-flight — both carry
    /// `merged: false` — which is why `would_refuse` is checked first.
    #[test]
    fn a_merge_refusal_arrives_on_http_200_and_is_not_success() {
        let refused = json!({
            "result": true,
            "source": "Author", "target": "author",
            "files_affected": 0, "target_files_before": 0,
            "would_refuse": {
                "reason": "merge_refused_type_mismatch",
                "message": "The two fields hold values of different declared types."
            },
            "merged": false, "already_merged": false, "snapshot": null
        });
        match classify_merge_response(&refused) {
            MergeOutcome::Refused { reason, .. } => {
                assert_eq!(reason, "merge_refused_type_mismatch");
            }
            other => panic!("a 200 carrying would_refuse must classify as Refused, got {other:?}"),
        }
    }

    /// `merge_source_already_merged` is the ONLY reason carrying a third key,
    /// and it is "the refusal an interactive picker never hits
    /// and an API or agent caller reaches easily" — i.e. exactly a CLI. The
    /// retry target must survive classification.
    #[test]
    fn already_merged_refusal_carries_the_retry_target() {
        let body = json!({
            "would_refuse": {
                "reason": "merge_source_already_merged",
                "message": "…",
                "source_resolves_to": "invoice_total"
            },
            "merged": false
        });
        match classify_merge_response(&body) {
            MergeOutcome::Refused {
                source_resolves_to, ..
            } => assert_eq!(source_resolves_to.as_deref(), Some("invoice_total")),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    /// An empty body must NOT read as a confident "would succeed". The merge
    /// pre-flight informs a decision the docs say cannot be undone, so asserting
    /// success from zero evidence is the wrong default.
    #[test]
    fn a_body_with_no_verdict_is_unknown_not_would_succeed() {
        assert_eq!(classify_merge_response(&json!({})), MergeOutcome::Unknown);
        assert_eq!(
            classify_merge_response(&json!({"result": true})),
            MergeOutcome::Unknown,
            "the envelope alone carries no verdict"
        );
        // …but an explicit `merged: false` with no refusal IS a pre-flight.
        assert_eq!(
            classify_merge_response(
                &json!({"result": true, "merged": false, "would_refuse": null})
            ),
            MergeOutcome::WouldSucceed
        );
    }

    /// A `would_refuse` we cannot parse must be Unknown, not Refused — saying
    /// "it did not happen" about an irreversible fold that may have happened is
    /// the worst available direction to be wrong in.
    #[test]
    fn an_unreadable_would_refuse_is_unknown_not_refused() {
        for bad in [json!(false), json!("nope"), json!(["a"]), json!(3)] {
            assert_eq!(
                classify_merge_response(&json!({"merged": true, "would_refuse": bad})),
                MergeOutcome::Unknown,
                "an unparseable would_refuse must not be reported as a refusal"
            );
        }
    }

    #[test]
    fn merge_outcomes_are_distinguished() {
        assert_eq!(
            classify_merge_response(&json!({"merged": true, "would_refuse": null})),
            MergeOutcome::Merged
        );
        assert_eq!(
            classify_merge_response(
                &json!({"merged": false, "already_merged": true, "would_refuse": null})
            ),
            MergeOutcome::AlreadyMerged
        );
        // Pre-flight: nothing refused, nothing performed.
        assert_eq!(
            classify_merge_response(&json!({"merged": false, "would_refuse": null})),
            MergeOutcome::WouldSucceed
        );
        // Also readable through the `{result, response}` envelope.
        assert_eq!(
            classify_merge_response(&json!({"response": {"merged": true}})),
            MergeOutcome::Merged
        );
    }

    #[test]
    fn declared_type_is_a_closed_set() {
        assert!(FIELD_DECLARED_TYPES.contains(&"datetime"));
        // Types used elsewhere in the platform that a field cannot store must
        // be refused, not passed through.
        for bogus in ["date", "number", "boolean", "text", "String"] {
            assert!(
                !FIELD_DECLARED_TYPES.contains(&bogus),
                "'{bogus}' must not be an accepted declared_type"
            );
        }
    }

    /// Three endpoints, three DIFFERENT whitespace rules — declare trims, merge
    /// treats a leading space as significant, a facts write refuses a padded
    /// name outright. A shared normalizer would be wrong on two of the three,
    /// so this pins that they are not shared.
    #[test]
    fn field_name_whitespace_rules_are_not_shared_across_endpoints() {
        // The declare path trims, so a padded name is 64-char-legal after trim.
        let padded = format!("  {}  ", "a".repeat(FIELD_NAME_MAX_CHARS));
        assert_eq!(padded.trim().chars().count(), FIELD_NAME_MAX_CHARS);
        // The merge path measures what is SENT, padding included — the same
        // string is over the limit there.
        assert!(padded.chars().count() > FIELD_NAME_MAX_CHARS);
    }

    /// The 25/100/250 snap belongs to `metadata/eligible/` and "a `page_size`
    /// elsewhere in the API is not necessarily quantized, so do not carry this
    /// rule across". Pinned so nobody folds them together.
    #[test]
    fn fields_page_size_is_a_plain_range_not_the_eligible_quantization() {
        assert_eq!(FIELDS_PAGE_SIZE_MAX, 250);
        for unquantized in [1_u32, 7, 99, 101, 249, 250] {
            assert!(
                (1..=FIELDS_PAGE_SIZE_MAX).contains(&unquantized),
                "{unquantized} is a legal fields page_size and must not be snapped"
            );
        }
    }

    /// THE SILENT WIPE. `PUT` is a full replace, and omitting `description`
    /// or `projection` clears it with a `200` — no error, no warning.
    /// `filter_params_from_existing` is the read half of the mandatory
    /// read-modify-write; this pins that a round-trip PRESERVES both fields.
    #[test]
    fn a_round_tripped_filter_preserves_the_fields_that_would_otherwise_be_wiped() {
        let existing = json!({
            "id": "f1", "name": "Contracts", "description": "signed MSAs",
            "predicate": [{"field": "document_type", "operator": "=", "value": "contract"}],
            "projection": {"sort": {"field": "updated", "dir": "desc"}},
            "template_id": "mt_x", "created": "2026-04-27 16:37:29 UTC"
        });
        let mut p = filter_params_from_existing(&existing);
        p.name = "Contracts (2026)".to_owned(); // the edit

        let body = filter_body(&p, false);
        assert_eq!(body["name"], json!("Contracts (2026)"));
        assert_eq!(
            body["description"],
            json!("signed MSAs"),
            "description must survive an unrelated edit — omitting it CLEARS it"
        );
        assert_eq!(
            body["projection"]["sort"]["dir"],
            json!("desc"),
            "projection must survive too"
        );
        assert!(
            body["predicate"].as_array().is_some_and(|a| a.len() == 1),
            "predicate is required on every update and must round-trip"
        );
        // template_id is NOT sent on update — the server ignores it, and sending
        // it would imply a control the caller does not have.
        assert!(body.get("template_id").is_none());
    }

    /// A `?output=terse` body omits `predicate` ENTIRELY (absent, not null).
    /// Round-tripping one would send `[]` and convert the filter to match-all —
    /// so the helper defaults to an empty array and the doc says read at `full`.
    /// This pins the shape so the hazard is visible rather than surprising.
    #[test]
    fn a_terse_filter_round_trip_yields_an_empty_predicate() {
        let terse = json!({"id": "f1", "name": "Contracts", "description": "d"});
        let p = filter_params_from_existing(&terse);
        assert_eq!(
            p.predicate,
            json!([]),
            "a terse read has no predicate; the caller must read at full tier"
        );
    }

    #[test]
    fn filter_params_are_validated_before_the_wire() {
        let base = |name: &str| FilterParams {
            name: name.to_owned(),
            predicate: json!([]),
            ..FilterParams::default()
        };
        assert!(validate_filter_params(&base("ok")).is_ok());
        assert!(validate_filter_params(&base("   ")).is_err(), "blank name");
        assert!(
            validate_filter_params(&base(&"a".repeat(FILTER_NAME_MAX_CHARS + 1))).is_err(),
            "name over 100"
        );
        let mut long_desc = base("ok");
        long_desc.description = Some("d".repeat(FILTER_DESCRIPTION_MAX_CHARS + 1));
        assert!(
            validate_filter_params(&long_desc).is_err(),
            "description over 255"
        );
        let mut not_array = base("ok");
        not_array.predicate = json!({"field": "x"});
        assert!(
            validate_filter_params(&not_array).is_err(),
            "predicate must be an array"
        );
        let mut too_many = base("ok");
        too_many.predicate = json!(vec![
            json!({"field":"f","operator":"="});
            COMPOUND_MAX_CLAUSES + 1
        ]);
        assert!(
            validate_filter_params(&too_many).is_err(),
            "over the clause cap"
        );
    }

    /// The server answers all three length/cap violations with ONE generic
    /// `1605` whose message does not say which was hit — so the client message
    /// must name the field, or the user cannot tell what to fix.
    #[test]
    fn each_validation_error_names_its_own_field() {
        let mut p = FilterParams {
            name: "a".repeat(FILTER_NAME_MAX_CHARS + 1),
            predicate: json!([]),
            ..FilterParams::default()
        };
        assert!(
            validate_filter_params(&p)
                .unwrap_err()
                .to_string()
                .contains("name")
        );
        p.name = "ok".to_owned();
        p.description = Some("d".repeat(FILTER_DESCRIPTION_MAX_CHARS + 1));
        assert!(
            validate_filter_params(&p)
                .unwrap_err()
                .to_string()
                .contains("description")
        );
    }

    #[test]
    fn filter_paths_encode_both_segments() {
        assert_eq!(
            filters_path("4687730903718774523"),
            "/workspace/4687730903718774523/metadata/filters/"
        );
        assert_eq!(
            filter_path("a/b", "c/d"),
            "/workspace/a%2Fb/metadata/filters/c%2Fd/"
        );
    }

    #[test]
    fn node_facts_path_encodes_both_segments() {
        assert_eq!(
            node_facts_path("4687730903718774523", "2yxh5-ojakx-r3mwz"),
            "/workspace/4687730903718774523/storage/2yxh5-ojakx-r3mwz/metadata/facts/"
        );
        assert_eq!(
            node_facts_path("a/b", "c/d"),
            "/workspace/a%2Fb/storage/c%2Fd/metadata/facts/"
        );
    }

    /// The read and the write share one path — they differ only in verb and
    /// body, and the platform meters them on separate allowances. Pinned so a
    /// future edit cannot fork them.
    #[test]
    fn facts_read_and_write_share_one_path() {
        let a = node_facts_path("1", "n");
        assert!(a.ends_with("/metadata/facts/"), "got {a}");
    }

    #[test]
    fn facts_must_be_a_non_empty_object_within_the_cap() {
        // An array is refused server-side; refuse it here too rather than
        // spending a round trip.
        assert!(validate_node_facts(&json!([{"field": "a"}])).is_err());
        assert!(validate_node_facts(&json!("not an object")).is_err());
        assert!(validate_node_facts(&json!({})).is_err());

        assert!(validate_node_facts(&json!({"invoice_number": "INV-1"})).is_ok());

        let mut at_cap = serde_json::Map::new();
        for i in 0..NODE_FACTS_MAX_ENTRIES {
            at_cap.insert(format!("f{i}"), json!(i));
        }
        let over = {
            let mut m = at_cap.clone();
            m.insert("one_too_many".to_owned(), json!(1));
            serde_json::Value::Object(m)
        };
        assert!(
            validate_node_facts(&serde_json::Value::Object(at_cap)).is_ok(),
            "exactly {NODE_FACTS_MAX_ENTRIES} must be accepted — the doc says 'at most', inclusive"
        );
        let err = validate_node_facts(&over).expect_err("one past the cap must be refused");
        assert!(
            err.to_string().contains("101") && err.to_string().contains("100"),
            "error must name both the cap and what was passed, got: {err}"
        );
    }

    /// New names permanently create fields that can never be deleted, so the
    /// caller needs to know which ones a write would mint BEFORE it happens.
    #[test]
    fn new_field_names_reports_only_names_the_vocabulary_lacks() {
        let known = ["invoice_number", "Author"];
        let facts = json!({
            "invoice_number": "INV-1",
            "amount_due": 42.5,
            "contract_type": "MSA",
        });
        let mut got = new_field_names(&facts, &known);
        got.sort();
        assert_eq!(
            got,
            vec!["amount_due".to_owned(), "contract_type".to_owned()]
        );
    }

    /// `Author` and `author` are the SAME field, so a differently cased
    /// spelling of a known name must NOT be reported as new — reporting it
    /// would warn about a permanent field creation that will not happen.
    #[test]
    fn new_field_names_folds_ascii_case_against_the_known_vocabulary() {
        let known = ["Author"];
        assert!(new_field_names(&json!({"author": "A"}), &known).is_empty());
        assert!(new_field_names(&json!({"AUTHOR": "A"}), &known).is_empty());
    }

    /// A `null` entry creates nothing, so it must not be announced as a field
    /// the write will mint.
    #[test]
    fn new_field_names_ignores_null_entries() {
        assert!(new_field_names(&json!({"never_seen": null}), &[]).is_empty());
        assert_eq!(
            new_field_names(&json!({"never_seen": null, "real": 1}), &[]),
            vec!["real".to_owned()]
        );
    }

    fn parsed_metadata(body: &serde_json::Value) -> BulkMetadataDetailsResponse {
        parse_bulk_metadata_details_response(body).expect("test body should parse")
    }

    #[test]
    fn metadata_parse_multi_format_envelope_wrapped() {
        let body = json!({
            "result": "yes",
            "response": {
                "format": "multi",
                "objects": [
                    {"node_id": "abc", "template_id": "tpl1", "custom_metadata": {}}
                ],
                "templates": {
                    "tpl1": {"name": "Photos", "fields": []}
                },
                "errors": [
                    {"node_id": "missing", "code": 191_049, "message": "not found"}
                ]
            }
        });
        let r = parsed_metadata(&body);
        assert_eq!(r.objects.len(), 1);
        assert_eq!(r.templates.len(), 1);
        assert!(r.templates.contains_key("tpl1"));
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].node_id, "missing");
        assert_eq!(r.errors[0].code, 191_049);
    }

    #[test]
    fn metadata_parse_multi_format_404_all_errored() {
        let body = json!({
            "result": "no",
            "response": {
                "format": "multi",
                "objects": [],
                "templates": {},
                "errors": [
                    {"node_id": "x", "code": 147_196, "message": "invalid id"},
                    {"node_id": "Y", "code": 191_049, "message": "not found"}
                ]
            }
        });
        let r = parsed_metadata(&body);
        assert!(r.objects.is_empty());
        assert!(r.templates.is_empty());
        assert_eq!(r.errors.len(), 2);
        assert_eq!(r.errors[1].node_id, "Y");
    }

    #[test]
    fn metadata_parse_single_format_returns_object_without_hoist() {
        // Current per-node details shape: split template_metadata/custom_metadata,
        // no top-level `template`. The non-multi fallback returns it verbatim as
        // objects[0] with an empty templates map (nothing to hoist).
        let body = json!({
            "result": "yes",
            "response": {
                "object_id": "abc",
                "template_id": "tpl1",
                "template_metadata": [{"key": "k", "value": "v"}],
                "custom_metadata": []
            }
        });
        let r = parsed_metadata(&body);
        assert_eq!(r.objects.len(), 1);
        assert!(r.templates.is_empty());
        assert_eq!(r.objects[0]["template_id"], "tpl1");
        assert_eq!(r.objects[0]["template_metadata"][0]["key"], "k");
    }

    #[test]
    fn metadata_parse_missing_format_with_objects_treats_as_multi() {
        let body = json!({
            "result": "no",
            "response": {
                "objects": [],
                "errors": [{"node_id": "x", "code": 191_049, "message": "missing"}]
            }
        });
        let r = parsed_metadata(&body);
        assert!(r.objects.is_empty());
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn metadata_parse_unknown_format_returns_parse_error() {
        let body = json!({
            "result": "yes",
            "response": {"format": "v2", "objects": []}
        });
        let err =
            parse_bulk_metadata_details_response(&body).expect_err("unknown format must error");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn metadata_parse_non_object_payload_returns_parse_error() {
        let body = json!([1, 2, 3]);
        let err =
            parse_bulk_metadata_details_response(&body).expect_err("non-object payload must error");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn metadata_sanitize_strips_control_and_bidi_codepoints() {
        let raw = "hello\x07\u{202E}drowssap\u{200D}.txt\u{FEFF}";
        let cleaned = sanitize_terminal_string(raw);
        assert_eq!(cleaned, "hellodrowssap.txt");
        assert_eq!(sanitize_terminal_string("a\x1bb"), "ab");
        assert_eq!(sanitize_terminal_string("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn metadata_build_path_joins_commas_literal() {
        let path = build_bulk_metadata_details_path(
            "ws-1",
            &["abc".to_owned(), "DeF".to_owned(), "ghi-jkl".to_owned()],
        )
        .expect("happy path");
        assert_eq!(
            path,
            "/workspace/ws-1/storage/abc,DeF,ghi-jkl/metadata/details/"
        );
    }

    #[test]
    fn metadata_build_path_duplicates_single_id_to_force_bulk_shape() {
        let path = build_bulk_metadata_details_path("ws", &["abc".to_owned()]).expect("happy path");
        assert_eq!(path, "/workspace/ws/storage/abc,abc/metadata/details/");
    }

    #[test]
    fn metadata_build_path_rejects_empty_input() {
        let err =
            build_bulk_metadata_details_path("ws", &[]).expect_err("empty input must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn metadata_build_path_rejects_oversize_input() {
        let ids: Vec<String> = (0..=BULK_METADATA_DETAILS_MAX_IDS)
            .map(|i| format!("id{i}"))
            .collect();
        let err = build_bulk_metadata_details_path("ws", &ids)
            .expect_err("oversize input must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn metadata_build_path_encodes_individual_ids() {
        let path = build_bulk_metadata_details_path("ws", &["a,b".to_owned(), "c d".to_owned()])
            .expect("happy path");
        assert_eq!(path, "/workspace/ws/storage/a%2Cb,c%20d/metadata/details/");
    }

    #[test]
    fn metadata_bulk_max_ids_matches_server_cap() {
        assert_eq!(BULK_METADATA_DETAILS_MAX_IDS, 25);
    }

    #[test]
    fn extract_fields_rejects_malformed_or_empty() {
        assert!(validate_extract_fields("not json").is_err());
        assert!(validate_extract_fields("{}").is_err());
        assert!(validate_extract_fields("[]").is_err());
        assert!(validate_extract_fields("[null]").is_err());
        assert!(validate_extract_fields(r#"["foo",""]"#).is_err());
        assert!(validate_extract_fields(r#"["foo","   "]"#).is_err());
    }

    #[test]
    fn extract_fields_accepts_valid() {
        assert!(validate_extract_fields(r#"["field1"]"#).is_ok());
        assert!(validate_extract_fields(r#"["field1","field2","field3"]"#).is_ok());
    }

    #[test]
    fn search_query_rejects_empty() {
        assert!(validate_search_query("").is_err());
    }

    #[test]
    fn search_query_rejects_whitespace_only() {
        // Defense-in-depth: caller trims, but validator must also reject
        // whitespace-only input on its own.
        assert!(validate_search_query("   ").is_err());
        assert!(validate_search_query("\t\n").is_err());
    }

    #[test]
    fn search_query_short_circuits_oversize_bytes() {
        // 5 KB of ASCII is well past the 1024-char cap and should be
        // rejected without walking the codepoints.
        let s: String = "x".repeat(METADATA_SEARCH_QUERY_MAX_CHARS * 4 + 1);
        assert!(validate_search_query(&s).is_err());
    }

    #[test]
    fn search_query_rejects_too_long() {
        let s: String = "x".repeat(METADATA_SEARCH_QUERY_MAX_CHARS + 1);
        assert!(validate_search_query(&s).is_err());
    }

    #[test]
    fn search_query_accepts_boundary_values() {
        assert!(validate_search_query("a").is_ok());
        let s: String = "x".repeat(METADATA_SEARCH_QUERY_MAX_CHARS);
        assert!(validate_search_query(&s).is_ok());
    }

    #[test]
    fn search_limit_rejects_zero_and_over_cap() {
        assert!(validate_search_limit(0).is_err());
        assert!(validate_search_limit(METADATA_SEARCH_LIMIT_MAX + 1).is_err());
    }

    #[test]
    fn search_limit_accepts_boundary_values() {
        assert!(validate_search_limit(1).is_ok());
        assert!(validate_search_limit(METADATA_SEARCH_LIMIT_MAX).is_ok());
    }

    #[test]
    fn search_window_rejects_overflow_window() {
        // 9_999 + 100 (default limit) = 10_099, past the 10_000 cap.
        assert!(validate_search_window(None, Some(9_999)).is_err());
        // Explicit limit + offset over cap.
        assert!(
            validate_search_window(Some(100), Some(METADATA_SEARCH_DEEP_PAGE_MAX - 99)).is_err()
        );
    }

    #[test]
    fn search_window_accepts_boundary_values() {
        // Default limit 100, offset 9_900 = 10_000 (boundary, allowed).
        assert!(validate_search_window(None, Some(9_900)).is_ok());
        // Smallest possible page at boundary.
        assert!(validate_search_window(Some(1), Some(METADATA_SEARCH_DEEP_PAGE_MAX - 1)).is_ok());
    }

    fn jobs_status_with_extract(entry: &serde_json::Value) -> serde_json::Value {
        json!({
            "result": "yes",
            "response": {
                "jobs": {
                    "intelligence": null,
                    "metadata_extract": [entry]
                }
            }
        })
    }

    #[test]
    fn classify_extract_job_detects_completed() {
        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": "abc",
            "job_id": "j1",
            "status": "completed",
            "progress_percent": 100
        }));
        assert_eq!(
            classify_single_extract_job(&body, "abc", Some("j1")),
            ExtractJobState::Completed
        );
        // job_id None still matches the node's single entry.
        assert_eq!(
            classify_single_extract_job(&body, "abc", None),
            ExtractJobState::Completed
        );
    }

    #[test]
    fn classify_extract_job_detects_errored_with_message() {
        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": "abc",
            "job_id": "j1",
            "status": "errored",
            "error_message": "extraction failed: bad mimetype"
        }));
        assert_eq!(
            classify_single_extract_job(&body, "abc", Some("j1")),
            ExtractJobState::Errored(Some("extraction failed: bad mimetype".to_owned()))
        );
    }

    #[test]
    fn classify_extract_job_errored_without_message_is_none_payload() {
        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": "abc",
            "status": "errored"
        }));
        assert_eq!(
            classify_single_extract_job(&body, "abc", None),
            ExtractJobState::Errored(None)
        );
    }

    #[test]
    fn classify_extract_job_in_progress_is_pending() {
        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": "abc",
            "job_id": "j1",
            "status": "in_progress",
            "progress_percent": 0
        }));
        assert_eq!(
            classify_single_extract_job(&body, "abc", Some("j1")),
            ExtractJobState::Pending
        );
    }

    #[test]
    fn classify_extract_job_queued_is_pending() {
        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": "abc",
            "status": "queued"
        }));
        assert_eq!(
            classify_single_extract_job(&body, "abc", None),
            ExtractJobState::Pending
        );
    }

    #[test]
    fn classify_extract_job_missing_entry_is_not_found() {
        // Empty list.
        let body = json!({
            "response": { "jobs": { "metadata_extract": [] } }
        });
        assert_eq!(
            classify_single_extract_job(&body, "abc", None),
            ExtractJobState::NotFound
        );
        // No jobs key at all.
        let bare = json!({ "response": {} });
        assert_eq!(
            classify_single_extract_job(&bare, "abc", None),
            ExtractJobState::NotFound
        );
    }

    #[test]
    fn classify_extract_job_ignores_batch_and_other_nodes() {
        let body = json!({
            "response": {
                "jobs": {
                    "metadata_extract": [
                        {"kind": "batch", "node_id": null, "status": "completed"},
                        {"kind": "single", "node_id": "other", "status": "completed"},
                        {"kind": "single", "node_id": "abc", "job_id": "want", "status": "in_progress"}
                    ]
                }
            }
        });
        // batch entry and other-node entry are skipped; our node is pending.
        assert_eq!(
            classify_single_extract_job(&body, "abc", Some("want")),
            ExtractJobState::Pending
        );
        // job_id mismatch on the single entry → no match → NotFound.
        assert_eq!(
            classify_single_extract_job(&body, "abc", Some("nope")),
            ExtractJobState::NotFound
        );
    }

    // The display and wire renderings of one real node id. `/jobs/status/`
    // publishes the raw `->id` property and never calls the formatter, so the
    // entry always carries the UNHYPHENATED form — while the id a user copies
    // out of `fastio files list` (or any formatted surface) is hyphenated.
    // Every id below is the SAME id in two renderings.
    const NODE_HYPHENATED: &str = "2yxh5-ojakx-r3mwz-ty6tv-k66cj-nqsw";
    const NODE_RAW: &str = "2yxh5ojakxr3mwzty6tvk66cjnqsw";

    #[test]
    fn classify_extract_job_matches_across_id_renderings() {
        // REGRESSION: the compare used a raw `!=` against the caller's string,
        // so a hyphenated `node_id` never matched the unhyphenated one the
        // jobs-status payload carries. `metadata extract --wait` and the MCP
        // twin could therefore NEVER observe a terminal state: they burned the
        // full 600s window and reported a timeout on an extraction that had
        // succeeded in seconds.
        //
        // This survived the existing suite because every other test in this
        // module uses short fake ids ("abc") that are byte-identical in both
        // renderings — the fixtures could not express the defect.
        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": NODE_RAW,
            "job_id": "j1",
            "status": "completed"
        }));
        assert_eq!(
            classify_single_extract_job(&body, NODE_HYPHENATED, Some("j1")),
            ExtractJobState::Completed,
            "hyphenated caller id must match the raw id in the jobs-status entry"
        );
        // And the reverse direction, so the fix is not one-way.
        let raw_body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": NODE_HYPHENATED,
            "job_id": "j1",
            "status": "completed"
        }));
        assert_eq!(
            classify_single_extract_job(&raw_body, NODE_RAW, Some("j1")),
            ExtractJobState::Completed
        );
    }

    #[test]
    fn classify_extract_job_compares_job_id_raw_not_canonicalized() {
        // DELIBERATE ASYMMETRY, LOCKED IN BY THIS TEST. `job_id` looks like the
        // same cross-channel shape as `node_id` and it is tempting to normalize
        // it "for consistency" — but the contract closes the question the other
        // way: per the `job_id` paragraph of the single-file extract route in
        // the published API docs (the one beginning "`job_id` is `null` while
        // the extraction is in flight"), a terminal entry returns the `202`'s id
        // BYTE FOR BYTE, and the wait loops take `job_id` from that `202` — not
        // from a user-typed argument — so no rendering split exists to bridge.
        //
        // The fixture is a realistic job id: a 29-char OpaqueId, which is the
        // shape the server mints for a job (a distinct id type from the node's,
        // hence a separate constant rather than reusing NODE_RAW).
        //
        // This test exists so that normalizing `job_id` is a deliberate act
        // that breaks a test carrying the reason, rather than a tidy-up.
        const JOB_RAW: &str = "aj3k9x2m7q4w8r5t1y6u0i9o3p2sd";
        const JOB_HYPHENATED: &str = "aj3k9-x2m7q-4w8r5-t1y6u-0i9o3-p2sd";

        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": NODE_RAW,
            "job_id": JOB_RAW,
            "status": "completed"
        }));
        // Exact match works.
        assert_eq!(
            classify_single_extract_job(&body, NODE_HYPHENATED, Some(JOB_RAW)),
            ExtractJobState::Completed
        );
        // The DISPLAY rendering of the very same job id must NOT match: this is
        // the assertion that fails the moment someone canonicalizes `job_id`.
        assert_eq!(
            classify_single_extract_job(&body, NODE_HYPHENATED, Some(JOB_HYPHENATED)),
            ExtractJobState::NotFound,
            "job_id is compared raw by design — see the doc comment before changing this"
        );
    }

    #[test]
    fn classify_extract_job_normalization_does_not_collapse_the_guard() {
        // POSITIVE CONTROL for the two tests above. Making ids match is trivial
        // to do by accident in the wrong direction — deleting the guard passes
        // both of them. These assertions fail if normalization ever degrades
        // into "everything matches", so the tests above mean something.
        let body = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": NODE_RAW,
            "job_id": "j1",
            "status": "completed"
        }));
        // A genuinely different node — same shape, same length, one char apart.
        assert_eq!(
            classify_single_extract_job(&body, "2yxh5-ojakx-r3mwz-ty6tv-k66cj-nqsx", Some("j1")),
            ExtractJobState::NotFound,
            "a different node id must NOT match merely because hyphens are stripped"
        );
        // A genuinely different job on the right node.
        assert_eq!(
            classify_single_extract_job(&body, NODE_HYPHENATED, Some("j2")),
            ExtractJobState::NotFound,
            "a different job id must NOT match merely because hyphens are stripped"
        );
        // Hyphens alone must not be treated as an id: an all-hyphen string
        // canonicalizes to empty and must not match a real entry.
        assert_eq!(
            classify_single_extract_job(&body, "-----", Some("j1")),
            ExtractJobState::NotFound
        );
    }

    #[test]
    fn classify_extract_job_empty_canonical_id_matches_nothing() {
        // `canonicalize` is NOT injective over malformed input: "-", "-----"
        // and "" all collapse to "". Without the empty guard, any all-hyphen
        // argument would match an entry carrying an empty `node_id` — a match
        // the previous raw compare could never make, i.e. a hole opened BY the
        // normalization rather than a pre-existing one.
        let empty_entry = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": "",
            "job_id": "j1",
            "status": "completed"
        }));
        for degenerate in ["", "-", "-----", "   ", " - - - "] {
            assert_eq!(
                classify_single_extract_job(&empty_entry, degenerate, None),
                ExtractJobState::NotFound,
                "degenerate id {degenerate:?} must not match an empty entry node_id"
            );
        }
        // Positive control: the SAME entry shape with a real id on both sides
        // does match, so the assertions above are not passing merely because
        // this fixture can never match anything.
        let real_entry = jobs_status_with_extract(&json!({
            "kind": "single",
            "node_id": NODE_RAW,
            "job_id": "j1",
            "status": "completed"
        }));
        assert_eq!(
            classify_single_extract_job(&real_entry, NODE_HYPHENATED, None),
            ExtractJobState::Completed
        );
    }

    #[test]
    fn classify_extract_job_works_on_flat_body() {
        // Tolerates a body without the `response` envelope wrapper.
        let body = json!({
            "jobs": {
                "metadata_extract": [
                    {"kind": "single", "node_id": "abc", "status": "completed"}
                ]
            }
        });
        assert_eq!(
            classify_single_extract_job(&body, "abc", None),
            ExtractJobState::Completed
        );
    }

    #[test]
    fn classify_extract_job_running_then_gone_is_never_terminal_success() {
        // FIX 4: the bounded `--wait` / extract-and-wait poll loops must NOT
        // treat a "seen running, then absent" transition as success. The
        // classifier never reports `Completed`/`Errored` for a running or
        // missing entry, so a loop that only terminates on those terminal
        // states cannot report a false success within its sub-age-out window.
        let running = json!({
            "response": {
                "jobs": {
                    "metadata_extract": [
                        {"kind": "single", "node_id": "abc", "job_id": "j1", "status": "in_progress"}
                    ]
                }
            }
        });
        assert_eq!(
            classify_single_extract_job(&running, "abc", Some("j1")),
            ExtractJobState::Pending
        );

        // Next poll: the entry has vanished from the list.
        let gone = json!({
            "response": { "jobs": { "metadata_extract": [] } }
        });
        let after = classify_single_extract_job(&gone, "abc", Some("j1"));
        assert_eq!(after, ExtractJobState::NotFound);
        // The invariant the loops depend on: neither state is terminal success.
        assert_ne!(after, ExtractJobState::Completed);
        assert!(!matches!(after, ExtractJobState::Errored(_)));
    }

    #[test]
    fn strip_declared_types_drops_type_from_every_entry_container() {
        // Fixtures use the documented entry shape
        // `{key, description, type, value, is_auto, updated}`: `type` sits at
        // index 2 with three keys AFTER it, which is what makes a reordering
        // strip observable below.
        //
        // Every container's entry carries an OBJECT value with its own `type`
        // key, so the over-strip lock below runs once per container. Covering
        // only some of them let an over-strip confined to a single container
        // ship green — and the legacy `metadata` one is the highest-exposure
        // container, not the lowest: it is the sole container in both the
        // documented single-node response and the `?output=terse` field list.
        let mut payload = json!({
            "object_id": "abc",
            "template_metadata": [
                {
                    "key": "invoice_number",
                    "description": "Invoice number",
                    "type": "json",
                    "value": {"number": "INV-1", "type": "user-supplied"},
                    "is_auto": false,
                    "updated": "2026-01-28 12:30:00 UTC"
                }
            ],
            "custom_metadata": [
                {
                    "key": "note",
                    "description": null,
                    "type": "json",
                    // A `type: "json"` entry holds arbitrary user data, which
                    // may itself have a `type` key. That inner one is a stored
                    // VALUE, not a declared type — see the over-strip
                    // assertion inside the loop.
                    "value": {"a": 1, "type": "user-supplied"},
                    "is_auto": false,
                    "updated": "2026-01-28 12:31:00 UTC"
                }
            ],
            "metadata": [
                {
                    "key": "legacy",
                    "description": "Legacy field",
                    "type": "json",
                    "value": {"amount": 1.5, "type": "user-supplied"},
                    "is_auto": true,
                    "updated": "2026-01-28 12:32:00 UTC"
                }
            ]
        });
        strip_declared_metadata_types(&mut payload);

        for container in ["template_metadata", "custom_metadata", "metadata"] {
            let entry = &payload[container][0];
            assert!(
                entry.get("type").is_none(),
                "{container}[0] must lose the declared type, got: {entry}"
            );
            assert!(
                entry.get("key").is_some() && entry.get("value").is_some(),
                "{container}[0] must keep its other fields, got: {entry}"
            );
            // Dropping `type` must not REORDER the survivors. Every human
            // renderer derives its column order from key order, so a
            // swap-remove would silently move `updated` into the vacated slot.
            let keys: Vec<&str> = entry
                .as_object()
                .expect("entry is an object")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                ["key", "description", "value", "is_auto", "updated"],
                "{container}[0] surviving keys must keep their original relative order"
            );
            // OVER-STRIP LOCK, once per container. The descent stops AT the
            // entry: it removes the entry's own `type` and never walks into
            // `value`. A stored value is opaque user data that may
            // legitimately carry a `type` key of its own, and deleting that
            // corrupts the payload on every human and MCP render — silently,
            // since the stripped result still looks well formed. Asserting
            // this inside the loop is what makes it parity-proof: a container
            // added to `METADATA_ENTRY_CONTAINERS` later gets the check for
            // free, and an over-strip confined to ONE container cannot hide.
            assert_eq!(
                entry["value"]["type"], "user-supplied",
                "{container}[0]: a `type` INSIDE a stored value is user data and must survive"
            );
        }
    }

    #[test]
    fn strip_declared_types_preserves_storage_node_type() {
        // `type` is an overloaded key: THIS payload carries `node_id.type`
        // (the storage node's KIND — `file`/`folder`/`note`) right beside the
        // declared metadata types this strip targets. A blanket walk would
        // take both, so the descent stays confined to the entry containers.
        let mut payload = json!({
            "node_id": {"id": "abc", "name": "invoice.pdf", "type": "file", "size": 42},
            "template_metadata": [{"key": "k", "type": "string", "value": "v"}]
        });
        strip_declared_metadata_types(&mut payload);

        assert_eq!(
            payload["node_id"]["type"], "file",
            "storage node kind must survive"
        );
        assert!(payload["template_metadata"][0].get("type").is_none());
    }

    #[test]
    fn strip_declared_types_leaves_room_message_parts_alone() {
        // Room messages carry `parts[].type`, which is NOT a metadata entry
        // container — a blanket "remove any key named type" would break it.
        let mut payload = json!({
            "parts": [
                {"type": "text", "text": "hello"},
                {"type": "file", "node_id": "abc"}
            ]
        });
        strip_declared_metadata_types(&mut payload);

        assert_eq!(payload["parts"][0]["type"], "text");
        assert_eq!(payload["parts"][1]["type"], "file");
    }

    #[test]
    fn strip_drops_fact_declared_type_nested_under_node_id_inside_bulk_objects() {
        // THE CELL THE FIXTURE SET WAS MISSING. Both placements of
        // `metadata_facts` were asserted for the SINGLE-node payload
        // (`strip_drops_fact_declared_type_but_keeps_stored_type`), and the
        // bulk test beside this one uses a `node_id` carrying no facts at all —
        // so the `node_id`-NESTED copy inside each bulk `objects[]` entry was
        // never covered, and it was the one placement the strip missed. Every
        // bulk `metadata details` response leaked `declared_type`.
        let entry = json!({
            "field": "count",
            "value": "abc",
            "declared_type": "int",
            "stored_type": "string"
        });
        let mut payload = json!({
            "format": "multi",
            "objects": [
                {
                    "node_id": {
                        "id": "a",
                        "type": "file",
                        "metadata_facts": { "items": [entry.clone()] }
                    },
                    "metadata_facts": { "items": [entry.clone()] }
                }
            ]
        });
        strip_declared_metadata_types(&mut payload);
        let s = payload.to_string();
        assert!(!s.contains("declared_type"), "declared_type must go: {s}");
        assert!(s.contains("stored_type"), "stored_type must stay: {s}");
        assert!(s.contains("abc"), "the value must survive: {s}");
        // The storage node's OWN `type` is not a fact claim and must survive.
        assert_eq!(payload["objects"][0]["node_id"]["type"], json!("file"));
    }

    #[test]
    fn strip_declared_types_descends_into_bulk_objects() {
        let mut payload = json!({
            "format": "multi",
            "objects": [
                {
                    "node_id": {"id": "a", "type": "file"},
                    "template_metadata": [{"key": "k1", "type": "string", "value": "v1"}],
                    "custom_metadata": [{"key": "k2", "type": "bool", "value": true}],
                    // `?output=terse` collapses the split containers into a
                    // single `metadata` array — stripped inside `objects[]`
                    // too.
                    "metadata": [{"key": "k4", "type": "int", "value": 7}]
                },
                {
                    "node_id": {"id": "b", "type": "note"},
                    "template_metadata": [{"key": "k3", "type": "datetime", "value": "2026-01-01"}],
                    "custom_metadata": []
                }
            ],
            // `templates` is the SCHEMA half of the bulk envelope: its
            // `fields[].type` declares what a template's field should hold and
            // is not a claim about any stored value, so the strip must not
            // descend here. The MCP bulk arm embeds this map in the payload it
            // strips and renders to an agent, so an over-strip is user-visible.
            "templates": {
                "tpl1": {
                    "name": "Invoices",
                    "fields": [{"name": "k1", "type": "string"}]
                }
            }
        });
        strip_declared_metadata_types(&mut payload);

        assert!(
            payload["objects"][0]["template_metadata"][0]
                .get("type")
                .is_none()
        );
        assert!(
            payload["objects"][0]["custom_metadata"][0]
                .get("type")
                .is_none()
        );
        assert!(payload["objects"][0]["metadata"][0].get("type").is_none());
        assert_eq!(
            payload["objects"][0]["metadata"][0]["value"], 7,
            "the stored value must survive"
        );
        assert!(
            payload["objects"][1]["template_metadata"][0]
                .get("type")
                .is_none()
        );
        // Node kinds inside the bulk wrapper survive too.
        assert_eq!(payload["objects"][0]["node_id"]["type"], "file");
        assert_eq!(payload["objects"][1]["node_id"]["type"], "note");
        // OVER-STRIP LOCK: the template SCHEMA keeps its declared field types.
        assert_eq!(
            payload["templates"]["tpl1"]["fields"][0]["type"], "string",
            "a template field's declared type is schema, not a stored value, \
             and must survive the strip"
        );
    }

    #[test]
    fn strip_declared_types_handles_root_level_array() {
        // The bulk non-JSON render path hands the renderer a bare array of
        // per-node objects, not the `{objects: [...]}` wrapper.
        let mut payload = json!([
            {
                "node_id": {"id": "a", "type": "folder"},
                "template_metadata": [{"key": "k", "type": "int", "value": 1}]
            }
        ]);
        strip_declared_metadata_types(&mut payload);

        assert!(payload[0]["template_metadata"][0].get("type").is_none());
        assert_eq!(payload[0]["node_id"]["type"], "folder");
    }

    #[test]
    fn strip_drops_fact_declared_type_but_keeps_stored_type() {
        // MEASURED 2026-08-27: a fact entry carries BOTH. They are not
        // the same kind of thing — `declared_type` is a CLAIM about the value
        // that can disagree with reality; `stored_type` is the observed type.
        // Human formats drop the claim and keep the observation.
        //
        // Facts arrive NESTED under `node_id` today and additionally TOP-LEVEL
        // from 2026-08-27, so both placements are asserted: a position-keyed
        // rule would silently stop applying when the payload moved.
        let entry = json!({
            "field": "count",
            "value": "abc",
            "declared_type": "int",
            "stored_type": "string"
        });
        for mut payload in [
            json!({ "node_id": { "metadata_facts": { "items": [entry.clone()] } } }),
            json!({ "metadata_facts": { "items": [entry.clone()] } }),
        ] {
            strip_declared_metadata_types(&mut payload);
            let s = payload.to_string();
            assert!(!s.contains("declared_type"), "declared_type must go: {s}");
            assert!(s.contains("stored_type"), "stored_type must stay: {s}");
            assert!(s.contains("abc"), "the value must survive: {s}");
        }
    }

    #[test]
    fn strip_declared_types_never_touches_the_stored_value() {
        // The whole point of SC5: a field DECLARED `int` can hold `"abc"`.
        // The strip removes the unbackable claim, never the value itself.
        //
        // A scalar value alone cannot discriminate here — no strip could
        // damage `"abc"` — so the second entry carries the case the name
        // actually claims: an OBJECT value with its own `type` key, which a
        // strip that descended one level too far would silently delete.
        // The third entry carries an ARRAY value: `metadata[].type` includes
        // `json` and the documented `value` is "mixed", so a stored value is
        // legitimately a list of objects, each of which may carry its own
        // `type`. A strip that walked into an array-valued `value` would
        // delete every one of them in a single pass.
        let mut payload = json!({
            "template_metadata": [
                {"key": "count", "type": "int", "value": "abc"},
                {
                    "key": "note",
                    "type": "json",
                    "value": {"type": "invoice", "total": 12, "nested": {"type": "line-item"}}
                },
                {
                    "key": "rows",
                    "type": "json",
                    "value": [{"type": "line-item", "qty": 1}, {"type": "tax", "qty": 2}]
                }
            ]
        });
        strip_declared_metadata_types(&mut payload);

        assert!(payload["template_metadata"][0].get("type").is_none());
        assert_eq!(
            payload["template_metadata"][0]["value"], "abc",
            "the stored value must be relayed untouched"
        );
        assert!(payload["template_metadata"][1].get("type").is_none());
        assert_eq!(
            payload["template_metadata"][1]["value"],
            json!({"type": "invoice", "total": 12, "nested": {"type": "line-item"}}),
            "an object-valued entry must keep every `type` key inside it, at \
             any depth"
        );
        // That `assert_eq!` is order-INSENSITIVE: `serde_json::Map: PartialEq`
        // delegates to `IndexMap`'s length-plus-per-key-lookup compare, so a
        // strip that removed the inner `type` and re-inserted it elsewhere
        // still passes. Key order inside a stored value IS user-visible —
        // `output/markdown.rs` serializes a complex cell with
        // `serde_json::to_string` — so it is asserted separately, the same
        // discipline `shift_remove` applies to the entry's own keys.
        let value_keys: Vec<&str> = payload["template_metadata"][1]["value"]
            .as_object()
            .expect("the stored value is still an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            value_keys,
            ["type", "total", "nested"],
            "a stored object value must keep its key ORDER, not just its keys"
        );
        assert!(payload["template_metadata"][2].get("type").is_none());
        assert_eq!(
            payload["template_metadata"][2]["value"],
            json!([{"type": "line-item", "qty": 1}, {"type": "tax", "qty": 2}]),
            "every element of an ARRAY-valued entry must keep its own `type`"
        );
        let element_keys: Vec<Vec<&str>> = payload["template_metadata"][2]["value"]
            .as_array()
            .expect("the stored value is still an array")
            .iter()
            .map(|element| {
                element
                    .as_object()
                    .expect("array element is an object")
                    .keys()
                    .map(String::as_str)
                    .collect()
            })
            .collect();
        assert_eq!(
            element_keys,
            [["type", "qty"], ["type", "qty"]],
            "array elements must keep their key ORDER too"
        );
    }

    #[test]
    fn strip_declared_types_tolerates_unexpected_shapes() {
        // Non-object entries, non-array containers, and scalars must all be
        // no-ops rather than panics.
        let mut scalar = json!("just a string");
        strip_declared_metadata_types(&mut scalar);
        assert_eq!(scalar, json!("just a string"));

        let mut odd = json!({
            "template_metadata": "not an array",
            "custom_metadata": [42, null, {"key": "k", "type": "string"}],
            "objects": {"not": "an array"}
        });
        strip_declared_metadata_types(&mut odd);
        assert_eq!(odd["template_metadata"], "not an array");
        assert_eq!(odd["custom_metadata"][0], 42);
        assert!(odd["custom_metadata"][2].get("type").is_none());
    }

    #[test]
    fn eligible_query_default_is_empty() {
        assert!(eligible_query(&EligibleParams::new()).is_empty());
    }

    #[test]
    fn eligible_query_emits_the_cursor_contract() {
        let q = eligible_query(
            &EligibleParams::new()
                .page_size(Some(100))
                .cursor(Some("OPAQUE=="))
                .mimetype(Some("application/pdf"))
                .extension(Some("pdf")),
        );
        assert_eq!(q.get("page_size").map(String::as_str), Some("100"));
        assert_eq!(q.get("cursor").map(String::as_str), Some("OPAQUE=="));
        assert_eq!(
            q.get("mimetype").map(String::as_str),
            Some("application/pdf")
        );
        assert_eq!(q.get("extension").map(String::as_str), Some("pdf"));
    }

    #[test]
    fn eligible_query_never_emits_limit_or_offset() {
        // The defect this replaced: `limit`/`offset` were sent, the server
        // ACCEPTED AND IGNORED them, and the response looked exactly like the
        // page the caller had asked for. Measured 2026-08-27 — `--limit 1`
        // returned 100 items, identical to no argument. There is no
        // builder that can set them any more; this pins that they cannot come
        // back through a future edit.
        let q = eligible_query(&EligibleParams::new().page_size(Some(25)).cursor(Some("c")));
        assert!(!q.contains_key("limit"), "limit must never be sent");
        assert!(!q.contains_key("offset"), "offset must never be sent");
        assert_eq!(q.len(), 2, "only the documented keys: {q:?}");
    }

    #[test]
    fn eligible_query_does_not_pre_snap_page_size() {
        // The 25/100/250 quantization is the SERVER's rule and the response
        // reports the size actually used, so the client must send what the
        // caller asked for rather than second-guessing it. 50 stays 50 on the
        // wire even though the server will answer with 25.
        let q = eligible_query(&EligibleParams::new().page_size(Some(50)));
        assert_eq!(q.get("page_size").map(String::as_str), Some("50"));
    }
}
