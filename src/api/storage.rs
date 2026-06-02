#![allow(clippy::missing_errors_doc)]

/// Storage API endpoints for workspace file and folder operations.
///
/// Maps to endpoints documented at `/current/workspace/{workspace_id}/storage/`.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List files and folders in a workspace folder.
///
/// `GET /workspace/{workspace_id}/storage/{parent_id}/list/`
pub async fn list_files(
    client: &ApiClient,
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
        "/workspace/{}/storage/{}/list/",
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
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/details/",
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
        // id (round-2 review N3). Presentation-layer code is
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
/// markdown sanitizer (CLAUDE.md gotcha #14): the goal is to keep an
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
    workspace_id: &str,
    node_ids: &[String],
) -> Result<BulkDetailsResponse, CliError> {
    let path = build_bulk_details_path(workspace_id, node_ids)?;
    let (_status, body) = client.get_partial_envelope(&path).await?;
    parse_bulk_details_response(&body)
}

/// Build the bulk-details URL path. Extracted as a free function so
/// chunking and validation can be unit-tested without an HTTP client.
fn build_bulk_details_path(workspace_id: &str, node_ids: &[String]) -> Result<String, CliError> {
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
        "/workspace/{}/storage/{}/details/",
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
    workspace_id: &str,
    parent_id: &str,
    name: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/createfolder/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    client.post(&path, &form).await
}

/// Move a storage node to a different parent folder.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/move/`
pub async fn move_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    target_parent_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("parent".to_owned(), target_parent_id.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/move/",
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
    workspace_id: &str,
    node_id: &str,
    target_parent_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("parent".to_owned(), target_parent_id.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/copy/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Rename (update) a storage node.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/update/`
pub async fn rename_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    new_name: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), new_name.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/update/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Move a storage node to trash (soft delete).
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/delete/`
pub async fn delete_node(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/delete/",
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
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/workspace/{}/storage/{}/restore/",
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
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/purge/",
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
    workspace_id: &str,
    sort_by: Option<&str>,
    sort_dir: Option<&str>,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    list_files(
        client,
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
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/versions/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Parameters for the keyword/semantic file-search endpoint
/// (`GET .../storage/search/`).
///
/// This is the **single** builder shared by `files search`, the deprecated
/// `ripley search` forwarder, the MCP `files`/`workspace` search actions, and
/// the re-pointed MCP `handle_ai_search`. No second search builder exists
/// (the former `api::ai::search` and `api::workspace::search_workspace` both
/// forward here).
///
/// Pagination uses `limit`/`offset` — **not** `page_size`/`cursor` (the
/// search endpoint does not document keyset pagination, unlike the storage
/// LIST endpoint). `files_scope`/`folders_scope` narrow the indexed set;
/// `details=true` enriches each hit with the full node resource (the server
/// then caps the default `limit` to 10); `output` selects the verbosity of
/// the `content_snippet` field (`terse`/`standard`/`full`).
///
/// Sources: `storage.txt:1574-1677` (canonical `search` + `output`, `files`
/// MAP response) and `ai.txt:1654-1703` / `agents.md:772-855` (the same
/// endpoint accepting `limit`, `offset`, `files_scope`, `folders_scope`, and
/// `details`, with a `results[]` response). Both response shapes are handled
/// defensively by the renderer.
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
        params
    }
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
    workspace_id: &str,
    query: &str,
    params: SearchFilesParams<'_>,
) -> Result<Value, CliError> {
    let query_params = params.into_query(query);
    let path = format!(
        "/workspace/{}/storage/search/",
        urlencoding::encode(workspace_id),
    );
    client.get_with_params(&path, &query_params).await
}

/// Search for files in a share (keyword + semantic).
///
/// `GET /share/{share_id}/storage/search/?search=<query>`
///
/// Same contract as [`search_files`] but share-scoped. Returns `404` (error
/// `1609`) for workspace-backed (folder) shares, where search is unavailable.
pub async fn search_files_share(
    client: &ApiClient,
    share_id: &str,
    query: &str,
    params: SearchFilesParams<'_>,
) -> Result<Value, CliError> {
    let query_params = params.into_query(query);
    let path = format!("/share/{}/storage/search/", urlencoding::encode(share_id),);
    client.get_with_params(&path, &query_params).await
}

/// Normalize a `/storage/search/` response so every output format renders one
/// ROW PER FILE.
///
/// The keyword-only / intelligence-disabled search response (the canonical
/// `storage.txt` shape) returns a top-level `files` value that is a **MAP
/// keyed by node `OpaqueId`** — e.g. `{"files": {"f3jm5-…": {"name": …}}}`.
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
/// Both [`search_files`] callers (`fastio files search`, the deprecated
/// `fastio ripley search`) run their response through this before rendering.
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
    workspace_id: &str,
    page_size: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(v) = page_size {
        params.insert("page_size".to_owned(), v.to_string());
    }
    if let Some(v) = cursor {
        params.insert("cursor".to_owned(), v.to_owned());
    }
    let path = format!(
        "/workspace/{}/storage/recent/",
        urlencoding::encode(workspace_id),
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Add a share link to a folder.
///
/// `POST /workspace/{workspace_id}/storage/{parent_id}/addlink/`
pub async fn add_link(
    client: &ApiClient,
    workspace_id: &str,
    parent_id: &str,
    share_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("share_id".to_owned(), share_id.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/addlink/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(parent_id),
    );
    client.post(&path, &form).await
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
/// `POST /workspace/{workspace_id}/storage/{node_id}/versions/{version_id}/restore/`
pub async fn version_restore(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    version_id: &str,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/workspace/{}/storage/{}/versions/{}/restore/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
        urlencoding::encode(version_id),
    );
    client.post(&path, &form).await
}

/// Acquire a file lock.
///
/// `POST /workspace/{workspace_id}/storage/{node_id}/lock/`
pub async fn lock_acquire(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let form = HashMap::new();
    let path = format!(
        "/workspace/{}/storage/{}/lock/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.post(&path, &form).await
}

/// Check lock status.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/lock/`
pub async fn lock_status(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/lock/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Release a file lock.
///
/// `DELETE /workspace/{workspace_id}/storage/{node_id}/lock/`
///
/// The `lock_token` is the token returned by `lock_acquire` and must be
/// provided to prove ownership of the lock.
pub async fn lock_release(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    lock_token: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("lock_token".to_owned(), lock_token.to_owned());
    let path = format!(
        "/workspace/{}/storage/{}/lock/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.delete_with_form(&path, &form).await
}

/// Read file content (text).
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/content/`
pub async fn read_content(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/content/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

/// Read raw file bytes as text.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/read/`
///
/// Returns the node's raw content as a UTF-8 string (for a `.md` file, the
/// markdown source). Unlike [`read_content`] (which returns the `/content/`
/// JSON envelope) this is the binary `/read/` endpoint surfaced as text, used
/// by `fastio view` as the fallback for raw `.md` files that are not Note
/// nodes. An optional `version_id` reads a specific version.
pub async fn read_raw(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
    version_id: Option<&str>,
) -> Result<String, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/read/",
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

/// Get or create a quickshare link.
///
/// `GET /workspace/{workspace_id}/storage/{node_id}/quickshare/`
pub async fn quickshare_get(
    client: &ApiClient,
    workspace_id: &str,
    node_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/storage/{}/quickshare/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(node_id),
    );
    client.get(&path).await
}

#[cfg(test)]
mod tests {
    use super::{
        BULK_DETAILS_MAX_IDS, BulkDetailsResponse, SearchFilesParams, normalize_search_response,
        parse_bulk_details_response, sanitize_terminal_string,
    };
    use crate::error::CliError;
    use crate::output::flatten_response;
    use serde_json::{Value, json};

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
            "ws-1",
            &["abc".to_owned(), "DeF".to_owned(), "ghi-jkl".to_owned()],
        )
        .expect("happy path");
        assert_eq!(path, "/workspace/ws-1/storage/abc,DeF,ghi-jkl/details/");
    }

    #[test]
    fn build_bulk_details_path_duplicates_single_id_to_force_bulk_shape() {
        let path = super::build_bulk_details_path("ws", &["abc".to_owned()]).expect("happy path");
        assert_eq!(path, "/workspace/ws/storage/abc,abc/details/");
    }

    #[test]
    fn build_bulk_details_path_rejects_empty_input() {
        let err =
            super::build_bulk_details_path("ws", &[]).expect_err("empty input must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn build_bulk_details_path_rejects_oversize_input() {
        let ids: Vec<String> = (0..=BULK_DETAILS_MAX_IDS)
            .map(|i| format!("id{i}"))
            .collect();
        let err = super::build_bulk_details_path("ws", &ids)
            .expect_err("oversize input must be rejected");
        assert!(matches!(err, CliError::Parse(_)));
    }

    #[test]
    fn build_bulk_details_path_encodes_individual_ids() {
        // Per-id urlencoding turns a literal `,` inside an id into
        // `%2C`, preventing it from acting as a separator. The
        // separator commas between encoded ids stay literal.
        let path = super::build_bulk_details_path("ws", &["a,b".to_owned(), "c d".to_owned()])
            .expect("happy path");
        assert_eq!(path, "/workspace/ws/storage/a%2Cb,c%20d/details/");
    }

    #[test]
    fn bulk_details_max_ids_matches_server_cap() {
        assert_eq!(BULK_DETAILS_MAX_IDS, 25);
    }
}
