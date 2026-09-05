#![allow(clippy::missing_errors_doc)]

/// Storage API endpoints for workspace file and folder operations.
///
/// Maps to endpoints documented at `/current/workspace/{workspace_id}/storage/`.
use std::collections::HashMap;

use serde_json::Value;

use crate::api::types::SearchModeParams;
use crate::client::ApiClient;
use crate::error::CliError;

/// List files and folders in a workspace folder.
///
/// `GET /workspace/{workspace_id}/storage/{parent_id}/list/`
// One more parameter than the lint likes, and the alternative — a params
// struct for six positional values, five of which are already `Option` — would
// cost more clarity than it buys at the only two call sites.
#[allow(clippy::too_many_arguments)]
pub async fn list_files(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    parent_id: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(v) = sort_by {
        params.insert("sort_by".to_owned(), v.to_owned());
    }
    if let Some(v) = sort_dir {
        params.insert("sort_dir".to_owned(), v.to_owned());
    }
    if let Some(v) = page_size {
        params.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = cursor {
        params.insert("cursor".to_owned(), v.to_owned());
    }
    let path = format!(
        "/{}/{}/storage/{}/list/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get details for a specific storage node.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/details/`
pub async fn get_file_details(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/details/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Server-enforced cap on the number of node ids per bulk-details request.
///
/// Going over this returns HTTP 400 with code 115519. Callers with more
/// than this many ids must chunk on the client side.
pub const BULK_DETAILS_MAX_IDS: usize = 25;

/// Per-id error returned by the bulk-details endpoint.
///
/// The server echoes back the input casing of `node_id` (the input is
/// normalized internally but the error retains what the caller sent),
/// so callers matching results to inputs must compare case-insensitively.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NodeFetchError {
    /// Node id the error applies to (echoes input casing).
    pub node_id: String,
    /// Numeric API error code. Common values:
    /// - `191_878` invalid `OpaqueId` format / wrong type
    /// - `133_123` node does not exist (or belongs to another workspace)
    /// - `146_256` generic retrieval error (transient — safe to retry)
    /// - `146_950` node exists but its physical content is gone (not retryable)
    /// - `179_961` formatting failed (rare; report as bug)
    pub code: u32,
    /// Human-readable error message.
    pub message: String,
}

impl NodeFetchError {
    fn from_value(v: &Value) -> Self {
        let node_id = v.get("node_id").and_then(Value::as_str).map(str::to_owned);
        if node_id.is_none() {
            tracing::warn!(error_row = %v, "bulk-details error row missing node_id");
        }
        let code_raw = v.get("code");
        let code = code_raw
            .and_then(Value::as_u64)
            .and_then(|c| u32::try_from(c).ok());
        if code.is_none() && code_raw.is_some_and(|c| !c.is_null()) {
            tracing::warn!(code = ?code_raw, "bulk-details error row code not a u32");
        }
        let message = v.get("message").and_then(Value::as_str).map(str::to_owned);
        if message.is_none() {
            tracing::warn!(error_row = %v, "bulk-details error row missing message");
        }
        // When `node_id` is missing we emit an empty string rather
        // than a synthetic placeholder — a synthetic value would
        // round-trip through downstream tooling as if it were a real
        // id. Presentation-layer code is
        // responsible for rendering empty as "<no id>" or similar.
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
/// markdown sanitizer contract: the goal is to keep an
/// attacker-controlled `message` from clearing the screen, replaying
/// stored escape sequences, or visually spoofing surrounding text.
fn sanitize_terminal_string(s: &str) -> String {
    s.chars()
        .filter(|c| {
            // Allow common whitespace within messages; strip everything
            // else in C0 (`0x00..=0x1F` minus `\t`) and C1 (`0x7F..=0x9F`).
            if c.is_control() && *c != '\t' && *c != '\n' && *c != '\r' {
                return false;
            }
            let cp = *c as u32;
            // Bidi override / isolate / zero-width / BOM (U+FEFF).
            !matches!(
                cp,
                0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF
            )
        })
        .collect()
}

/// Bulk-details response: zero or more resolved nodes plus per-id errors.
///
/// Both HTTP 200 (≥1 id resolved) and HTTP 404 (all ids errored) carry
/// this same shape; partial results are normal and a non-empty `errors`
/// list at HTTP 200 must NOT be treated as a request-level failure.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BulkDetailsResponse {
    /// Successfully resolved nodes. Server does NOT preserve input order.
    pub nodes: Vec<Value>,
    /// Per-id errors. May be non-empty even at HTTP 200.
    pub errors: Vec<NodeFetchError>,
}

/// Get details for one or more storage nodes via the details endpoint.
///
/// `GET /workspace/{workspace_id}/storage/{id1},{id2},.../details/`
///
/// The server now annotates the response payload with a `format` field
/// (`"single"` or `"multi"`) so clients can normalize without inspecting
/// the URL. This function joins the input ids with literal commas and
/// returns a unified [`BulkDetailsResponse`]: single-format responses
/// surface their lone node as `nodes[0]`, multi-format responses pass
/// through `nodes[]` and `errors[]` as-is.
///
/// Constraints:
/// - 1..=`BULK_DETAILS_MAX_IDS` ids per call (callers needing more must chunk)
/// - All ids must belong to the same `workspace_id`
/// - Commas between ids must NOT be URL-encoded (the server splits on `,`)
///
/// Both HTTP 200 (some ok) and HTTP 404 (all errored) return a populated
/// [`BulkDetailsResponse`]; HTTP 400/5xx surface as `CliError::Api`.
pub async fn get_bulk_node_details(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_ids: &[String],
) -> Result<BulkDetailsResponse, CliError> {
    let path = build_bulk_details_path(context_type, workspace_id, node_ids)?;
    let (_status, body) = client.get_partial_envelope(&path).await?;
    parse_bulk_details_response(&body)
}

/// Build the bulk-details URL path. Extracted as a free function so
/// chunking and validation can be unit-tested without an HTTP client.
fn build_bulk_details_path(
    context_type: &str,
    workspace_id: &str,
    node_ids: &[String],
) -> Result<String, CliError> {
    if node_ids.is_empty() {
        return Err(CliError::Parse(
            "bulk node details requires at least one id".to_owned(),
        ));
    }
    if node_ids.len() > BULK_DETAILS_MAX_IDS {
        return Err(CliError::Parse(format!(
            "bulk node details accepts at most {BULK_DETAILS_MAX_IDS} ids per call (got {})",
            node_ids.len()
        )));
    }
    let encoded: Vec<String> = node_ids
        .iter()
        .map(|id| urlencoding::encode(id).into_owned())
        .collect();
    // The server distinguishes single vs bulk shape by the presence
    // of a comma in the URL segment. For chunks of exactly one id we
    // duplicate the id with a literal comma so the response always
    // arrives in multi shape (per the platform team's recommended
    // "uniform code path" pattern: server dedupes case-insensitively,
    // so this is still one lookup). Without this, a 1-id trailing
    // chunk in an N+1 chunked run hits the single-id endpoint, and
    // a server-side 4xx on that single id would abort the whole run
    // and discard the nodes accumulated in earlier chunks.
    let segment = if encoded.len() == 1 {
        format!("{0},{0}", encoded[0])
    } else {
        encoded.join(",")
    };
    Ok(format!(
        "/{}/{}/storage/{}/details/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        segment,
    ))
}

/// Parse the details response body into a unified [`BulkDetailsResponse`].
///
/// Branches on `payload.format`:
/// - `"multi"`: pass through `nodes[]` and `errors[]`.
/// - `"single"`: wrap `node` into `nodes[0]` (drop a `null` node).
/// - absent: defensive shape-sniffing — if `nodes` (array) or `errors`
///   (array) exist treat as multi (covers older server builds and
///   404-all-errored bodies); otherwise fall back to `single` shape
///   (legacy single-id endpoint contract per platform team).
/// - any other value: returns `CliError::Parse` rather than silently
///   dropping data.
///
/// Tolerates both `{result, response: {…}}` (the documented envelope) and
/// a flat `{…}` body, mirroring `single_call_upload`'s pre-fix tolerance.
///
/// Public so binary tests can construct a [`BulkDetailsResponse`] from
/// a JSON body without needing a public struct-literal constructor —
/// the type is `#[non_exhaustive]`.
pub fn parse_bulk_details_response(body: &Value) -> Result<BulkDetailsResponse, CliError> {
    let payload = body.get("response").unwrap_or(body);
    if !payload.is_object() {
        return Err(CliError::Parse(
            "bulk-details response payload is not a JSON object".to_owned(),
        ));
    }
    let format = payload.get("format").and_then(Value::as_str);
    let multi_shape = payload.get("nodes").is_some_and(Value::is_array)
        || payload.get("errors").is_some_and(Value::is_array);

    let treat_as_multi = match format {
        Some("multi") => true,
        Some("single") => false,
        None => multi_shape,
        Some(other) => {
            return Err(CliError::Parse(format!(
                "bulk-details response has unknown format {other:?}"
            )));
        }
    };

    if treat_as_multi {
        let nodes = payload
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let errors = payload
            .get("errors")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(NodeFetchError::from_value).collect())
            .unwrap_or_default();
        return Ok(BulkDetailsResponse { nodes, errors });
    }

    // "single" or absent-with-no-multi-shape: lift the lone node into
    // the unified shape, dropping `null` so it can't masquerade as a
    // resolved node downstream.
    let nodes = payload
        .get("node")
        .filter(|n| !n.is_null())
        .cloned()
        .map(|n| vec![n])
        .unwrap_or_default();
    Ok(BulkDetailsResponse {
        nodes,
        errors: Vec::new(),
    })
}

/// Create a new folder in workspace storage.
///
/// `POST /workspace/{workspace_id}/storage/{parent_id}/createfolder/`
pub async fn create_folder(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    parent_id: &str,
    name: &str,
    force: bool,
) -> Result<Value, CliError> {
    let form = create_folder_form(name, force);
    let path = format!(
        "/{}/{}/storage/{}/createfolder/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    client.post(&path, &form).await
}

/// Build the form body for [`create_folder`].
///
/// `force=true` always creates a new folder (auto-renamed on a name
/// collision); the default is idempotent (returns the existing folder).
fn create_folder_form(name: &str, force: bool) -> HashMap<String, String> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    if force {
        form.insert("force".to_owned(), "true".to_owned());
    }
    form
}

/// Move a storage node to a different parent folder.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/move/`
pub async fn move_node(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
    target_parent_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("parent".to_owned(), target_parent_id.to_owned());
    let path = format!(
        "/{}/{}/storage/{}/move/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Copy a storage node to a different parent folder.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/copy/`
pub async fn copy_node(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
    target_parent_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("parent".to_owned(), target_parent_id.to_owned());
    let path = format!(
        "/{}/{}/storage/{}/copy/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Update a storage node's name, content source, or metadata overrides.
///
/// `POST /{context_type}/{context_id}/storage/{node_id}/update/` (the
/// `workspace` and `share` twins share one wire contract). All fields are
/// optional, but the server requires at least one — callers should enforce
/// that before invoking. `from` is a JSON-encoded source object (same shape
/// as `addfile`: `{"type":"upload","upload":{"id":"…"}}` or
/// `{"type":"hash","hash":{"hash":"…","hash_type":"sha256"}}`); supplying it
/// replaces the file content and creates a new version. `metadata_title`
/// (max 50) and `metadata_short` (max 2048) override the node's title and
/// short description. Pass the literal string `"null"` (or `""`) in a field
/// to clear it, per the server contract.
///
/// `if_version_id` is a compare-and-swap precondition: the version id the update
/// was derived from. **Enforced** on this endpoint as of the 2026-08-24 deploy —
/// a stale base returns `409` with
/// `params[].reason = "conflict_version_mismatch"` and the current id.
///
/// Send the version actually READ (a pinned download), not one from a separate
/// lookup — the check is only that the id is CURRENT, so a base that is present
/// but wrong PASSES and overwrites the change it was meant to protect.
#[allow(clippy::too_many_arguments)]
pub async fn update_node(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    node_id: &str,
    name: Option<&str>,
    from: Option<&str>,
    metadata_title: Option<&str>,
    metadata_short: Option<&str>,
    if_version_id: Option<&str>,
) -> Result<Value, CliError> {
    let form = update_node_form(name, from, metadata_title, metadata_short, if_version_id);
    let path = format!(
        "/{}/{}/storage/{}/update/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Build the form body for [`update_node`] from the supplied `Some` fields.
fn update_node_form(
    name: Option<&str>,
    from: Option<&str>,
    metadata_title: Option<&str>,
    metadata_short: Option<&str>,
    if_version_id: Option<&str>,
) -> HashMap<String, String> {
    let mut form = HashMap::new();
    if let Some(v) = name {
        form.insert("name".to_owned(), v.to_owned());
    }
    if let Some(v) = from {
        form.insert("from".to_owned(), v.to_owned());
    }
    if let Some(v) = metadata_title {
        form.insert("metadata_title".to_owned(), v.to_owned());
    }
    if let Some(v) = metadata_short {
        form.insert("metadata_short".to_owned(), v.to_owned());
    }
    // Blank is absent: an empty precondition would be a base the server cannot
    // check, which is the "present but wrong" case this field exists to stop.
    if let Some(v) = if_version_id.map(str::trim).filter(|v| !v.is_empty()) {
        form.insert("if_version_id".to_owned(), v.to_owned());
    }
    form
}

/// Reject an `if_version_id` this endpoint cannot act on.
///
/// Blank is rejected and always will be: an empty precondition is not "no
/// precondition", it is a base the server could never check, and treating it as
/// absent turns a typo into an unguarded write. The server rejects it too
/// (`207092`) as of the 2026-08-24 deploy; this keeps the message local and
/// specific.
///
/// The precondition IS enforced by the server, so there is no "not enforced
/// here" refusal. Such a refusal would be warranted only if the endpoint did not
/// declare the field: the platform accepts undeclared fields, so an unenforced
/// precondition would be silently discarded and the write would land unguarded.
/// **Verified on the deployed platform, 2026-08-24, two arms with a positive
/// control:** a valid-but-STALE base returns `409` with
/// `params[].reason = "conflict_version_mismatch"` and `current_version_id`;
/// the CURRENT base succeeds. A local refusal would block a working, enforced
/// precondition.
///
/// Scope: the blank rule is UNIVERSAL — an empty precondition is meaningless on
/// any endpoint — so this is now applied on BOTH the attach endpoint
/// (`.../storage/{node}/update/`) and the one-step replace via an upload session
/// (`POST /upload/` `action=update`, the File Share write-back). The latter was
/// added 2026-08-25: `api/upload.rs` sends `if_version_id` whenever it is
/// `Some`, so a blank reached the wire and the outcome depended on server-side
/// validation the upload reference does not document (the storage reference
/// rejects it; the upload page is silent). Refusing locally is fail-safe under
/// either answer.
///
/// What this does NOT cover: a MISTYPED precondition. That cannot arise through
/// clap (which yields a `String`) and is handled on the JSON surface by
/// `optional_str_strict` — a non-string would otherwise read as ABSENT and slip
/// past this guard entirely, since it only ever sees `Some(..)`.
pub fn reject_unsupported_if_version(if_version: Option<&str>) -> Result<(), CliError> {
    let Some(raw) = if_version else {
        return Ok(());
    };
    if raw.trim().is_empty() {
        return Err(CliError::Parse(
            "--if-version was given an empty value. Pass the version id the update was \
             derived from, or omit the flag entirely."
                .to_owned(),
        ));
    }
    Ok(())
}

/// Rename a storage node in a workspace (thin wrapper over [`update_node`]).
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/update/`
pub async fn rename_node(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
    new_name: &str,
) -> Result<Value, CliError> {
    update_node(
        client,
        context_type,
        workspace_id,
        node_id,
        Some(new_name),
        None,
        None,
        None,
        None,
    )
    .await
}

/// Move a storage node to trash (soft delete).
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/delete/`
pub async fn delete_node(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/delete/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.delete(&path).await
}

/// Restore a node from trash.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/restore/`
pub async fn restore_node(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/{}/{}/storage/{}/restore/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Permanently delete a trashed node.
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/purge/`
pub async fn purge_node(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/purge/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.delete(&path).await
}

/// List items in the trash folder.
///
/// Uses the list endpoint with `trash` as the `parent_id`.
/// `GET /workspace/{workspace_id}/storage/trash/list/`
pub async fn list_trash(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    list_files(
        client,
        context_type,
        workspace_id,
        "trash",
        sort_by,
        sort_dir,
        page_size,
        cursor,
    )
    .await
}

/// List versions of a storage node.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/versions/`
pub async fn list_versions(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/versions/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Parameters for the keyword/semantic file-search endpoint
/// (`GET .../storage/search/`).
///
/// This is the **single** builder for the flat filename/content search leg,
/// shared by `files search` (`--workspace` and `--share`) and the MCP
/// `files search` action (`workspace_id` / `share_id`). No second builder for
/// this endpoint exists (the former `api::workspace::search_workspace`
/// forwarded here before it was removed).
///
/// The same parameter set serves both profile types; only the assembled path
/// differs ([`search_files`] vs [`search_files_share`]). `filters` is
/// workspace-only **server-side** — a share accepts the request and answers
/// `200` with UNFILTERED results and no `metadata_filter` block, so callers
/// must check for that acknowledgement rather than trust the status code.
///
/// This is NOT the primary search surface. `/storage/search/` returns a flat
/// node list matched on filename and file content; the unified
/// [`crate::api::search`] family (`/search/`) is the full search — one query
/// across `files`, `metadata`, and `comments` buckets. Reach for that first
/// and use this only when a single flat file list is what you want.
///
/// Pagination uses `limit`/`offset` — **not** `page_size`/`cursor` (the
/// search endpoint does not document keyset pagination, unlike the storage
/// LIST endpoint). `files_scope`/`folders_scope` narrow the indexed set;
/// `details=true` enriches each hit with the full node resource (the server
/// then caps the default `limit` to 10); `output` selects the verbosity of
/// the `content_snippet` field (`terse`/`standard`/`full`).
///
/// The published API docs describe two shapes for this endpoint: the canonical
/// `search` + `output` form with a `files` MAP response, and the same endpoint
/// accepting `limit`, `offset`, `files_scope`, `folders_scope`, and `details`
/// with a `results[]` response. Both response shapes are handled defensively by
/// the renderer.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SearchFilesParams<'a> {
    /// Comma-separated `nodeId:versionId` pairs (max 100) narrowing the
    /// searched files.
    pub files_scope: Option<&'a str>,
    /// Comma-separated `nodeId:depth` pairs (max 100) narrowing the searched
    /// folders.
    pub folders_scope: Option<&'a str>,
    /// Maximum number of results (1-500; server caps to 10 when `details`).
    pub limit: Option<u32>,
    /// Result offset for pagination.
    pub offset: Option<u32>,
    /// When `true`, each hit is enriched with the full node resource.
    pub details: Option<bool>,
    /// `content_snippet` verbosity: `terse`, `standard`, or `full`.
    pub output: Option<&'a str>,
    /// Metadata-predicate filter — a JSON array of `{field, operator, value}`
    /// clause objects, AND-combined (**workspace routes only**). Sent verbatim
    /// as the `filters` query parameter.
    ///
    /// The caller is responsible for the response-side honesty check: the
    /// server acknowledges a filter that actually ran with a **top-level**
    /// `metadata_filter` block, and **its absence means the results are
    /// unfiltered** (a proxy stripped the parameter, or the deployment does not
    /// accept it yet). See
    /// [`crate::api::storage::metadata_filter_block`].
    pub filters: Option<&'a str>,
    /// `search_in` / `name_match` / `case_sensitive`. All optional; omitting
    /// every one reproduces today's query and ranking exactly.
    pub modes: SearchModeParams,
}

impl<'a> SearchFilesParams<'a> {
    /// An empty parameter set (all fields unset). Equivalent to
    /// [`Default::default`]; provided so callers in other crates can build the
    /// `#[non_exhaustive]` struct without struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `files_scope` (comma-separated `nodeId:versionId` pairs).
    #[must_use]
    pub fn files_scope(mut self, v: Option<&'a str>) -> Self {
        self.files_scope = v;
        self
    }

    /// Set `folders_scope` (comma-separated `nodeId:depth` pairs).
    #[must_use]
    pub fn folders_scope(mut self, v: Option<&'a str>) -> Self {
        self.folders_scope = v;
        self
    }

    /// Set the result `limit`.
    #[must_use]
    pub fn limit(mut self, v: Option<u32>) -> Self {
        self.limit = v;
        self
    }

    /// Set the result `offset`.
    #[must_use]
    pub fn offset(mut self, v: Option<u32>) -> Self {
        self.offset = v;
        self
    }

    /// Set `details` (full-node enrichment).
    #[must_use]
    pub fn details(mut self, v: bool) -> Self {
        self.details = v.then_some(true);
        self
    }

    /// Set the `content_snippet` verbosity (`output`).
    #[must_use]
    pub fn output(mut self, v: Option<&'a str>) -> Self {
        self.output = v;
        self
    }

    /// Set `filters` (a JSON array of metadata predicate clauses).
    ///
    /// The value is forwarded **verbatim** — deliberately not re-serialized, so
    /// a large integer in a clause cannot lose precision on a round trip
    /// (see the published API docs).
    ///
    /// No client-side check is made for the clause limit, the operator set, the
    /// field vocabulary, or the current refusal to combine `filters` with
    /// `folders_scope`: those are **server policy that the contract explicitly
    /// flags as changeable** ("handle the refusal as a condition rather than
    /// building on it as an invariant"). Encoding them
    /// here would make the CLI refuse a request the platform had started
    /// accepting. Shape is validated at the command boundary; policy is left to
    /// the server.
    #[must_use]
    pub fn filters(mut self, v: Option<&'a str>) -> Self {
        self.filters = v;
        self
    }

    /// Set the search-mode parameters (`search_in` / `name_match` /
    /// `case_sensitive`).
    #[must_use]
    pub fn modes(mut self, v: SearchModeParams) -> Self {
        self.modes = v;
        self
    }

    /// Build the query-parameter map (excluding `search`, added by the caller).
    fn into_query(self, query: &str) -> HashMap<String, String> {
        let mut params = HashMap::new();
        params.insert("search".to_owned(), query.to_owned());
        if let Some(v) = self.files_scope {
            params.insert("files_scope".to_owned(), v.to_owned());
        }
        if let Some(v) = self.folders_scope {
            params.insert("folders_scope".to_owned(), v.to_owned());
        }
        if let Some(v) = self.limit {
            params.insert("limit".to_owned(), v.to_string());
        }
        if let Some(v) = self.offset {
            params.insert("offset".to_owned(), v.to_string());
        }
        if let Some(true) = self.details {
            params.insert("details".to_owned(), "true".to_owned());
        }
        if let Some(v) = self.output {
            params.insert("output".to_owned(), v.to_owned());
        }
        if let Some(v) = self.filters {
            params.insert("filters".to_owned(), v.to_owned());
        }
        self.modes.apply(&mut params);
        params
    }
}

/// Locate the **top-level** `metadata_filter` acknowledgement block on a
/// `/storage/search/` response.
///
/// The server emits this block **only when a `filters` value actually reached it
/// and ran**, which makes its *presence* — not any field inside it — the signal
/// that a result set is genuinely filtered (see the published API docs). A request
/// that believed it sent `filters` but gets no block back is holding
/// **unfiltered** results: either an intermediary stripped the unrecognised
/// query parameter, or the deployment does not accept `filters` yet. Both come
/// back `200` with a plausible list of files, so nothing else on the response
/// distinguishes them.
///
/// It sits beside `search_metadata`, not inside it — `search_metadata.scoped`
/// reports only `files_scope`/`folders_scope`, so `scoped: false` next to a
/// present `metadata_filter` is normal and is **not** evidence the filter was
/// ignored.
///
/// **Top level only, deliberately.** The unified `/search/` endpoint nests its
/// capability report at `buckets.files.search_metadata`, so a nested
/// `metadata_filter` twin looks like the obvious companion lookup — but that
/// endpoint "has no `files_scope` / `folders_scope` parameters" (per the
/// published API docs) and no `filters` parameter either, so no such block is
/// ever emitted there. Probing for one would be an invented shape, and because
/// this function's `None` is what raises the "results are unfiltered" warning,
/// a speculative second path is exactly where a silent false negative would
/// hide.
#[must_use]
pub fn metadata_filter_block(value: &Value) -> Option<&Value> {
    value.get("metadata_filter").filter(|v| !v.is_null())
}

/// Search for files in a workspace (keyword + semantic).
///
/// `GET /workspace/{workspace_id}/storage/search/?search=<query>`
///
/// THE single file-search builder (see [`SearchFilesParams`]). The response is
/// either a `files` MAP keyed by node id (keyword-only / canonical
/// storage.txt shape) or a `results[]` array with `pagination` (the
/// `details`/ai.txt shape); both are rendered defensively downstream.
pub async fn search_files(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    query: &str,
    params: SearchFilesParams<'_>,
) -> Result<Value, CliError> {
    params.modes.validate(query)?;
    let query_params = params.into_query(query);
    let path = format!(
        "/{}/{}/storage/search/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
    );
    client.get_with_params(&path, &query_params).await
}

/// Search for files in a share (keyword + semantic).
///
/// `GET /share/{share_id}/storage/search/?search=<query>`
///
/// Same contract as [`search_files`] but share-scoped — backs
/// `fastio files search --share` and the MCP `files search` `share_id` leg.
///
/// Error codes on this route, measured on the wire and confirmed from backend
/// source (2026-08-29):
///
/// | `error.code` | condition |
/// |------|---------|
/// | `139420` | workspace-backed (folder) share — search unavailable here |
/// | `179101` | orphaned share ("this shared folder no longer exists") |
///
/// `179101` is this route's spelling of the orphaned condition; the unified
/// `/share/{id}/search/` route uses `142794` for the same thing. Both are a
/// **different** condition from `139420` and must not be conflated with it.
/// Note the docs' error tables render the class as `1609 (Not Found)`, which is
/// an HTTP-status class and never appears in `error.code`.
///
/// `filters` is **workspace-only server-side**: a share answers `200` with
/// unfiltered results and no `metadata_filter` acknowledgement rather than
/// refusing, so a caller that sends it must check for that block.
pub async fn search_files_share(
    client: &ApiClient,
    share_id: &str,
    query: &str,
    params: SearchFilesParams<'_>,
) -> Result<Value, CliError> {
    params.modes.validate(query)?;
    let query_params = params.into_query(query);
    let path = format!("/share/{}/storage/search/", urlencoding::encode(share_id),);
    client.get_with_params(&path, &query_params).await
}

/// Normalize a `/storage/search/` response so every output format renders one
/// ROW PER FILE.
///
/// The keyword-only search response (the canonical `storage.txt` shape, and the
/// shape `search_in=filename` always produces since it skips the semantic leg)
/// returns a top-level `files` value that is a **MAP keyed by node `OpaqueId`**
/// — e.g. `{"files": {"f3jm5-…": {"name": …}}}`. Note that "keyword-only" is not
/// the same as "intelligence disabled": instance intelligence gates the semantic
/// half of a content search only, so summaries indexed earlier remain searchable
/// with it off.
/// Rendered as-is, table/CSV would collapse it to a single row whose columns
/// are node ids, and markdown would emit a nested bullet list — neither of
/// which is one-row-per-file. This rewrites that map IN PLACE into an ARRAY of
/// records, hoisting each node id into both an `id` and a `node_id` field
/// (the id only ever lives in the map key, never inside the value), and
/// preserving insertion order so renderers produce a stable table.
///
/// The `details`/`ai.txt` shape (a `results[]` array with `pagination`) and
/// any already-array `files` value are left untouched, as is any non-object
/// envelope. This is the SCOPED counterpart to the generic
/// [`crate::output::flatten_response`]: the files-map → rows transform lives
/// on the search path only, so a future endpoint that legitimately returns a
/// top-level `files` object is never silently restructured.
///
/// Every [`search_files`] caller (`fastio files search` and the MCP
/// `files search` action) runs its response through this before rendering.
#[must_use]
pub fn normalize_search_response(mut value: Value) -> Value {
    let Some(map) = value.as_object_mut() else {
        return value;
    };
    // Only the MAP-shaped `files` value is rewritten; an array (or absent
    // key) is left as the server sent it.
    let Some(Value::Object(files_map)) = map.get("files") else {
        return value;
    };
    let mut rows = Vec::with_capacity(files_map.len());
    for (node_id, node_val) in files_map {
        let mut row = match node_val {
            Value::Object(obj) => obj.clone(),
            // Defensive: a non-object value still surfaces as a row carrying
            // just its id and the raw value, rather than being dropped.
            other => {
                let mut m = serde_json::Map::new();
                m.insert("value".to_owned(), other.clone());
                m
            }
        };
        row.insert("node_id".to_owned(), Value::String(node_id.clone()));
        row.insert("id".to_owned(), Value::String(node_id.clone()));
        rows.push(Value::Object(row));
    }
    map.insert("files".to_owned(), Value::Array(rows));
    value
}

/// List recently accessed files.
///
/// `GET /workspace/{workspace_id}/storage/recent/`
pub async fn list_recent(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
    node_type: Option<&str>,
) -> Result<Value, CliError> {
    let params = recent_query(page_size, cursor, node_type);
    let path = format!(
        "/{}/{}/storage/recent/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Build the query map for [`list_recent`]. `node_type` filters by node type
/// (`file`, `folder`, `link`, `note`).
fn recent_query(
    page_size: Option<u32>,
    cursor: Option<&str>,
    node_type: Option<&str>,
) -> HashMap<String, String> {
    let mut params = HashMap::new();
    if let Some(v) = page_size {
        params.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = cursor {
        params.insert("cursor".to_owned(), v.to_owned());
    }
    if let Some(v) = node_type {
        params.insert("type".to_owned(), v.to_owned());
    }
    params
}

/// Add a share link to a folder.
///
/// `POST /workspace/{workspace_id}/storage/{parent_id}/addlink/` — the node id
/// is the URL path part; the body carries `type=share` (the only accepted
/// link-target type) and `share=<share_id>`. The server reads the link-target
/// type under the wire key `type` (NOT `link_target_type`) and the target id
/// under `share`.
pub async fn add_link(
    client: &ApiClient,
    workspace_id: &str,
    parent_id: &str,
    share_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("type".to_owned(), "share".to_owned());
    form.insert("share".to_owned(), share_id.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/addlink/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    client.post(&path, &form).await
}

/// Add a file to a folder from a completed upload or by content-hash dedup.
///
/// `POST /{context_type}/{context_id}/storage/{parent_id}/addfile/` (the
/// `workspace` and `share` twins share one wire contract). `from` is a
/// JSON-encoded source object: either
/// `{"type":"upload","upload":{"id":"<upload_id>"}}` to attach a completed
/// upload session, or `{"type":"hash","hash":{"hash":"<hash>","hash_type":"sha256"}}`
/// to deduplicate against an existing object by content hash (instant add when
/// the content already exists server-side).
pub async fn add_file(
    client: &ApiClient,
    context_type: &str,
    context_id: &str,
    parent_id: &str,
    name: &str,
    from: &str,
) -> Result<Value, CliError> {
    let form = add_file_form(name, from);
    let path = format!(
        "/{}/{}/storage/{}/addfile/",
        urlencoding::encode(context_type),
        urlencoding::encode(context_id),
        urlencoding::encode(parent_id),
    );
    client.post(&path, &form).await
}

/// Build the form body for [`add_file`] (`name` + the JSON-encoded `from`).
fn add_file_form(name: &str, from: &str) -> HashMap<String, String> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    form.insert("from".to_owned(), from.to_owned());
    form
}

/// Transfer a node to another workspace.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/transfer/`
pub async fn transfer_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    target_workspace_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "target_workspace_id".to_owned(),
        target_workspace_id.to_owned(),
    );
    let path = format!(
        "/workspace/{}/storage/{}/transfer/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Restore a specific version of a file.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/restore-version/` — the
/// node id is the URL path part and `version_id` is a POST **body** field (NOT
/// a `/versions/{id}/restore/` path segment).
pub async fn version_restore(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
    version_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("version_id".to_owned(), version_id.to_owned());
    let path = format!(
        "/{}/{}/storage/{}/restore-version/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Acquire a file lock.
///
/// `POST /{context_type}/{context_id}/storage/{node_id}/lock/` — optional
/// `duration` (60-3600 seconds) sets the lock lifetime and `client_info`
/// (a JSON object, e.g. `{"device_name":"…","client_version":"…"}`) records
/// client metadata.
pub async fn lock_acquire(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
    duration: Option<u32>,
    client_info: Option<&str>,
) -> Result<Value, CliError> {
    crate::api::locking::lock_acquire(
        client,
        context_type,
        workspace_id,
        node_id,
        duration,
        client_info,
    )
    .await
}

/// Build the form body for an acquire-lock request. `duration` (60-3600 s)
/// sets the lock lifetime; `client_info` is a JSON object of client metadata.
pub(crate) fn lock_acquire_form(
    duration: Option<u32>,
    client_info: Option<&str>,
) -> HashMap<String, String> {
    let mut form = HashMap::new();
    if let Some(d) = duration {
        form.insert("duration".to_owned(), d.to_string());
    }
    if let Some(ci) = client_info {
        form.insert("client_info".to_owned(), ci.to_owned());
    }
    form
}

/// Check lock status.
///
/// `GET /{context_type}/{context_id}/storage/{node_id}/lock/`
pub async fn lock_status(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    crate::api::locking::lock_status(client, context_type, workspace_id, node_id).await
}

/// Release a file lock.
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/lock/`
///
/// The `lock_token` is the token returned by `lock_acquire` and must be
/// provided to prove ownership of the lock.
///
/// This is the workspace-context specialization of
/// [`crate::api::locking::lock_release`] and delegates to it, so the token
/// delivery (a query parameter — the server does not read a `DELETE` body
/// here) is defined in exactly one place.
pub async fn lock_release(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    lock_token: &str,
) -> Result<Value, CliError> {
    crate::api::locking::lock_release(client, "workspace", workspace_id, node_id, lock_token).await
}

/// Read a file's content as UTF-8 text.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/read/`
///
/// Returns `{ "node_id": <id>, "content": <text> }`.
///
/// Three different routes return "the content" of a node and they are not
/// interchangeable:
///
/// - `/read/` — the file's **raw bytes** as text (this fn / [`read_raw`]).
/// - `/readnote/` — a Note node's **markdown source**
///   ([`crate::api::workspace::read_note`], used by `fastio view`).
/// - `/content/` — the platform's **extracted, chunked text**, the same text it
///   indexed for search and AI ([`read_content_chunks`], and
///   [`read_content_many`] across several files). This is the route that reads
///   a PDF's words; `/read/` would hand back the PDF container.
pub async fn read_content(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let content = read_raw(client, context_type, workspace_id, node_id, None).await?;
    Ok(serde_json::json!({ "node_id": node_id, "content": content }))
}

/// Read raw file bytes as text.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/read/`
///
/// Returns the node's raw content as a UTF-8 string (for a `.md` file, the
/// markdown source). This is the binary `/read/` endpoint surfaced as text,
/// used by `fastio view` as the fallback for raw `.md` files that are not Note
/// nodes, and by [`read_content`] (which wraps it as JSON). An optional
/// `version_id` reads a specific version.
pub async fn read_raw(
    client: &ApiClient,
    context_type: &str,
    workspace_id: &str,
    node_id: &str,
    version_id: Option<&str>,
) -> Result<String, CliError> {
    let path = format!(
        "/{}/{}/storage/{}/read/",
        urlencoding::encode(context_type),
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    let params = version_id.map(|v| {
        let mut p = HashMap::new();
        p.insert("version_id".to_owned(), v.to_owned());
        p
    });
    client.get_raw_text(&path, params.as_ref()).await
}

// ─── Extracted text (`/content/`) ────────────────────────────────────────────

/// Maximum length of a `/content/` relevance query (`q`), in Unicode **code
/// points** — counted with `chars().count()`, never `len()`. The contract
/// states the bound as "1-512 characters".
pub const CONTENT_QUERY_MAX_LEN: usize = 512;

/// Exclusive ceiling on a `chunk_from` / `chunk_to` position.
///
/// A position at or beyond this cannot be addressed by a range window; a caller
/// walking a large file continues with `cursor`, which has no such limit.
pub const CONTENT_POSITION_LIMIT: u32 = 10_000;

/// Smallest accepted `limit` (chunks per response) on the content routes.
pub const CONTENT_LIMIT_MIN: u32 = 1;

/// Largest accepted `limit` (chunks per response) on the content routes.
pub const CONTENT_LIMIT_MAX: u32 = 20;

/// Smallest accepted `max_bytes` (UTF-8 byte budget over the emitted text).
pub const CONTENT_MAX_BYTES_MIN: u32 = 1024;

/// Largest accepted `max_bytes` (UTF-8 byte budget over the emitted text).
pub const CONTENT_MAX_BYTES_MAX: u32 = 262_144;

/// Accepted `output` verbosity tokens on the content routes.
///
/// The published contract also allows composing a `markdown` modifier onto an
/// `output` value on every storage endpoint (`output=standard,markdown`). That
/// composition is deliberately NOT expressible here: the CLI and the MCP server
/// render markdown locally through `crate::output::markdown`, so a server-side
/// markdown body would bypass the formatters entirely.
pub const CONTENT_OUTPUT_VALUES: [&str; 3] = ["terse", "standard", "full"];

/// Maximum number of node ids in one multi-file content read (`nodes`).
pub const CONTENT_MANY_MAX_NODES: usize = 10;

/// Validate a content-route `q` value (1-[`CONTENT_QUERY_MAX_LEN`] characters).
///
/// `label` names the parameter in the error message so the single-file route
/// (where `q` is optional) and the multi-file route (where it is required) can
/// share one implementation.
fn validate_content_query(query: &str, label: &str) -> Result<(), CliError> {
    if query.trim().is_empty() {
        return Err(CliError::Parse(format!("{label} must not be empty")));
    }
    let len = query.chars().count();
    if len > CONTENT_QUERY_MAX_LEN {
        return Err(CliError::Parse(format!(
            "{label} must be at most {CONTENT_QUERY_MAX_LEN} characters (got {len})"
        )));
    }
    Ok(())
}

/// Validate the shared `limit` / `max_bytes` / `output` bounds carried by both
/// content routes. Extracted so the two parameter structs cannot drift.
fn validate_content_bounds(
    limit: Option<u32>,
    max_bytes: Option<u32>,
    output: Option<&str>,
) -> Result<(), CliError> {
    if let Some(v) = limit
        && !(CONTENT_LIMIT_MIN..=CONTENT_LIMIT_MAX).contains(&v)
    {
        return Err(CliError::Parse(format!(
            "limit must be between {CONTENT_LIMIT_MIN} and {CONTENT_LIMIT_MAX} (got {v})"
        )));
    }
    if let Some(v) = max_bytes
        && !(CONTENT_MAX_BYTES_MIN..=CONTENT_MAX_BYTES_MAX).contains(&v)
    {
        return Err(CliError::Parse(format!(
            "max_bytes must be between {CONTENT_MAX_BYTES_MIN} and {CONTENT_MAX_BYTES_MAX} \
             (got {v})"
        )));
    }
    if let Some(v) = output
        && !CONTENT_OUTPUT_VALUES.contains(&v)
    {
        return Err(CliError::Parse(format!(
            "output must be one of {} (got `{v}`)",
            CONTENT_OUTPUT_VALUES.join(", "),
        )));
    }
    Ok(())
}

/// Parameters for the single-file extracted-text read
/// (`GET .../storage/{node_id}/content/`).
///
/// Every field is optional. At most **one** window selector may be set — `q`,
/// `page`, or the `chunk_from`/`chunk_to` range — and with none the response
/// starts at the beginning of the file. [`Self::validate`] enforces that and
/// every documented bound client-side, so a mistake surfaces as a readable
/// message instead of a `406`/`1605` from the server.
///
/// The unit is a **chunk, not a page**: text is chunked for retrieval and a
/// chunk can span two pages or split one, so `page` returns the whole chunks
/// that OVERLAP that page. A chunk's address is its `position`, which is what
/// `chunk_from`/`chunk_to` select on; `chunk_index` is the legacy spelling of
/// that idea and is nullable, and `sequence` is a correlation coordinate, not an
/// address. Read `page_addressable` before sending a `page` — formats with no
/// pages (spreadsheets, plain text, code, notes) answer an empty `chunks` list.
///
/// `cursor` is opaque: pass back the `next_cursor` from the MOST RECENT
/// response verbatim, never a value built by the caller and never one stored
/// from an earlier walk. It carries the file version it was issued against, so
/// a walk can never straddle two versions — if the file is replaced mid-walk
/// the next call returns an empty `chunks` list with `next_cursor: null`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ContentReadParams<'a> {
    /// `q` — relevance mode. Rank this file's own chunks by keyword match and
    /// return the best ones, each with full text and a numeric `score`.
    /// 1-[`CONTENT_QUERY_MAX_LEN`] characters. Cannot be combined with `page`,
    /// `chunk_from`/`chunk_to`, or `cursor`, and defaults `limit` to 3.
    pub query: Option<&'a str>,
    /// `page` — return the chunks whose `[start_page, end_page]` range overlaps
    /// this 1-based page.
    pub page: Option<u32>,
    /// `chunk_from` — start of an inclusive `position` range. Must be below
    /// [`CONTENT_POSITION_LIMIT`].
    pub chunk_from: Option<u32>,
    /// `chunk_to` — end of that inclusive `position` range. Requires
    /// `chunk_from`, must be greater than or equal to it, and is subject to the
    /// same ceiling.
    pub chunk_to: Option<u32>,
    /// `cursor` — opaque continuation token from the previous response's
    /// `next_cursor`. Not valid with `q`.
    pub cursor: Option<&'a str>,
    /// `limit` — chunks per response
    /// ([`CONTENT_LIMIT_MIN`]-[`CONTENT_LIMIT_MAX`]; server default 5, or 3 in
    /// relevance mode).
    pub limit: Option<u32>,
    /// `max_bytes` — UTF-8 byte budget over the text in one response, applied in
    /// the ordered modes only
    /// ([`CONTENT_MAX_BYTES_MIN`]-[`CONTENT_MAX_BYTES_MAX`]; server default
    /// 32768). Deliberately not applied in relevance mode, so a hit is never
    /// silently dropped.
    pub max_bytes: Option<u32>,
    /// `output` — one of [`CONTENT_OUTPUT_VALUES`]. `terse` returns the chunk
    /// map with no `text`.
    pub output: Option<&'a str>,
}

impl<'a> ContentReadParams<'a> {
    /// An empty parameter set (no window selector, server defaults for
    /// everything). Equivalent to [`Default::default`]; provided so callers in
    /// other crates can build the `#[non_exhaustive]` struct without
    /// struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `q` (relevance mode).
    #[must_use]
    pub fn query(mut self, v: Option<&'a str>) -> Self {
        self.query = v;
        self
    }

    /// Set `page` (1-based).
    #[must_use]
    pub fn page(mut self, v: Option<u32>) -> Self {
        self.page = v;
        self
    }

    /// Set the inclusive `chunk_from`/`chunk_to` `position` range.
    #[must_use]
    pub fn chunks(mut self, from: Option<u32>, to: Option<u32>) -> Self {
        self.chunk_from = from;
        self.chunk_to = to;
        self
    }

    /// Set `cursor` (an opaque `next_cursor` from the most recent response).
    #[must_use]
    pub fn cursor(mut self, v: Option<&'a str>) -> Self {
        self.cursor = v;
        self
    }

    /// Set `limit` (chunks per response).
    #[must_use]
    pub fn limit(mut self, v: Option<u32>) -> Self {
        self.limit = v;
        self
    }

    /// Set `max_bytes` (UTF-8 byte budget over the emitted text).
    #[must_use]
    pub fn max_bytes(mut self, v: Option<u32>) -> Self {
        self.max_bytes = v;
        self
    }

    /// Set `output` (chunk verbosity).
    #[must_use]
    pub fn output(mut self, v: Option<&'a str>) -> Self {
        self.output = v;
        self
    }

    /// Reject, client-side, every window combination the server answers with
    /// `1605 (Invalid Input)` / `406`.
    ///
    /// Both the CLI and the MCP server call this before sending, so the message
    /// text is user-facing and names the offending wire parameters (`q`,
    /// `page`, `chunk_from`, `chunk_to`, `cursor`, `limit`, `max_bytes`,
    /// `output`) rather than any one front end's flag spelling.
    ///
    /// # Errors
    /// [`CliError::Parse`] when more than one window selector is set, when `q`
    /// is combined with `cursor`, when `chunk_to` is given without
    /// `chunk_from`, when `chunk_from` exceeds `chunk_to`, when either range
    /// endpoint is at or beyond [`CONTENT_POSITION_LIMIT`], when `page` is `0`,
    /// or when `q`, `limit`, `max_bytes` or `output` is outside its documented
    /// bounds.
    pub fn validate(&self) -> Result<(), CliError> {
        let mut selectors: Vec<&str> = Vec::new();
        if self.query.is_some() {
            selectors.push("q");
        }
        if self.page.is_some() {
            selectors.push("page");
        }
        if self.chunk_from.is_some() || self.chunk_to.is_some() {
            selectors.push("chunk_from/chunk_to");
        }
        if selectors.len() > 1 {
            return Err(CliError::Parse(format!(
                "at most one content window may be selected, but {} were given — \
                 choose q (relevance), page, or chunk_from/chunk_to (positions)",
                selectors.join(" and "),
            )));
        }
        if self.query.is_some() && self.cursor.is_some() {
            return Err(CliError::Parse(
                "q cannot be combined with cursor — relevance mode returns one ranked \
                 page, not a walk"
                    .to_owned(),
            ));
        }
        if self.chunk_to.is_some() && self.chunk_from.is_none() {
            return Err(CliError::Parse(
                "chunk_to requires chunk_from — a range needs both endpoints".to_owned(),
            ));
        }
        if let (Some(from), Some(to)) = (self.chunk_from, self.chunk_to)
            && from > to
        {
            return Err(CliError::Parse(format!(
                "chunk_from ({from}) must not be greater than chunk_to ({to})"
            )));
        }
        for (name, position) in [("chunk_from", self.chunk_from), ("chunk_to", self.chunk_to)] {
            if let Some(p) = position
                && p >= CONTENT_POSITION_LIMIT
            {
                return Err(CliError::Parse(format!(
                    "{name} must be below {CONTENT_POSITION_LIMIT} (got {p}) — continue \
                     further into a large file with cursor instead"
                )));
            }
        }
        if let Some(0) = self.page {
            return Err(CliError::Parse(
                "page is 1-based and must be at least 1".to_owned(),
            ));
        }
        if let Some(q) = self.query {
            validate_content_query(q, "q")?;
        }
        validate_content_bounds(self.limit, self.max_bytes, self.output)
    }

    /// Build the query-parameter map. Only the parameters the caller actually
    /// set are emitted, so the server applies its own documented defaults to
    /// the rest.
    fn to_query(&self) -> HashMap<String, String> {
        let mut params = HashMap::new();
        if let Some(v) = self.query {
            params.insert("q".to_owned(), v.to_owned());
        }
        if let Some(v) = self.page {
            params.insert("page".to_owned(), v.to_string());
        }
        if let Some(v) = self.chunk_from {
            params.insert("chunk_from".to_owned(), v.to_string());
        }
        if let Some(v) = self.chunk_to {
            params.insert("chunk_to".to_owned(), v.to_string());
        }
        if let Some(v) = self.cursor {
            params.insert("cursor".to_owned(), v.to_owned());
        }
        if let Some(v) = self.limit {
            params.insert("limit".to_owned(), v.to_string());
        }
        if let Some(v) = self.max_bytes {
            params.insert("max_bytes".to_owned(), v.to_string());
        }
        if let Some(v) = self.output {
            params.insert("output".to_owned(), v.to_owned());
        }
        params
    }
}

/// Read a file's or note's **extracted text** as ordered chunks.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/content/`
/// `GET /share/{share_id}/storage/{node_id}/content/`
///
/// This is the same text the platform already extracted and indexed for search
/// and AI, which makes it the route that lets a caller actually read a PDF's
/// words — [`read_raw`] hands back the raw bytes and search returns only a
/// snippet. `context_type` is `"workspace"` or `"share"`, matching
/// [`read_content`] and [`list_files`].
///
/// Returns the response object verbatim after the envelope unwrap: `node_id`,
/// `name`, `mimetype`, `indexed`, `complete`, `indexed_version_id`,
/// `page_addressable`, `num_pages`, `total_chunks`, `chunks[]` (`position`,
/// `sequence`, `chunk_index`, `start_page`, `end_page`, `chars`, `score`,
/// `text`), `next_cursor` and `truncated`.
///
/// **`indexed: false` is a normal `200`, not an error** — the version has no
/// text in the index (never processed, still queued, or a format carrying no
/// extractable text). It is always a statement about the file and never about
/// the platform, because a failure to read the index is a `500`.
///
/// **Auth.** View permission on the workspace. The share path requires
/// **download**, not view — extracted text is the interior of the file, so a
/// guest who may not fetch the bytes may not read the text either.
///
/// Error codes (see the published API docs at
/// `https://api.fast.io/current/llms/full/`):
///
/// | class | HTTP | condition |
/// |-------|------|-----------|
/// | `1609 (Not Found)` | 404 | node not found, or it is in the trash |
/// | `1605 (Invalid Input)` | 406 | node is a folder or a link — only files and notes carry text |
/// | `1605 (Invalid Input)` | 406 | window parameters conflict or are out of range (pre-empted by [`ContentReadParams::validate`], except for a stale or out-of-range `cursor`, which only the server can judge) |
/// | `1680 (Access Denied)` | 401 | share caller has no download permission, or the file is virus-flagged |
/// | `1652 (Resource Not Found)` | 404 | the file's content is no longer available |
/// | `1654 (Internal Error)` | 500 | content temporarily unavailable — **retry**; never treat this as "the file has no text" |
///
/// Those classes are HTTP-status classes, not `error.code` values (the same
/// distinction called out on [`search_files_share`]).
pub async fn read_content_chunks(
    client: &ApiClient,
    context_type: &str,
    profile_id: &str,
    node_id: &str,
    params: &ContentReadParams<'_>,
) -> Result<Value, CliError> {
    params.validate()?;
    let query = params.to_query();
    let path = format!(
        "/{}/{}/storage/{}/content/",
        urlencoding::encode(context_type),
        urlencoding::encode(profile_id),
        urlencoding::encode(node_id),
    );
    client.get_with_params(&path, &query).await
}

/// Parameters for the multi-file content read
/// (`GET /workspace/{workspace_id}/storage/content/`).
///
/// `nodes` and `query` are both **required** by the route; `limit`, `max_bytes`
/// and `output` fall back to the server's documented defaults when unset.
/// Blank `nodes` segments are dropped before the request is built (the server
/// ignores them too) and duplicates are de-duplicated server-side.
///
/// There is no `page`, `chunk_from`/`chunk_to` or `cursor` here: those address a
/// walk through ONE file, which [`read_content_chunks`] already serves. Follow a
/// passage found here by calling that route with the `position` this one
/// returned.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ContentManyParams {
    /// `nodes` — 1-[`CONTENT_MANY_MAX_NODES`] node ids, sent comma-joined. Both
    /// spellings are accepted, with or without hyphens.
    pub nodes: Vec<String>,
    /// `q` — the query every file is scored against.
    /// 1-[`CONTENT_QUERY_MAX_LEN`] characters. Required: this route publishes no
    /// other way to select text.
    pub query: String,
    /// `limit` — chunks returned **per file**
    /// ([`CONTENT_LIMIT_MIN`]-[`CONTENT_LIMIT_MAX`]; server default 3).
    pub limit: Option<u32>,
    /// `max_bytes` — UTF-8 byte budget spent **per file**, not shared across the
    /// request ([`CONTENT_MAX_BYTES_MIN`]-[`CONTENT_MAX_BYTES_MAX`]; server
    /// default 32768). Ten files at the maximum is a deliberate opt-in to
    /// roughly 2.5 MiB of text.
    pub max_bytes: Option<u32>,
    /// `output` — one of [`CONTENT_OUTPUT_VALUES`].
    pub output: Option<String>,
}

impl ContentManyParams {
    /// A parameter set naming the files to score and the query to score them
    /// against. Blank ids are dropped when the request is built.
    #[must_use]
    pub fn new(nodes: Vec<String>, query: impl Into<String>) -> Self {
        Self {
            nodes,
            query: query.into(),
            limit: None,
            max_bytes: None,
            output: None,
        }
    }

    /// Set `limit` (chunks per file).
    #[must_use]
    pub fn limit(mut self, v: Option<u32>) -> Self {
        self.limit = v;
        self
    }

    /// Set `max_bytes` (per-file UTF-8 byte budget).
    #[must_use]
    pub fn max_bytes(mut self, v: Option<u32>) -> Self {
        self.max_bytes = v;
        self
    }

    /// Set `output` (chunk verbosity).
    #[must_use]
    pub fn output(mut self, v: Option<&str>) -> Self {
        self.output = v.map(str::to_owned);
        self
    }

    /// The node ids actually sent: trimmed, with blank segments dropped.
    ///
    /// One function so [`Self::validate`] counts exactly what
    /// [`Self::to_query`] emits — a count taken from the raw `Vec` would accept
    /// eleven ids when one of them is blank, or refuse a list of one real id
    /// beside a stray comma.
    fn effective_nodes(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// Reject, client-side, the inputs the server answers with
    /// `1605 (Invalid Input)` / `406`.
    ///
    /// # Errors
    /// [`CliError::Parse`] when `nodes` names no ids after blanks are dropped or
    /// more than [`CONTENT_MANY_MAX_NODES`] of them, or when `q`, `limit`,
    /// `max_bytes` or `output` is outside its documented bounds. A malformed
    /// (but non-blank) id is left to the server, which owns the id grammar.
    pub fn validate(&self) -> Result<(), CliError> {
        let nodes = self.effective_nodes();
        if nodes.is_empty() {
            return Err(CliError::Parse(
                "nodes must name at least one file id".to_owned(),
            ));
        }
        if nodes.len() > CONTENT_MANY_MAX_NODES {
            return Err(CliError::Parse(format!(
                "nodes must name at most {CONTENT_MANY_MAX_NODES} file ids (got {})",
                nodes.len(),
            )));
        }
        validate_content_query(&self.query, "q")?;
        validate_content_bounds(self.limit, self.max_bytes, self.output.as_deref())
    }

    /// Build the query-parameter map, joining `nodes` into the single
    /// comma-separated value the route expects.
    fn to_query(&self) -> HashMap<String, String> {
        let mut params = HashMap::new();
        params.insert("nodes".to_owned(), self.effective_nodes().join(","));
        params.insert("q".to_owned(), self.query.clone());
        if let Some(v) = self.limit {
            params.insert("limit".to_owned(), v.to_string());
        }
        if let Some(v) = self.max_bytes {
            params.insert("max_bytes".to_owned(), v.to_string());
        }
        if let Some(v) = &self.output {
            params.insert("output".to_owned(), v.clone());
        }
        params
    }
}

/// Score several named files against one query and return the passages that
/// answered it.
///
/// `GET /workspace/{workspace_id}/storage/content/?nodes=<csv>&q=…`
///
/// The relevance mode of [`read_content_chunks`] asked of up to
/// [`CONTENT_MANY_MAX_NODES`] files at once, so a caller assembling context for
/// a prompt makes one request instead of ten. The chunk objects are identical to
/// that route's, field for field, and a `position` returned here can be sent
/// straight back to it as `chunk_from`.
///
/// **Workspace only — there is no share form of this route.** The file ids
/// travel in the `nodes` query parameter rather than the path, so this endpoint
/// sits beside `search/` rather than under a `{node_id}`.
///
/// **Each file is scored against itself.** Scores are comparable only WITHIN one
/// file's chunk list; do not merge the per-file lists and re-sort them by
/// `score`.
///
/// Returns the response object verbatim: `q`, `limit`, `nodes` (an **object**
/// keyed by hyphenated node id, in the order the ids were named — it is never
/// normalized into an array here, so a caller can key straight into it) and
/// `missing[]` (`{id, reason}` with `reason` one of `not_found`, `trashed`,
/// `not_text`). `complete` and `next_cursor` are deliberately not published on
/// this route; ask [`read_content_chunks`] when you need them.
///
/// Error codes (see the published API docs at
/// `https://api.fast.io/current/llms/full/`):
///
/// | class | HTTP | condition |
/// |-------|------|-----------|
/// | `1605 (Invalid Input)` | 406 | `nodes` names no ids, more than [`CONTENT_MANY_MAX_NODES`], or a malformed id; `q` missing, empty or too long; `limit` or `max_bytes` out of bounds |
/// | `1654 (Internal Error)` | 500 | content temporarily unavailable — **retry**; never treat this as "these files have no matching text" |
/// | `1654 (Internal Error)` | 500 | a named node could not be retrieved — the WHOLE request fails; such a node is never reported in `missing` |
///
/// An empty `chunks` list therefore always means those files hold nothing
/// matching the query, and never that the platform could not look.
pub async fn read_content_many(
    client: &ApiClient,
    workspace_id: &str,
    params: &ContentManyParams,
) -> Result<Value, CliError> {
    params.validate()?;
    let query = params.to_query();
    let path = format!(
        "/workspace/{}/storage/content/",
        urlencoding::encode(workspace_id),
    );
    client.get_with_params(&path, &query).await
}

/// Flatten a multi-file read response into one row per returned chunk.
///
/// The [`read_content_many`] envelope is `{q, limit, nodes: {<id>: {...}},
/// missing: [...]}`. The single-payload renderers (`--format table|csv`) pick
/// ONE array out of a response, and with that key set they land on `missing`
/// — showing the ids that could not be read while silently dropping every
/// chunk that was. Callers rendering a table therefore pass these rows
/// instead of the envelope (JSON and markdown keep the envelope untouched).
///
/// Each row carries the file it came from (`node_id`, `name`,
/// `indexed_version_id`) beside the chunk's own fields, in the order the
/// files were named and the chunks were returned (`score` descending within
/// a file). Scores are only comparable WITHIN one file, so the rows are
/// deliberately not re-sorted across files. Files that matched nothing
/// contribute no rows; the `missing` list is not folded in — report it
/// separately.
#[must_use]
pub fn content_many_chunk_rows(value: &Value) -> Vec<Value> {
    let Some(nodes) = value.get("nodes").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (node_id, file) in nodes {
        let Some(chunks) = file.get("chunks").and_then(Value::as_array) else {
            continue;
        };
        for chunk in chunks {
            let mut row = serde_json::Map::new();
            row.insert("node_id".to_owned(), Value::String(node_id.clone()));
            for key in ["name", "indexed_version_id"] {
                row.insert(
                    key.to_owned(),
                    file.get(key).cloned().unwrap_or(Value::Null),
                );
            }
            if let Some(fields) = chunk.as_object() {
                for (k, v) in fields {
                    row.insert(k.clone(), v.clone());
                }
            }
            rows.push(Value::Object(row));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{
        BULK_DETAILS_MAX_IDS, BulkDetailsResponse, SearchFilesParams, add_file_form,
        create_folder_form, lock_acquire_form, metadata_filter_block, normalize_search_response,
        parse_bulk_details_response, recent_query, sanitize_terminal_string, update_node_form,
    };
    use super::{
        CONTENT_LIMIT_MAX, CONTENT_LIMIT_MIN, CONTENT_MANY_MAX_NODES, CONTENT_MAX_BYTES_MAX,
        CONTENT_MAX_BYTES_MIN, CONTENT_OUTPUT_VALUES, CONTENT_POSITION_LIMIT,
        CONTENT_QUERY_MAX_LEN, ContentManyParams, ContentReadParams, content_many_chunk_rows,
    };
    use crate::error::CliError;
    use crate::output::flatten_response;
    use serde_json::{Value, json};

    #[test]
    fn create_folder_form_omits_force_by_default() {
        let f = create_folder_form("docs", false);
        assert_eq!(f.get("name").map(String::as_str), Some("docs"));
        assert!(!f.contains_key("force"), "force omitted unless requested");
    }

    #[test]
    fn create_folder_form_sets_force() {
        let f = create_folder_form("docs", true);
        assert_eq!(f.get("force").map(String::as_str), Some("true"));
    }

    #[test]
    fn recent_query_filters_by_type() {
        let q = recent_query(Some(250), Some("cur"), Some("file"));
        assert_eq!(q.get("page_size").map(String::as_str), Some("250"));
        assert_eq!(q.get("cursor").map(String::as_str), Some("cur"));
        assert_eq!(q.get("type").map(String::as_str), Some("file"));
    }

    #[test]
    fn recent_query_empty_when_no_args() {
        assert!(recent_query(None, None, None).is_empty());
    }

    #[test]
    fn update_node_form_sends_only_present_fields() {
        let f = update_node_form(None, None, Some("My Title"), Some("Short desc"), None);
        assert_eq!(
            f.get("metadata_title").map(String::as_str),
            Some("My Title")
        );
        assert_eq!(
            f.get("metadata_short").map(String::as_str),
            Some("Short desc")
        );
        assert!(!f.contains_key("name"));
        assert!(!f.contains_key("from"));
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn update_node_form_carries_content_source() {
        let from = r#"{"type":"upload","upload":{"id":"u1"}}"#;
        let f = update_node_form(Some("new.txt"), Some(from), None, None, None);
        assert_eq!(f.get("name").map(String::as_str), Some("new.txt"));
        assert_eq!(f.get("from").map(String::as_str), Some(from));
    }

    /// The CAS precondition must reach the wire under the server's field name,
    /// and must be OMITTED when absent or blank — an empty precondition is a
    /// base the server cannot check.
    /// A metadata-only update creates NO version, so a precondition on it can
    /// never go stale — two writers on one base both pass and the second wins
    /// silently. Measured on the deployed platform through the shipped binary. The
    /// refusal lives at the command boundary; this pins the FACT it rests on so
    /// the guard is not "simplified" away later.
    #[test]
    fn metadata_only_update_sends_no_version_creating_field() {
        let f = update_node_form(None, None, Some("t"), Some("s"), Some("v1"));
        assert!(
            !f.contains_key("name") && !f.contains_key("from"),
            "metadata-only update carries no version-creating field: {f:?}"
        );
        assert_eq!(f.get("if_version_id").map(String::as_str), Some("v1"));
    }

    #[test]
    fn update_node_form_carries_if_version_id() {
        let f = update_node_form(None, None, None, Some("s"), Some("ver-1"));
        assert_eq!(f.get("if_version_id").map(String::as_str), Some("ver-1"));

        for blank in [None, Some(""), Some("   ")] {
            let f = update_node_form(None, None, None, Some("s"), blank);
            assert!(
                !f.contains_key("if_version_id"),
                "blank/absent precondition must be omitted (supplied {blank:?})"
            );
        }
    }

    #[test]
    fn add_file_form_carries_name_and_from() {
        let from = r#"{"type":"hash","hash":{"hash":"abc","hash_type":"sha256"}}"#;
        let f = add_file_form("photo.jpg", from);
        assert_eq!(f.get("name").map(String::as_str), Some("photo.jpg"));
        assert_eq!(f.get("from").map(String::as_str), Some(from));
    }

    #[test]
    fn lock_acquire_form_emits_duration_and_client_info() {
        let f = lock_acquire_form(Some(300), Some(r#"{"device_name":"Laptop"}"#));
        assert_eq!(f.get("duration").map(String::as_str), Some("300"));
        assert_eq!(
            f.get("client_info").map(String::as_str),
            Some(r#"{"device_name":"Laptop"}"#)
        );
    }

    #[test]
    fn lock_acquire_form_empty_by_default() {
        assert!(lock_acquire_form(None, None).is_empty());
    }

    #[test]
    fn normalize_search_files_map_to_rows() {
        // Keyword-only / intelligence-disabled shape: `files` is a MAP keyed by
        // node id. After normalization it must be an ARRAY with one record per
        // file, each carrying `id` and `node_id`.
        let resp = json!({
            "result": true,
            "files": {
                "f1": {"name": "File 1", "type": "file"},
                "f2": {"name": "File 2", "type": "file", "relevance_score": 0.5}
            }
        });
        let out = normalize_search_response(resp);
        let files = out.get("files").and_then(Value::as_array).expect("array");
        assert_eq!(files.len(), 2);
        // Order preserved (preserve_order feature).
        assert_eq!(files[0]["id"], "f1");
        assert_eq!(files[0]["node_id"], "f1");
        assert_eq!(files[0]["name"], "File 1");
        assert_eq!(files[1]["id"], "f2");
        // The envelope `result` field survives for the markdown preamble.
        assert_eq!(out["result"], json!(true));
    }

    #[test]
    fn normalize_search_files_map_flattens_to_one_row_per_file() {
        // End-to-end: the normalized envelope must flatten (table/CSV path) to
        // the `files` ARRAY — one row per file — not a single object row.
        let resp = json!({
            "result": true,
            "files": {
                "f1": {"name": "File 1"},
                "f2": {"name": "File 2"}
            }
        });
        let flattened = flatten_response(&normalize_search_response(resp));
        let arr = flattened.as_array().expect("flattened to array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["node_id"], "f1");
    }

    #[test]
    fn normalize_search_results_array_untouched() {
        // The `details`/ai.txt shape (`results[]` + pagination) has no `files`
        // map and must pass through unchanged.
        let resp = json!({
            "result": true,
            "results": [{"node_id": "f1", "score": 0.9}],
            "pagination": {"total": 1, "has_more": false}
        });
        let out = normalize_search_response(resp.clone());
        assert_eq!(out, resp);
    }

    #[test]
    fn normalize_search_files_array_untouched() {
        // An already-array `files` value (or any non-map) is left as-is.
        let resp = json!({"files": [{"id": "f1"}]});
        let out = normalize_search_response(resp.clone());
        assert_eq!(out, resp);
    }

    #[test]
    fn normalize_search_non_object_envelope_untouched() {
        let resp = json!(["a", "b"]);
        assert_eq!(normalize_search_response(resp.clone()), resp);
    }

    #[test]
    fn search_files_params_emit_documented_keys_only() {
        // Per storage.txt/ai.txt the SEARCH endpoint takes search/limit/offset/
        // files_scope/folders_scope/details/output — and NEVER page_size/cursor.
        let params = SearchFilesParams::new()
            .files_scope(Some("f1:v1"))
            .folders_scope(Some("d1:3"))
            .limit(Some(10))
            .offset(Some(5))
            .details(true)
            .output(Some("terse"));
        let q = params.into_query("quarterly report");
        assert_eq!(
            q.get("search").map(String::as_str),
            Some("quarterly report")
        );
        assert_eq!(q.get("files_scope").map(String::as_str), Some("f1:v1"));
        assert_eq!(q.get("folders_scope").map(String::as_str), Some("d1:3"));
        assert_eq!(q.get("limit").map(String::as_str), Some("10"));
        assert_eq!(q.get("offset").map(String::as_str), Some("5"));
        assert_eq!(q.get("details").map(String::as_str), Some("true"));
        assert_eq!(q.get("output").map(String::as_str), Some("terse"));
        // The retired keyset params must never appear.
        assert!(!q.contains_key("page_size"), "page_size must not be sent");
        assert!(!q.contains_key("cursor"), "cursor must not be sent");
    }

    #[test]
    fn search_files_params_default_sends_only_search() {
        let q = SearchFilesParams::new().into_query("hello");
        assert_eq!(q.len(), 1);
        assert_eq!(q.get("search").map(String::as_str), Some("hello"));
        // An unset filter must not put the key on the wire at all: an empty
        // `filters` is not the same request as no `filters`.
        assert!(!q.contains_key("filters"), "filters must not be sent unset");
    }

    #[test]
    fn search_files_params_send_filters_verbatim() {
        // The predicate text must reach the wire byte-identical. The integer
        // here is above 2^53, so any round trip through a float-backed JSON
        // number would silently corrupt it (see the published API docs).
        let raw = r#"[{"field":"invoice_total","operator":">=","value":9007199254740993}]"#;
        let q = SearchFilesParams::new().filters(Some(raw)).into_query("q");
        assert_eq!(q.get("filters").map(String::as_str), Some(raw));
    }

    #[test]
    fn search_files_params_filters_combine_with_files_scope() {
        // `filters` + `files_scope` is an INTERSECTION the server supports, so
        // the builder must never treat them as alternatives.
        let q = SearchFilesParams::new()
            .files_scope(Some("n1:v1"))
            .filters(Some("[{\"field\":\"a\",\"operator\":\"exists\"}]"))
            .into_query("q");
        assert_eq!(q.get("files_scope").map(String::as_str), Some("n1:v1"));
        assert!(q.contains_key("filters"));
    }

    #[test]
    fn search_files_params_filters_with_folder_scope_left_to_the_server() {
        // The server currently refuses this pair, but that is POLICY the
        // contract flags as changeable — the builder must still send both and
        // let the server answer, or the CLI would keep refusing after the
        // platform started accepting it.
        let q = SearchFilesParams::new()
            .folders_scope(Some("d1:3"))
            .filters(Some("[{\"field\":\"a\",\"operator\":\"exists\"}]"))
            .into_query("q");
        assert_eq!(q.get("folders_scope").map(String::as_str), Some("d1:3"));
        assert!(q.contains_key("filters"));
    }

    #[test]
    fn metadata_filter_block_found_only_at_top_level() {
        let resp = json!({
            "files": [],
            "search_metadata": {"scoped": false},
            "metadata_filter": {"applied": true, "matched": 34},
        });
        let block = metadata_filter_block(&resp).expect("block present");
        assert_eq!(block.get("matched"), Some(&json!(34)));
    }

    #[test]
    fn metadata_filter_block_absent_is_none() {
        // The signal that a filter did NOT run. `scoped` is unrelated and must
        // not be mistaken for it.
        let resp = json!({"files": [], "search_metadata": {"scoped": true}});
        assert!(metadata_filter_block(&resp).is_none());
    }

    #[test]
    fn metadata_filter_block_null_is_treated_as_absent() {
        let resp = json!({"files": [], "metadata_filter": null});
        assert!(metadata_filter_block(&resp).is_none());
    }

    #[test]
    fn metadata_filter_block_survives_response_normalization() {
        // `normalize_search_response` rewrites the `files` MAP in place; if it
        // ever dropped sibling keys, the "filter did not run" warning would
        // fire on every successful filtered search.
        let resp = json!({
            "files": {"n1": {"name": "a.pdf"}},
            "metadata_filter": {"applied": true, "matched": 2},
        });
        let out = normalize_search_response(resp);
        assert!(out.get("files").expect("files").is_array());
        assert!(metadata_filter_block(&out).is_some());
    }

    #[test]
    fn search_files_params_forward_search_modes() {
        let modes = crate::api::types::SearchModeParams::new()
            .search_in(Some("filename"))
            .name_match(Some("contains"))
            .case_sensitive(Some(true));
        let q = SearchFilesParams::new().modes(modes).into_query("re*rt");
        assert_eq!(q.get("search_in").map(String::as_str), Some("filename"));
        assert_eq!(q.get("name_match").map(String::as_str), Some("contains"));
        assert_eq!(q.get("case_sensitive").map(String::as_str), Some("true"));
        // The pattern is forwarded RAW — the server owns escaping, so a literal
        // `*` must survive the client untouched.
        assert_eq!(q.get("search").map(String::as_str), Some("re*rt"));
    }

    #[test]
    fn search_files_params_do_not_cap_query_length() {
        // `/storage/search/` is uncapped and the CLI has always forwarded
        // queries past the unified endpoint's 1024-char limit. Guard that.
        let long: String = "x".repeat(2000);
        let modes = crate::api::types::SearchModeParams::new();
        assert!(modes.validate(&long).is_ok());
        let q = SearchFilesParams::new().into_query(&long);
        assert_eq!(q.get("search").map(String::len), Some(2000));
    }

    #[test]
    fn search_files_params_details_false_omits_key() {
        let q = SearchFilesParams::new().details(false).into_query("x");
        assert!(!q.contains_key("details"));
    }

    fn parsed(body: &serde_json::Value) -> BulkDetailsResponse {
        parse_bulk_details_response(body).expect("test body should parse")
    }

    #[test]
    fn parse_multi_format_envelope_wrapped() {
        let body = json!({
            "result": "yes",
            "response": {
                "format": "multi",
                "nodes": [{"id": "abc", "name": "a.txt"}],
                "errors": [
                    {"node_id": "missing", "code": 133_123, "message": "No such file or folder exists"}
                ]
            }
        });
        let r = parsed(&body);
        assert_eq!(r.nodes.len(), 1);
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].node_id, "missing");
        assert_eq!(r.errors[0].code, 133_123);
    }

    #[test]
    fn parse_multi_format_flat_envelope() {
        let body = json!({
            "result": "yes",
            "format": "multi",
            "nodes": [{"id": "abc"}],
            "errors": []
        });
        let r = parsed(&body);
        assert_eq!(r.nodes.len(), 1);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn parse_multi_format_404_all_errored() {
        let body = json!({
            "result": "no",
            "response": {
                "format": "multi",
                "nodes": [],
                "errors": [
                    {"node_id": "x", "code": 191_878, "message": "invalid id"},
                    {"node_id": "Y", "code": 133_123, "message": "No such file or folder exists"}
                ]
            }
        });
        let r = parsed(&body);
        assert!(r.nodes.is_empty());
        assert_eq!(r.errors.len(), 2);
        // Server echoes input casing, so "Y" stays uppercase here.
        assert_eq!(r.errors[1].node_id, "Y");
    }

    #[test]
    fn parse_single_format_lifts_node_into_nodes_vec() {
        let body = json!({
            "result": "yes",
            "response": {
                "format": "single",
                "node": {"id": "abc", "name": "a.txt"}
            }
        });
        let r = parsed(&body);
        assert_eq!(r.nodes.len(), 1);
        assert_eq!(r.nodes[0]["id"], "abc");
        assert!(r.errors.is_empty());
    }

    #[test]
    fn parse_single_format_with_null_node_drops_it() {
        // Hostile/buggy server: format=single with node=null must NOT
        // produce a Value::Null masquerading as a resolved node
        // (caught in adversarial review F1).
        let body = json!({
            "result": "yes",
            "response": {"format": "single", "node": null}
        });
        let r = parsed(&body);
        assert!(r.nodes.is_empty());
    }

    #[test]
    fn parse_missing_format_with_multi_shape_treats_as_multi() {
        // Older server builds and 404-all-errored bodies omitted
        // `format`. If the body has `nodes`/`errors` arrays,
        // shape-sniff to multi rather than silently dropping data.
        let body = json!({
            "result": "no",
            "response": {
                "nodes": [],
                "errors": [{"node_id": "x", "code": 133_123, "message": "missing"}]
            }
        });
        let r = parsed(&body);
        assert!(r.nodes.is_empty());
        assert_eq!(r.errors.len(), 1);
    }

    #[test]
    fn parse_missing_format_defaults_to_single() {
        // Backwards compat: legacy single-id responses without
        // `format`, nodes, or errors fall back to single-shape.
        let body = json!({
            "result": "yes",
            "response": {"node": {"id": "abc"}}
        });
        let r = parsed(&body);
        assert_eq!(r.nodes.len(), 1);
        assert_eq!(r.nodes[0]["id"], "abc");
    }

    #[test]
    fn parse_missing_format_and_node_yields_empty() {
        let body = json!({"result": "yes", "response": {}});
        let r = parsed(&body);
        assert!(r.nodes.is_empty());
        assert!(r.errors.is_empty());
    }

    #[test]
    fn parse_unknown_format_returns_parse_error() {
        let body = json!({
            "result": "yes",
            "response": {"format": "v2-batch", "nodes": []}
        });
        let err = parse_bulk_details_response(&body).expect_err("unknown format must error");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn parse_non_object_payload_returns_parse_error() {
        let body = json!([1, 2, 3]);
        let err = parse_bulk_details_response(&body).expect_err("non-object payload must error");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn sanitize_strips_control_and_bidi_codepoints() {
        // C0 controls (BEL, ESC), bidi override (U+202E), zero-width
        // joiner (U+200D), BOM (U+FEFF) are all stripped. Note the
        // filter strips control codepoints individually — printable
        // remnants of an ANSI escape sequence (`[2J` after ESC) survive,
        // but they're harmless without the preceding ESC byte.
        let raw = "hello\x07\u{202E}drowssap\u{200D}.txt\u{FEFF}";
        let cleaned = sanitize_terminal_string(raw);
        assert_eq!(cleaned, "hellodrowssap.txt");
        // ESC alone is stripped.
        assert_eq!(sanitize_terminal_string("a\x1bb"), "ab");
        // Whitespace controls (\t, \n, \r) preserved.
        assert_eq!(sanitize_terminal_string("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn build_bulk_details_path_joins_commas_literal() {
        let path = super::build_bulk_details_path(
            "workspace",
            "ws-1",
            &["abc".to_owned(), "DeF".to_owned(), "ghi-jkl".to_owned()],
        )
        .expect("happy path");
        assert_eq!(path, "/workspace/ws-1/storage/abc,DeF,ghi-jkl/details/");
    }

    #[test]
    fn build_bulk_details_path_duplicates_single_id_to_force_bulk_shape() {
        let path = super::build_bulk_details_path("workspace", "ws", &["abc".to_owned()])
            .expect("happy path");
        assert_eq!(path, "/workspace/ws/storage/abc,abc/details/");
    }

    #[test]
    fn build_bulk_details_path_rejects_empty_input() {
        let err = super::build_bulk_details_path("workspace", "ws", &[])
            .expect_err("empty input must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn build_bulk_details_path_rejects_oversize_input() {
        let ids: Vec<String> = (0..=BULK_DETAILS_MAX_IDS)
            .map(|i| format!("id{i}"))
            .collect();
        let err = super::build_bulk_details_path("workspace", "ws", &ids)
            .expect_err("oversize input must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn build_bulk_details_path_encodes_individual_ids() {
        // Per-id urlencoding turns a literal `,` inside an id into
        // `%2C`, preventing it from acting as a separator. The
        // separator commas between encoded ids stay literal.
        let path = super::build_bulk_details_path(
            "workspace",
            "ws",
            &["a,b".to_owned(), "c d".to_owned()],
        )
        .expect("happy path");
        assert_eq!(path, "/workspace/ws/storage/a%2Cb,c%20d/details/");
    }

    #[test]
    fn bulk_details_max_ids_matches_server_cap() {
        assert_eq!(BULK_DETAILS_MAX_IDS, 25);
    }

    // ─── content routes: client-side window validation ──────────────────────

    /// The empty parameter set is the "read from the beginning" call and must
    /// stay valid — every field is optional on this route.
    #[test]
    fn content_read_accepts_no_selector() {
        assert!(ContentReadParams::new().validate().is_ok());
        assert!(ContentReadParams::new().to_query().is_empty());
    }

    #[test]
    fn content_read_accepts_each_single_selector() {
        assert!(
            ContentReadParams::new()
                .query(Some("retention"))
                .validate()
                .is_ok()
        );
        assert!(ContentReadParams::new().page(Some(2)).validate().is_ok());
        assert!(
            ContentReadParams::new()
                .chunks(Some(0), None)
                .validate()
                .is_ok()
        );
        assert!(
            ContentReadParams::new()
                .chunks(Some(4), Some(9))
                .validate()
                .is_ok()
        );
        // A range and an equal endpoint pair are both single windows.
        assert!(
            ContentReadParams::new()
                .chunks(Some(7), Some(7))
                .validate()
                .is_ok()
        );
    }

    /// `cursor` is a continuation, not a selector: it composes with the ordered
    /// windows and only conflicts with `q`.
    #[test]
    fn content_read_cursor_composes_with_ordered_windows() {
        assert!(
            ContentReadParams::new()
                .cursor(Some("tok"))
                .validate()
                .is_ok()
        );
        assert!(
            ContentReadParams::new()
                .chunks(Some(0), Some(20))
                .cursor(Some("tok"))
                .validate()
                .is_ok()
        );
        assert!(
            ContentReadParams::new()
                .page(Some(3))
                .cursor(Some("tok"))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn content_read_rejects_two_selectors_and_names_them() {
        let err = ContentReadParams::new()
            .query(Some("clause"))
            .page(Some(2))
            .validate()
            .expect_err("q and page are two windows");
        let msg = err.to_string();
        assert!(msg.contains('q') && msg.contains("page"), "{msg}");

        assert!(
            ContentReadParams::new()
                .query(Some("clause"))
                .chunks(Some(1), Some(2))
                .validate()
                .is_err()
        );
        assert!(
            ContentReadParams::new()
                .page(Some(1))
                .chunks(Some(1), None)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn content_read_rejects_query_with_cursor() {
        let err = ContentReadParams::new()
            .query(Some("clause"))
            .cursor(Some("tok"))
            .validate()
            .expect_err("relevance mode is not a walk");
        let msg = err.to_string();
        assert!(msg.contains('q') && msg.contains("cursor"), "{msg}");
    }

    #[test]
    fn content_read_rejects_chunk_to_without_chunk_from() {
        let err = ContentReadParams::new()
            .chunks(None, Some(5))
            .validate()
            .expect_err("a range needs both endpoints");
        assert!(err.to_string().contains("chunk_from"), "{err}");
    }

    #[test]
    fn content_read_rejects_inverted_range() {
        let err = ContentReadParams::new()
            .chunks(Some(9), Some(4))
            .validate()
            .expect_err("chunk_from must not exceed chunk_to");
        let msg = err.to_string();
        assert!(msg.contains('9') && msg.contains('4'), "{msg}");
    }

    /// The ceiling is EXCLUSIVE — position 9999 is addressable, 10000 is not.
    #[test]
    fn content_read_rejects_positions_at_or_beyond_the_ceiling() {
        assert_eq!(CONTENT_POSITION_LIMIT, 10_000);
        assert!(
            ContentReadParams::new()
                .chunks(Some(CONTENT_POSITION_LIMIT - 1), None)
                .validate()
                .is_ok()
        );
        for params in [
            ContentReadParams::new().chunks(Some(CONTENT_POSITION_LIMIT), None),
            ContentReadParams::new().chunks(Some(0), Some(CONTENT_POSITION_LIMIT)),
            ContentReadParams::new().chunks(Some(50_000), None),
        ] {
            let err = params.validate().expect_err("position at/over the ceiling");
            assert!(err.to_string().contains("cursor"), "{err}");
        }
    }

    #[test]
    fn content_read_rejects_page_zero() {
        assert!(ContentReadParams::new().page(Some(0)).validate().is_err());
        assert!(ContentReadParams::new().page(Some(1)).validate().is_ok());
    }

    #[test]
    fn content_read_enforces_query_length_bounds() {
        assert!(ContentReadParams::new().query(Some("")).validate().is_err());
        assert!(
            ContentReadParams::new()
                .query(Some("   "))
                .validate()
                .is_err()
        );
        let max = "x".repeat(CONTENT_QUERY_MAX_LEN);
        assert!(
            ContentReadParams::new()
                .query(Some(&max))
                .validate()
                .is_ok()
        );
        let over = "x".repeat(CONTENT_QUERY_MAX_LEN + 1);
        assert!(
            ContentReadParams::new()
                .query(Some(&over))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn content_read_enforces_limit_and_max_bytes_bounds() {
        assert!(ContentReadParams::new().limit(Some(0)).validate().is_err());
        assert!(ContentReadParams::new().limit(Some(21)).validate().is_err());
        assert!(
            ContentReadParams::new()
                .limit(Some(CONTENT_LIMIT_MIN))
                .validate()
                .is_ok()
        );
        assert!(
            ContentReadParams::new()
                .limit(Some(CONTENT_LIMIT_MAX))
                .validate()
                .is_ok()
        );

        assert!(
            ContentReadParams::new()
                .max_bytes(Some(CONTENT_MAX_BYTES_MIN - 1))
                .validate()
                .is_err()
        );
        assert!(
            ContentReadParams::new()
                .max_bytes(Some(CONTENT_MAX_BYTES_MAX + 1))
                .validate()
                .is_err()
        );
        assert!(
            ContentReadParams::new()
                .max_bytes(Some(CONTENT_MAX_BYTES_MIN))
                .validate()
                .is_ok()
        );
        assert!(
            ContentReadParams::new()
                .max_bytes(Some(CONTENT_MAX_BYTES_MAX))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn content_read_enforces_output_vocabulary() {
        for ok in CONTENT_OUTPUT_VALUES {
            assert!(
                ContentReadParams::new().output(Some(ok)).validate().is_ok(),
                "{ok} must be accepted"
            );
        }
        for bad in ["verbose", "Full", "standard,markdown", ""] {
            assert!(
                ContentReadParams::new()
                    .output(Some(bad))
                    .validate()
                    .is_err(),
                "`{bad}` must be rejected"
            );
        }
    }

    /// Only the parameters the caller set may reach the wire — the server owns
    /// the defaults, and sending our own would freeze them into the client.
    #[test]
    fn content_read_query_emits_only_what_was_set() {
        let q = ContentReadParams::new()
            .chunks(Some(4), Some(9))
            .limit(Some(20))
            .output(Some("terse"))
            .to_query();
        assert_eq!(q.get("chunk_from").map(String::as_str), Some("4"));
        assert_eq!(q.get("chunk_to").map(String::as_str), Some("9"));
        assert_eq!(q.get("limit").map(String::as_str), Some("20"));
        assert_eq!(q.get("output").map(String::as_str), Some("terse"));
        for absent in ["q", "page", "cursor", "max_bytes"] {
            assert!(!q.contains_key(absent), "{absent} must be absent: {q:?}");
        }

        // The relevance query travels as `q`, never as `search` or `query`.
        let q = ContentReadParams::new().query(Some("retention")).to_query();
        assert_eq!(q.get("q").map(String::as_str), Some("retention"));
        assert_eq!(q.len(), 1, "{q:?}");
    }

    // ─── content routes: multi-file parameters ──────────────────────────────

    fn many(ids: &[&str]) -> ContentManyParams {
        ContentManyParams::new(ids.iter().map(|s| (*s).to_owned()).collect(), "retention")
    }

    #[test]
    fn content_many_rejects_no_ids() {
        assert!(many(&[]).validate().is_err());
        // Only blanks is the same thing as none. (A literal `,` INSIDE an id
        // is not split here — `nodes` is a `Vec` of ids, so the caller names
        // one id per element and the server owns the id grammar.)
        let err = many(&["", "  ", "\t"])
            .validate()
            .expect_err("blank-only nodes name nothing");
        assert!(err.to_string().contains("nodes"), "{err}");
    }

    #[test]
    fn content_many_enforces_the_node_cap() {
        assert_eq!(CONTENT_MANY_MAX_NODES, 10);
        let ten: Vec<String> = (0..CONTENT_MANY_MAX_NODES)
            .map(|i| format!("id{i}"))
            .collect();
        assert!(ContentManyParams::new(ten.clone(), "q").validate().is_ok());

        let mut eleven = ten;
        eleven.push("one-too-many".to_owned());
        let err = ContentManyParams::new(eleven, "q")
            .validate()
            .expect_err("eleven ids exceed the cap");
        assert!(err.to_string().contains("10"), "{err}");
    }

    /// Blanks are dropped BEFORE the cap is counted, so a stray comma neither
    /// pushes a legal list over the limit nor rescues an over-long one.
    #[test]
    fn content_many_drops_blank_segments_before_counting() {
        let p = many(&["a", "", "  ", "b"]);
        assert!(p.validate().is_ok());
        assert_eq!(p.to_query().get("nodes").map(String::as_str), Some("a,b"));

        let mut with_blanks: Vec<String> = (0..CONTENT_MANY_MAX_NODES)
            .map(|i| format!("id{i}"))
            .collect();
        with_blanks.push(String::new());
        assert!(
            ContentManyParams::new(with_blanks, "q").validate().is_ok(),
            "a blank must not count toward the cap"
        );
    }

    #[test]
    fn content_many_enforces_query_bounds() {
        assert!(
            ContentManyParams::new(vec!["a".to_owned()], "")
                .validate()
                .is_err()
        );
        assert!(
            ContentManyParams::new(vec!["a".to_owned()], "   ")
                .validate()
                .is_err()
        );
        let over = "x".repeat(CONTENT_QUERY_MAX_LEN + 1);
        assert!(
            ContentManyParams::new(vec!["a".to_owned()], over)
                .validate()
                .is_err()
        );
        let max = "x".repeat(CONTENT_QUERY_MAX_LEN);
        assert!(
            ContentManyParams::new(vec!["a".to_owned()], max)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn content_many_shares_the_single_file_bounds() {
        assert!(many(&["a"]).limit(Some(21)).validate().is_err());
        assert!(many(&["a"]).max_bytes(Some(512)).validate().is_err());
        assert!(many(&["a"]).output(Some("brief")).validate().is_err());
        assert!(
            many(&["a"])
                .limit(Some(3))
                .max_bytes(Some(CONTENT_MAX_BYTES_MAX))
                .output(Some("full"))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn content_many_query_joins_nodes_and_carries_q() {
        let q = many(&["abc", "def-ghi"]).limit(Some(3)).to_query();
        assert_eq!(q.get("nodes").map(String::as_str), Some("abc,def-ghi"));
        assert_eq!(q.get("q").map(String::as_str), Some("retention"));
        assert_eq!(q.get("limit").map(String::as_str), Some("3"));
        assert!(!q.contains_key("max_bytes"));
        assert!(!q.contains_key("output"));
    }

    #[test]
    fn content_many_chunk_rows_keeps_every_file_and_skips_missing() {
        let value = json!({
            "q": "retention",
            "limit": 3,
            "nodes": {
                "aaaaa-bbbbb": {
                    "name": "MSA.pdf",
                    "indexed_version_id": "v1",
                    "chunks": [
                        {"position": 4, "score": 7.25, "text": "record retention"},
                        {"position": 9, "score": 1.5, "text": "later"}
                    ],
                    "truncated": false
                },
                "ccccc-ddddd": {
                    "name": "empty.txt",
                    "indexed_version_id": null,
                    "chunks": [],
                    "truncated": false
                }
            },
            "missing": [{"id": "eeeee-fffff", "reason": "trashed"}]
        });
        let rows = content_many_chunk_rows(&value);
        assert_eq!(rows.len(), 2, "one row per chunk, none for the empty file");
        assert_eq!(rows[0]["node_id"], "aaaaa-bbbbb");
        assert_eq!(rows[0]["name"], "MSA.pdf");
        assert_eq!(rows[0]["indexed_version_id"], "v1");
        assert_eq!(rows[0]["position"], 4);
        assert_eq!(rows[1]["position"], 9);
        assert!(rows.iter().all(|r| r.get("missing").is_none()));

        // The flattener would have chosen `missing` over `nodes`, which is
        // exactly why the rows exist.
        let flattened = flatten_response(&value);
        assert!(flattened.as_array().is_some_and(|a| a.len() == 1));
        assert_eq!(flattened[0]["reason"], "trashed");

        assert!(content_many_chunk_rows(&json!({"nodes": {}})).is_empty());
        assert!(content_many_chunk_rows(&json!([])).is_empty());
    }
}
