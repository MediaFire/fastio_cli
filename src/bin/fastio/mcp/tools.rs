// `CallToolResult` is the standard MCP error payload and is shared by
// every handler in this module; propagating it by-value in `Result::Err`
// is the ergonomic API that rmcp expects. Boxing it would cascade
// through ~200 handlers for no runtime benefit. Suppress the
// `result_large_err` lint at the module level.
#![allow(clippy::result_large_err)]

//! MCP tool definitions and action-based routing for the Fast.io CLI.
//!
//! Each tool corresponds to an API domain (auth, org, workspace, etc.)
//! and uses an `action` parameter to select the specific operation.
//! All tool handlers delegate to the existing `src/api/` functions.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, Content, ListToolsResult, Tool};
use serde_json::{Map, Value, json};

use secrecy::SecretString;

use fastio_cli::api;
use fastio_cli::auth::credentials::{CredentialsFile, StoredCredentials};

use super::McpState;

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build a successful MCP tool result from a JSON value.
///
/// Returns markdown-formatted content by default, byte-equivalent to the
/// server-side `?output=markdown` contract. Markdown is roughly 3–5×
/// more token-efficient for LLM consumers than pretty-printed JSON
/// across typical Fast.io list/details payloads, so MCP tool responses
/// use it by default.
fn success_json(value: &Value) -> CallToolResult {
    let text = fastio_cli::output::markdown::to_markdown(value);
    CallToolResult::success(vec![Content::text(text)])
}

/// Build an error MCP tool result (`is_error` = true).
fn error_text(msg: &str) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_owned())])
}

/// Resolve a tool name (possibly a hidden dispatch alias) to its canonical
/// `TOOL_DEFS` name — the name a caller passes to `--tools` and the name that
/// appears in `list_tools`.
///
/// The router accepts two back-compat aliases that route to a renamed handler:
/// `ai` → `ripley` and `how-to` → `howto`. The `--tools` allow-list is validated
/// against `TOOL_DEFS` names (which carry the canonical spellings only), so the
/// filter gate must map an alias to its canonical name before checking
/// membership — otherwise `--tools ripley` would wrongly reject a legacy `ai`
/// call. It is also applied when normalizing the raw `--tools` list so a caller
/// may spell the allow-list entry as either the alias or the canonical name. Any
/// name that is not a known alias is returned unchanged.
pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "ai" => "ripley",
        "how-to" => "howto",
        other => other,
    }
}

/// Extract a required string parameter.
fn required_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, CallToolResult> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        CallToolResult::error(vec![Content::text(format!(
            "Missing required parameter: {key}"
        ))])
    })
}

/// Extract an optional string parameter.
fn optional_str<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Extract an optional string parameter, REJECTING a present non-string.
///
/// [`optional_str`] is `as_str()`, so a present number, bool, array or object
/// reads as absent. That is harmless where absence and a bad value lead to the
/// same place, but not where omitting the field changes the REQUEST — there,
/// silently treating `5` as "not supplied" sends a different call than the
/// caller wrote. This helper makes each case explicit:
///
/// - key missing, or present as JSON `null` → `Ok(None)`: the field is omitted
///   from the request. `null` is how MCP clients spell an unset optional, so it
///   is deliberately not an error.
/// - a JSON string, **including `""`** → `Ok(Some(..))`: sent verbatim. An empty
///   string is an EXPLICIT empty value and is not the same request as omission.
/// - any other JSON type → `Err(..)`, naming the parameter, so a mistyped value
///   is refused rather than reinterpreted.
fn optional_str_strict<'a>(
    args: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(CallToolResult::error(vec![Content::text(format!(
            "Parameter {key} must be a string (omit it, or send null, to leave it unset)"
        ))])),
    }
}

/// Extract an optional **ID** parameter, accepting a string OR a JSON number.
///
/// Reading a scoping id with a plain [`optional_str`] treats an LLM that emits
/// a 19-digit id the natural way — as a JSON *number* — as having omitted it.
/// The search would then run **unscoped** across everything the caller could
/// see and return another workspace's events as though they were the one
/// asked for: wrong data, no error, and no server backstop, since the server
/// only sees a request with no scope.
///
/// Coercing rather than rejecting is deliberate. `Number::to_string` is exact
/// for the u64 range that covers every 19-digit Fast.io id, so nothing is lost,
/// and an agent that spells an id as a number gets the query it meant instead of
/// an error it has to learn to avoid. A present value of any OTHER type is still
/// an error — that is a caller bug, not a spelling.
///
/// Returns owned `String`s because a coerced number has no borrowable source.
fn optional_id(args: &Map<String, Value>, key: &str) -> Result<Option<String>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(_) => Err(CallToolResult::error(vec![Content::text(format!(
            "Parameter {key} must be an ID string (a JSON number is accepted; \
             omit it, or send null, to leave it unset)"
        ))])),
    }
}

/// Extract an optional u32 parameter.
fn optional_u32(args: &Map<String, Value>, key: &str) -> Option<u32> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Extract an optional bool parameter.
fn optional_bool(args: &Map<String, Value>, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| {
        v.as_bool()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Strict variant of [`optional_u32`]: distinguishes ABSENT from
/// PRESENT-but-unparseable.
///
/// The lenient [`optional_u32`] collapses both "key omitted" and "key present
/// but not a valid u32" to `None`, which silently drops a malformed value (e.g.
/// `rotate: "90deg"` becomes "no rotation" rather than an error). For the
/// constrained transform params, a caller who *supplied* a value but
/// got it wrong should get a clear error, not silent default behavior. Returns
/// `Ok(None)` when the key is absent, `Ok(Some(v))` when present and a valid
/// u32, and `Err(error_text(...))` when present but not a valid u32.
fn optional_u32_strict(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<u32>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or_else(|| error_text(&format!("{key} must be a non-negative integer"))),
    }
}

/// Did the caller actually SUPPLY a retired offset-pagination argument?
///
/// Present-and-`null` is how an MCP client spells an unset optional — the
/// convention [`optional_str_strict`] and [`optional_u32_strict`] document and
/// honour. A bare `args.contains_key(..)` does not make that distinction, so a
/// client that materializes every optional as `null` was told its slice had
/// been IGNORED when it had asked for nothing at all.
///
/// Extracted from the `eligible` arm rather than left inline **because it is a
/// guard and guards need tests**: as an expression buried in an async handler
/// it could not be exercised without a live client, which is precisely why the
/// `null` case went unnoticed. The CLI twin computes the same thing from parsed
/// values (`limit.is_some() || offset.is_some()`) and never had the bug.
fn legacy_offset_args_supplied(args: &Map<String, Value>) -> bool {
    ["limit", "offset"]
        .iter()
        .any(|key| args.get(*key).is_some_and(|v| !v.is_null()))
}

/// Strict variant of [`optional_bool`]: distinguishes ABSENT from
/// PRESENT-but-unparseable.
///
/// The lenient [`optional_bool`] collapses "key omitted" and "key present but
/// not a valid bool" to `None`, so a typo like `intelligence: "tru"` silently
/// becomes the default rather than an error. For the constrained boolean
/// params, a present-but-invalid value should error. Returns
/// `Ok(None)` when absent, `Ok(Some(v))` when present and a valid bool
/// (native bool or the strings `"true"`/`"false"`), and
/// `Err(error_text(...))` when present but not a valid bool.
fn optional_bool_strict(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or_else(|| error_text(&format!("{key} must be a boolean (true or false)"))),
    }
}

/// Extract an optional u64 parameter (number or string-encoded).
fn optional_u64(args: &Map<String, Value>, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Require authentication before proceeding. Returns an error result if not authed.
async fn require_auth(state: &McpState) -> Result<(), CallToolResult> {
    if state.is_authenticated().await {
        Ok(())
    } else {
        Err(CallToolResult::error(vec![Content::text(
            "Not authenticated. Run `fastio auth login` in a terminal first, \
             or use the auth tool with action=\"signin\" to sign in with email/password.",
        )]))
    }
}

/// Convert a `CliError` into an MCP tool error result.
fn cli_err_to_result(err: &fastio_cli::error::CliError) -> CallToolResult {
    error_text(&err.to_string())
}

/// Gate an AI-credit-spending action behind an explicit `confirm_ai_spend`
/// acknowledgement.
///
/// The interactive CLI gates credit spend behind the `--confirm-ai-spend`
/// flag. The MCP surface is non-interactive, so the same protection is
/// expressed as a required-to-proceed `confirm_ai_spend` boolean parameter:
/// unless the caller passes `confirm_ai_spend == true`, the action is rejected
/// BEFORE any API call (and thus before any credits are spent). Returns
/// `Some(error_result)` to short-circuit the handler when confirmation is
/// missing, or `None` when the caller has explicitly opted in.
fn require_ai_spend_confirmation(args: &Map<String, Value>) -> Option<CallToolResult> {
    if ai_spend_confirmed(args) {
        None
    } else {
        Some(error_text(AI_SPEND_REJECTION))
    }
}

/// Error message returned when an AI-credit-spending MCP action is invoked
/// without an explicit `confirm_ai_spend=true`.
const AI_SPEND_REJECTION: &str =
    "this action spends AI credits; pass confirm_ai_spend=true to proceed";

/// Valid `preview_type` values for the `preview` tool's `get` action. The
/// server 400s on anything outside this set, so MCP validates the same way
/// the CLI does (which requires `preview_type` for `get`).
const PREVIEW_TYPES: &[&str] = &[
    "bin",
    "thumbnail",
    "image",
    "hlsstream",
    "pdf",
    "spreadsheet",
    "audio",
    "mp4",
];

/// Valid `sort` values for the `comment` list / list-all actions. The CLI
/// restricts this via a clap `value_parser = ["asc", "desc"]`; MCP enforces the
/// same set so a bad sort gets a clear error instead of reaching the server.
const COMMENT_SORT_ORDERS: &[&str] = &["asc", "desc"];

/// Validate the optional `sort` arg for the `comment` list actions against
/// [`COMMENT_SORT_ORDERS`], mirroring the CLI's clap `value_parser`. Returns the
/// validated value (or `None`) on success, or an `error_text` result to return.
fn validate_comment_sort(args: &Map<String, Value>) -> Result<Option<&str>, CallToolResult> {
    // STRICT: this function exists to REJECT an invalid sort, so a present
    // non-string must not slip through as "not supplied" — that would silently
    // return the default ordering to a caller who asked for a specific one.
    match optional_str_strict(args, "sort")? {
        Some(s) if COMMENT_SORT_ORDERS.contains(&s) => Ok(Some(s)),
        Some(s) => Err(error_text(&format!(
            "Invalid sort '{s}' (one of: asc, desc)"
        ))),
        None => Ok(None),
    }
}

/// Valid `status` filter values for the `upload web-list` action. The
/// `/web_upload/` list endpoint validates the filter against this exact set
/// (see the published API docs), so MCP enforces the same set the CLI's clap
/// `value_parser` does — a bad value gets a clear error instead of reaching the
/// server. Note: the spelling is `canceled` (single `l`), per the contract.
const WEB_UPLOAD_STATUSES: &[&str] = &[
    "pending",
    "queued",
    "downloading",
    "uploading",
    "complete",
    "failed",
    "canceled",
];

/// Pure predicate: has the caller explicitly opted into AI credit spend via
/// `confirm_ai_spend=true`? Accepts a native bool or the string `"true"`.
fn ai_spend_confirmed(args: &Map<String, Value>) -> bool {
    optional_bool(args, "confirm_ai_spend") == Some(true)
}

/// Error message returned when `keys` is present on `metadata-delete` but
/// empty/malformed (blank, whitespace, `[]`, an empty array, or a value that
/// is not a JSON array of key names) — which must NOT degrade into a
/// destructive delete-all.
const EMPTY_KEYS_REJECTION: &str = "keys is present but empty; omit keys entirely to delete all, \
     or supply a non-empty JSON array of key names";

/// Error message returned when `fields` is present on an extract action but
/// blank/whitespace (which must NOT silently widen scope to the full schema).
const EMPTY_FIELDS_REJECTION: &str = "fields is present but empty; omit fields entirely to extract \
     the full schema, or supply a non-empty JSON array of field names";

/// Resolve the optional `fields` extraction-scope parameter, rejecting a
/// PRESENT-but-empty value.
///
/// Per the single-file extract contract — the only extraction surface this
/// client still calls — an OMITTED `fields` is a **discovery pass** over the
/// full schema: the wide, most expensive path. See the `fields` row of the
/// extract parameter table and the section *"What `fields` does, precisely"* in
/// the published API docs.
///
/// (Cited by section rather than by line number on purpose: the reference is
/// regenerated and line numbers drift between revisions.) An absent `fields` is therefore allowed and resolves
/// to `Ok(None)`. But a PRESENT-but-blank/whitespace `fields` must NOT
/// silently widen the operation into a full extraction: a caller who supplied
/// the field (even malformed) is asking to NARROW scope, so a blank value is
/// rejected rather than spending credits on the full schema.
fn resolve_extract_fields(args: &Map<String, Value>) -> Result<Option<&str>, &'static str> {
    match args.get("fields") {
        None => Ok(None),
        Some(v) => {
            let raw = v.as_str().unwrap_or_default();
            if raw.trim().is_empty() {
                Err(EMPTY_FIELDS_REJECTION)
            } else {
                Ok(Some(raw))
            }
        }
    }
}

/// Resolve the optional `keys` parameter for `metadata-delete`, distinguishing
/// ABSENT from PRESENT-but-empty/malformed.
///
/// Per the published API docs, `keys` is a JSON-encoded array of key
/// names to delete, and an OMITTED `keys` deliberately deletes ALL metadata
/// keys for the node. An absent `keys` therefore resolves to `Ok(None)`
/// (delete-all, allowed).
///
/// A PRESENT `keys`, however, signals intent to delete a SPECIFIC subset, so it
/// must NOT silently degrade into a destructive delete-all. Because the server
/// may treat an empty array `[]` like omission (delete-all), an empty array is
/// just as dangerous as a blank value. This function rejects (with
/// `EMPTY_KEYS_REJECTION`) any present-but-empty/malformed value:
/// blank/whitespace, the literal `[]` / `[ ]`, a JSON array with zero elements,
/// or any value that does not parse as a JSON array of strings. Only a present,
/// non-empty JSON array of key names resolves to `Ok(Some(raw))`.
fn resolve_delete_keys(args: &Map<String, Value>) -> Result<Option<&str>, &'static str> {
    match args.get("keys") {
        None => Ok(None),
        Some(v) => {
            let raw = v.as_str().unwrap_or_default();
            if raw.trim().is_empty() {
                return Err(EMPTY_KEYS_REJECTION);
            }
            // `keys` is documented as a JSON-encoded array of key names. Parse
            // it and require a non-empty array whose elements are all strings.
            // An empty array `[]` is ambiguous/destructive (the server may
            // treat it like omission → delete-all), so it is rejected here.
            match serde_json::from_str::<Value>(raw) {
                Ok(Value::Array(items))
                    if !items.is_empty() && items.iter().all(Value::is_string) =>
                {
                    Ok(Some(raw))
                }
                _ => Err(EMPTY_KEYS_REJECTION),
            }
        }
    }
}

// ─── Tool Definitions ───────────────────────────────────────────────────────

/// Build the published JSON Schema for one tool parameter.
///
/// Every parameter on this surface is a **string** by convention — structured
/// values travel as JSON-encoded text (e.g. `node_ids`). `filters` is the one
/// exception: its handler ([`search_filters_arg`]) accepts a real JSON array as
/// well as an encoded string, because a predicate array is the natural way to
/// write it, and its own description tells callers to send an array.
///
/// A flat `"type": "string"` would make that array form **unreachable for any
/// schema-validating client** — the tool would advertise one thing and accept
/// another. So `filters` publishes as `array`-or-`string`. This is a strict
/// WIDENING of the previous schema: everything that validated before still
/// validates, so no existing caller can be broken by it.
fn property_schema(name: &str, desc: &str) -> Value {
    if name == "filters" {
        return json!({
            "description": desc,
            "oneOf": [
                { "type": "array", "items": { "type": "object" } },
                { "type": "string" },
            ],
        });
    }
    // `only` (the unified `search` tool's bucket filter) accepts a
    // comma-separated string OR an array of bucket names — see
    // [`only_buckets_arg`]. The published schema has to say so: a
    // schema-validating client rejects the array form against a bare
    // `"type": "string"`, which would make the accepted spelling unreachable
    // for exactly the callers that validate.
    if name == "only" {
        return json!({
            "description": desc,
            "oneOf": [
                { "type": "array", "items": { "type": "string" } },
                { "type": "string" },
            ],
        });
    }
    json!({
        "type": "string",
        "description": desc,
    })
}

/// Build the JSON Schema for a tool with an action parameter plus additional optional props.
fn action_schema(actions: &[&str], props: &[(&str, &str, bool)]) -> Arc<Map<String, Value>> {
    let actions_list: Vec<Value> = actions
        .iter()
        .map(|a| Value::String((*a).to_owned()))
        .collect();
    let mut properties = serde_json::Map::new();
    properties.insert(
        "action".to_owned(),
        json!({
            "type": "string",
            "description": "The operation to perform.",
            "enum": actions_list,
        }),
    );
    let mut required = vec![Value::String("action".to_owned())];
    for &(name, desc, is_required) in props {
        properties.insert(name.to_owned(), property_schema(name, desc));
        if is_required {
            required.push(Value::String(name.to_owned()));
        }
    }
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), Value::Array(required));
    Arc::new(schema)
}

/// Describes one MCP tool.
struct ToolDef {
    name: &'static str,
    description: &'static str,
    actions: &'static [&'static str],
    /// Extra parameters: (name, description, always-required).
    params: &'static [(&'static str, &'static str, bool)],
}

/// The canonical names of every registered tool, in `TOOL_DEFS` order.
///
/// The single source of truth for validating the `--tools` allow-list (the
/// `serve()` entry point drops any requested name not in this set, with a
/// startup warning). Includes `sign` regardless of the E-Sign kill-switch — a
/// `--tools sign` on a disabled server is a KNOWN name (so it is not warned
/// about as unknown); the E-Sign gate then hides/refuses it at runtime.
pub fn known_tool_names() -> impl Iterator<Item = &'static str> {
    TOOL_DEFS.iter().map(|def| def.name)
}

const TOOL_DEFS: &[ToolDef] = &[
    ToolDef {
        name: "auth",
        description: "Authentication: sign in, sign out, check status, manage API keys, 2FA, OAuth sessions, email/password management, token introspection. NOTE: 'email-check' is DEPRECATED and is NOT an availability check — it does no account lookup and returns success for ANY well-formed address, so a true result says nothing about whether the email is registered; to handle an already-registered email just call signup, which notifies the existing account and returns the same success. NOTE: 'signout' is LOCAL-ONLY — it clears this MCP session's in-memory token and the locally stored credential but does NOT revoke the server-side session or API key (run `fastio auth signout` in a terminal to revoke a revocable server session; use 'api-key-delete' to revoke an API key).",
        actions: &[
            "signin",
            "signout",
            "status",
            "set-api-key",
            "api-key-create",
            "api-key-list",
            "api-key-delete",
            "api-key-get",
            "api-key-update",
            "check",
            "session",
            "email-check",
            "password-reset-request",
            "password-reset",
            "password-reset-check",
            "scopes",
            "2fa-status",
            "2fa-send",
            "2fa-verify-setup",
            "oauth-list",
            "oauth-details",
            "oauth-rename",
            "oauth-revoke",
            "oauth-revoke-all",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "email",
                "Email address (signin, email-check, password-reset-request)",
                false,
            ),
            ("password", "Password (signin)", false),
            ("api_key", "API key value (set-api-key)", false),
            (
                "name",
                "API key name (api-key-create, api-key-update)",
                false,
            ),
            (
                "scopes",
                "Comma-separated scopes (api-key-create, api-key-update)",
                false,
            ),
            (
                "agent_name",
                "Agent/app name (api-key-create, api-key-update, oauth-rename)",
                false,
            ),
            (
                "expires",
                "Expiration datetime, strtotime-compatible e.g. \"2026-12-31 23:59:59 UTC\" (api-key-create, api-key-update)",
                false,
            ),
            (
                "key_id",
                "API key ID (api-key-delete, api-key-get, api-key-update)",
                false,
            ),
            (
                "code",
                "Reset code (password-reset, password-reset-check)",
                false,
            ),
            ("password1", "New password (password-reset)", false),
            ("password2", "Confirm password (password-reset)", false),
            (
                "channel",
                "2FA channel: sms, totp, whatsapp (2fa-send)",
                false,
            ),
            ("token", "2FA token (2fa-verify-setup)", false),
            (
                "session_id",
                "OAuth session ID (oauth-details, oauth-rename, oauth-revoke)",
                false,
            ),
            (
                "device_name",
                "New device label (oauth-rename; empty string clears)",
                false,
            ),
            (
                "current_session_id",
                "Session ID to keep active when revoking all others (oauth-revoke-all)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "user",
        description: "User profile: view, update, search users, manage invitations and assets, autosync, support PIN, phone validation.",
        actions: &[
            "info",
            "update",
            "search",
            "details",
            "profiles",
            "allowed",
            "org-limits",
            "shares",
            "invitations-list",
            "invitations-details",
            "invitations-accept-all",
            "asset-types",
            "asset-list",
            "asset-delete",
            "asset-upload",
            "asset-read",
            "autosync",
            "pin",
            "phone",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("first_name", "First name (update)", false),
            ("last_name", "Last name (update)", false),
            ("display_name", "Display name (update)", false),
            ("query", "Search query (search)", false),
            (
                "user_id",
                "User ID (details, asset-list, asset-read)",
                false,
            ),
            (
                "invitation_id",
                "Invitation ID (invitations-details)",
                false,
            ),
            (
                "asset_type",
                "Asset type name (asset-delete, asset-upload, asset-read)",
                false,
            ),
            ("file", "File path for upload (asset-upload)", false),
            ("output", "Output file path (asset-read)", false),
            (
                "state",
                "Autosync state: enable or disable (autosync)",
                false,
            ),
            ("country_code", "Country code e.g. 1 for US (phone)", false),
            (
                "phone_number",
                "Phone number (phone validate, and update to set the account phone)",
                false,
            ),
            (
                "phone_country",
                "Numeric phone country code to set on the account (update; send with phone_number, requires 2FA disabled)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "org",
        description: "Organizations: list, create, view, update, delete orgs; billing, members, invitations, transfer tokens, discovery, assets, workspaces, shares. DESTRUCTIVE/FLOW billing actions: 'billing-subscribe' (starts a paid subscription — returns a setup_intent + onboarding URL the user completes; the one-time client_secret and public_key are stripped from the result), 'billing-cancel' (schedules cancel at period end — REQUIRES confirm_cancel=true), 'billing-reactivate' (owner-only; reverses a scheduled cancel).",
        actions: &[
            "list",
            "create",
            "info",
            "update",
            "delete",
            "billing-details",
            "billing-plans",
            "billing-usage",
            "billing-meters",
            "billing-cancel",
            "billing-reactivate",
            "billing-members",
            "billing-subscribe",
            "billing-invoices",
            "members-list",
            "members-invite",
            "members-remove",
            "members-update-role",
            "members-details",
            "members-leave",
            "members-join",
            "transfer",
            "discover",
            "public-details",
            "limits",
            "invitations-list",
            "invitations-update",
            "invitations-delete",
            "transfer-token-create",
            "transfer-token-list",
            "transfer-token-delete",
            "transfer-claim",
            "discover-all",
            "discover-available",
            "discover-check-domain",
            "discover-external",
            "workspaces",
            "shares",
            "asset-types",
            "asset-list",
            "asset-delete",
            "create-workspace",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("org_id", "Organization ID", false),
            ("name", "Org/workspace name", false),
            ("domain", "Org domain/slug", false),
            ("description", "Org description", false),
            ("industry", "Industry", false),
            ("billing_email", "Billing email", false),
            ("homepage_url", "Homepage URL", false),
            (
                "background_color",
                "Org background color as JSON string (update)",
                false,
            ),
            (
                "background_mode",
                "Org background display mode (update)",
                false,
            ),
            (
                "use_background",
                "Enable/disable the org brand background — boolean (update)",
                false,
            ),
            ("facebook_url", "Facebook profile URL (update)", false),
            ("twitter_url", "Twitter/X profile URL (update)", false),
            ("instagram_url", "Instagram profile URL (update)", false),
            ("youtube_url", "YouTube channel URL (update)", false),
            (
                "perm_authorized_domains",
                "Authorized email domain for auto-join (update)",
                false,
            ),
            (
                "owner_defined",
                "Custom owner-defined properties as JSON string (update)",
                false,
            ),
            ("confirm", "Confirmation string (delete)", false),
            (
                "confirm_cancel",
                "REQUIRED to proceed (true/false) for billing-cancel; the action is rejected unless confirm_cancel=true (mirrors the CLI --yes gate).",
                false,
            ),
            ("email", "Member email (invite)", false),
            ("role", "Member role", false),
            ("member_id", "Member ID", false),
            ("new_owner_id", "New owner user ID (transfer)", false),
            (
                "meter",
                "Meter name, e.g. storage_bytes / bandwidth_bytes / ai_tokens (billing-meters)",
                false,
            ),
            ("start_time", "Start time (billing-meters)", false),
            ("end_time", "End time (billing-meters)", false),
            (
                "workspace_id",
                "Filter meters by workspace; XOR with share_id (billing-meters)",
                false,
            ),
            (
                "share_id",
                "Filter meters by share; XOR with workspace_id (billing-meters)",
                false,
            ),
            (
                "archived",
                "Show ARCHIVED workspaces instead of active ones (workspaces). Server default: false. Boolean (true/false).",
                false,
            ),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
            (
                "starting_after",
                "Invoice-ID cursor for the next page (billing-invoices)",
                false,
            ),
            (
                "plan_id",
                "Billing plan ID, e.g. solo_monthly / business_v2_monthly / growth_monthly (billing-subscribe)",
                false,
            ),
            ("invitation_id", "Invitation ID", false),
            (
                "state",
                "Invitation state (invitations-list, invitations-update)",
                false,
            ),
            (
                "token_id",
                "Transfer token ID (transfer-token-delete)",
                false,
            ),
            ("token", "Transfer token (transfer-claim)", false),
            (
                "domain_name",
                "Domain to check (discover-check-domain)",
                false,
            ),
            ("asset_type", "Asset type name (asset-delete)", false),
            (
                "folder_name",
                "Workspace folder name (create-workspace)",
                false,
            ),
            (
                "perm_join",
                "Join permission (create-workspace): 'Member or above' | 'Admin or above' | 'Only Org Owners' (default 'Member or above')",
                false,
            ),
            (
                "perm_member_manage",
                "Member-management permission (create-workspace): 'Member or above' | 'Admin or above' (default 'Admin or above')",
                false,
            ),
            (
                "intelligence",
                "AI indexing on the new workspace (create-workspace) — boolean (true/false). OMIT to take the platform default, which is ON; the field is optional here. (The `share` tool's identically-named argument is REQUIRED and has no default — different contract, same name.)",
                false,
            ),
            (
                "metadata_extraction",
                "Automatic metadata extraction for newly uploaded files (create-workspace) — boolean (true/false). An OPT-OUT layered under `intelligence`: OMIT to take the platform default, which is ON, and send false to withhold automatic extraction. It can never enable extraction where `intelligence` or the plan does not allow it, and it does not affect explicit per-file extraction requests (the `metadata` tool's extract actions).",
                false,
            ),
            ("accent_color", "Accent color (create-workspace)", false),
            (
                "background_color1",
                "Background color 1 (create-workspace)",
                false,
            ),
            (
                "background_color2",
                "Background color 2 (create-workspace)",
                false,
            ),
            ("user_id", "User ID (members-details)", false),
        ],
    },
    ToolDef {
        name: "workspace",
        description: "Workspaces: list, create, view, update, delete, archive/unarchive, members, shares, notes, metadata, import/export. SIDE EFFECTS — these metadata actions SPEND AI CREDITS: 'metadata-extract', 'metadata-extract-and-wait'. 'metadata-extract-and-wait' enqueues a single-file extraction and polls workspace jobs-status to a terminal state before returning.",
        actions: &[
            "list",
            "create",
            "info",
            "update",
            "delete",
            "jobs-status",
            "search",
            "limits",
            "archive",
            "unarchive",
            "members",
            "list-shares",
            "import-share",
            "available",
            "check-name",
            "create-note",
            "update-note",
            "read-note",
            "enable-import",
            "disable-import",
            "metadata-delete",
            "metadata-details",
            "metadata-extract",
            "metadata-extract-and-wait",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("org_id", "Organization ID (list, create)", false),
            ("name", "Workspace name", false),
            ("folder_name", "Folder name / slug (create)", false),
            ("description", "Description", false),
            ("intelligence", "Enable AI (true/false)", false),
            (
                "metadata_extraction",
                "Automatic metadata extraction for newly uploaded files (create, update) — boolean (true/false). An OPT-OUT layered under `intelligence`: OMIT to take the platform default, which is ON, and send false to withhold automatic extraction. It can never enable extraction where `intelligence` or the plan does not allow it, and it does not affect explicit per-file extraction requests (the `metadata` tool's extract actions). Unlike `intelligence` it deletes nothing and is not rate-limited.",
                false,
            ),
            (
                "perm_join",
                "Who can self-join — permission phrase (update)",
                false,
            ),
            (
                "perm_member_manage",
                "Who can manage members — permission phrase (update)",
                false,
            ),
            (
                "accent_color",
                "Accent color as JSON string (update)",
                false,
            ),
            (
                "background_color1",
                "Primary background color as JSON string (update)",
                false,
            ),
            (
                "background_color2",
                "Secondary background color as JSON string (update)",
                false,
            ),
            (
                "owner_defined",
                "Custom owner-defined properties as JSON string (update)",
                false,
            ),
            ("query", "Search query", false),
            (
                "search_in",
                "Where to search (search): filename | content | both (default both). Scopes the FILES bucket only — the other result buckets ignore it. filename is literal, pattern-driven name matching; content searches the AI summary plus semantic index, not raw file text.",
                false,
            ),
            (
                "name_match",
                "How the filename is matched (search): auto | exact | prefix | contains | glob (default auto). glob takes shell wildcards over the whole filename, so 'Quarterly*.pdf' matches 'Quarterly Report.pdf'; under exact/prefix/contains, '*' and '?' are ordinary characters. Send the raw pattern — the server escapes it.",
                false,
            ),
            (
                "case_sensitive",
                "Match filenames case-sensitively (search; exact/prefix/contains/glob only; default false). Do NOT lower-case the query yourself.",
                false,
            ),
            ("confirm", "Confirmation string (delete)", false),
            ("share_id", "Share ID (import-share)", false),
            ("node_id", "Node ID (notes, metadata)", false),
            ("parent_id", "Parent folder ID (create-note)", false),
            (
                "content",
                "Note markdown content, max 100KB (required for create-note)",
                false,
            ),
            (
                "if_version_id",
                "Compare-and-swap version precondition (update-note); 409 on mismatch",
                false,
            ),
            (
                "version_id",
                "Specific note version to read (read-note)",
                false,
            ),
            (
                "fields",
                "JSON-encoded array of field NAMES scoping the extraction (metadata-extract, \
                 metadata-extract-and-wait); omit for a full extraction. Naming fields makes the \
                 request EXCLUSIVE — those fields alone are written and anything else the model \
                 returns is discarded — but the file is still read in full and a named field is \
                 written only if the document contains it. Every name must be a field the \
                 workspace has already produced (an unknown name is rejected, not ignored); max \
                 50 distinct names per request.",
                false,
            ),
            (
                "node_ids",
                "JSON-encoded array (or comma-separated list) of 1-25 node IDs (metadata-details)",
                false,
            ),
            (
                "keys",
                "JSON array of keys to delete (metadata-delete)",
                false,
            ),
            (
                "poll_interval",
                "Seconds between job-status polls, 1-60, default 3 (metadata-extract-and-wait)",
                false,
            ),
            (
                "confirm_ai_spend",
                "REQUIRED to proceed (true/false) for the AI-credit-spending actions metadata-extract and metadata-extract-and-wait. These actions are rejected unless confirm_ai_spend=true. Ignored by read-only metadata actions.",
                false,
            ),
            (
                "archived",
                "Show ARCHIVED workspaces/shares instead of active ones (list, list-shares). Server default: false, so an archived item is invisible unless you ask. Boolean (true/false).",
                false,
            ),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
        ],
    },
    ToolDef {
        name: "files",
        description: "File operations: list, details, create folders, move, copy, rename, update (metadata/content), add-file, delete, restore, purge, trash, versions, search, recent, lock, transfer, read content. \
                      THREE WAYS TO GET AT A FILE'S WORDS, and they are not interchangeable: 'read' \
                      returns the file's RAW BYTES (a PDF's container, not its text); 'content' returns \
                      ONE file's EXTRACTED TEXT as ordered chunks, each with a `position` address, which \
                      is the same text the platform indexed for search and AI; 'content-many' scores UP TO \
                      10 files against one question in a single call. To quote a passage: search with \
                      output=standard, take the hit's best_chunk.position P, then call content with \
                      chunk_from=P-1 and chunk_to=P+1. \
                      search splits by target via search_in (filename | content | both): filename gives literal find-style matching with glob/prefix/contains/exact patterns over the whole name, while content searches the AI's summary and semantic index rather than raw file text. Each hit may carry best_chunk (position + indexed_version_id) naming the passage that matched — feed those to the 'content' action; when best_chunk.same_as_snippet is true its text is null at output=terse/standard, so read content_snippet instead. \
                      'transfer': do NOT re-issue it after a 5xx. A lost acknowledgement is \
                      indistinguishable from a failure here, so a repeat is a SECOND transfer, not \
                      a retry — and (reported by the platform 2026-08-26, not measured here) the \
                      endpoint has no idempotency, so repeating a FOLDER transfer is said to leave \
                      a second copy of the whole tree beside the first ('X', 'X (1)', 'X (2)') \
                      rather than replacing it. On a 5xx treat the outcome as UNKNOWN: read the \
                      destination workspace and decide from what is actually there.",
        actions: &[
            "list",
            "info",
            "create-folder",
            "move",
            "copy",
            "rename",
            "update",
            "add-file",
            "delete",
            "restore",
            "purge",
            "trash",
            "versions",
            "search",
            "recent",
            "add-link",
            "transfer",
            "version-restore",
            "lock-acquire",
            "lock-status",
            "lock-release",
            "read",
            "content",
            "content-many",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "workspace_id",
                "Workspace ID (omit when targeting a share)",
                false,
            ),
            (
                "node_id",
                "File/folder node ID. On `content` it names the ONE file or note whose extracted text to read; the multi-file form is `content-many`, which takes `nodes` instead. Two things to read off a `content` response before trusting it: `indexed: false` is a normal 200 and a statement about the FILE — that version has no extracted text (never processed, still queued, or a format that carries none) — never a platform failure; and `indexed_version_id` must MATCH the one on the search hit you came from (best_chunk.indexed_version_id) before you quote the passage, because a mismatch means the file was re-indexed and the search should be re-run.",
                false,
            ),
            (
                "folder",
                "Parent folder ID (list, create-folder, add-file); defaults to root",
                false,
            ),
            ("name", "Folder/file name (create-folder, add-file)", false),
            (
                "force",
                "create-folder: always create (auto-rename on collision) instead of returning the existing folder",
                false,
            ),
            ("to", "Target parent folder ID (move, copy)", false),
            ("new_name", "New name (rename)", false),
            (
                "from",
                "JSON content source for update: {\"type\":\"upload\",\"upload\":{\"id\":\"…\"}} or {\"type\":\"hash\",\"hash\":{\"hash\":\"…\",\"hash_type\":\"sha256\"}}. add-file is workspace-scoped and supports ONLY an upload source — use upload_id; the hash form is for update only",
                false,
            ),
            (
                "metadata_title",
                "Title override, max 50 chars (update)",
                false,
            ),
            (
                "metadata_short",
                "Short-description override, max 2048 chars (update)",
                false,
            ),
            (
                "if_version",
                "Compare-and-swap precondition (update): the version id this update was derived from. The server rejects the update if that is no longer current, so a concurrent writer's change is not silently replaced. Use the version you actually READ — the server only checks that the id is current, so a base that is present but wrong passes and overwrites",
                false,
            ),
            (
                "upload_id",
                "Completed upload session ID to attach (add-file) — the ONLY supported add-file source",
                false,
            ),
            (
                "hash",
                "Content hash to dedup against. NOT supported by workspace add-file (use upload_id); to dedup by hash use update with from={type:hash}",
                false,
            ),
            (
                "hash_type",
                "Hash algorithm (md5/sha1/sha256/sha384) paired with hash; see hash — not supported by workspace add-file. \
                 YOU SEND 'hash_type' AND READ 'hash_algo' BACK: the request field is hash_type, the same value comes back \
                 on the node as hash_algo, so looking for hash_type in a response finds nothing and that absence says \
                 NOTHING about the file. Deliberate asymmetry, translated on write (not independently \
                 verified here)",
                false,
            ),
            (
                "query",
                "The query text. On `search` it is the search query. On `content` it selects RELEVANCE MODE: this one file's own chunks are ranked against it and the best are returned with FULL text and a numeric score, ignoring max_bytes so a hit is never truncated away (1-512 characters; it cannot be combined with page, chunk_from/chunk_to or cursor, and it defaults limit to 3). On `content-many` it is REQUIRED — it is the only way that route selects text.",
                false,
            ),
            (
                "limit",
                "Max results (search). On `content` it is CHUNKS PER RESPONSE, 1-20 (server default 5, or 3 in relevance mode); on `content-many` it is chunks per FILE, 1-20 (server default 3).",
                false,
            ),
            ("offset", "Result offset (search)", false),
            (
                "nodes",
                "content-many ONLY: 1-10 file node IDs, comma-separated, scored against `query` in ONE request — the way to assemble prompt context from several candidate files without ten round trips. WORKSPACE ONLY (send workspace_id, never share_id). Scores are computed per file and are NOT comparable across files, so never merge the per-file chunk lists and re-sort them by score. The response is {q, limit, nodes: {<node id>: {…chunks…}}, missing: [{id, reason}]} with reason one of not_found | trashed | not_text; a file that answered with an empty chunks list simply holds nothing matching the query. A `position` returned here can be sent straight back to `content` as chunk_from.",
                false,
            ),
            (
                "page",
                "content ONLY: return the chunks whose [start_page, end_page] range OVERLAPS this 1-based page. The unit is a chunk, not a page — a chunk can span two pages or split one — so this returns whole chunks, not a page's exact text. Read `page_addressable` in the response first: formats with no pages (spreadsheets, plain text, code, notes) answer an empty chunks list. One window selector at a time: not with query, chunk_from/chunk_to or cursor.",
                false,
            ),
            (
                "chunk_from",
                "content ONLY: start of an inclusive `position` range, below 10000. POSITION IS THE ADDRESS — it is the field to page and re-read by; `chunk_index` is a nullable legacy spelling of the same idea and `sequence` is a correlation coordinate, not an address. Given a search hit's best_chunk.position P, read the passage in context with chunk_from=P-1 and chunk_to=P+1; when that position is null the passage has no address — use query on the file instead. To go further into a large file than 10000, walk with cursor instead. One window selector at a time: not with query, page or cursor.",
                false,
            ),
            (
                "chunk_to",
                "content ONLY: end of that inclusive `position` range. Requires chunk_from, must be greater than or equal to it, and is subject to the same 10000 ceiling.",
                false,
            ),
            (
                "max_bytes",
                "content / content-many: UTF-8 byte budget over the text in one response, 1024-262144 (server default 32768; PER FILE on content-many, so ten files at the maximum is a deliberate opt-in to roughly 2.5 MiB). Applied in the ordered modes only — deliberately NOT in relevance mode, where a hit is never silently dropped.",
                false,
            ),
            (
                "files_scope",
                "Comma-separated nodeId:versionId pairs to narrow search (search)",
                false,
            ),
            (
                "folders_scope",
                "Comma-separated nodeId:depth pairs to narrow search (search)",
                false,
            ),
            (
                "filters",
                "Metadata predicate filter (search; WORKSPACE ONLY). JSON array of \
                 {field, operator, value} clause objects, AND-combined, e.g. \
                 [{\"field\":\"category\",\"operator\":\"=\",\"value\":\"Legal\"}]. Operators: \
                 = != < <= > >= in exists not_exists confidence_gte (exists/not_exists take no \
                 value). Only files that ALREADY have extracted metadata are candidates, so an \
                 empty result means 'nothing extracted matches', NOT 'no such files exist'. \
                 Intersects with files_scope; the server currently refuses filters together with \
                 folders_scope. CHECK the top-level metadata_filter block in the response: if it \
                 is ABSENT the filter did not run and the results are UNFILTERED",
                false,
            ),
            (
                "details",
                "Enrich each search hit with the full node resource (search)",
                false,
            ),
            (
                "output",
                "Verbosity: terse | standard | full. On `search` it shapes content_snippet — and note that at terse and standard a hit's best_chunk.text is null when best_chunk.same_as_snippet is true, so read content_snippet for that passage rather than concluding the chunk is empty. On `content` / `content-many` it shapes the returned chunks: `terse` returns the CHUNK MAP with no text at all, which is the cheap way to find out how a file is laid out before spending a read on it.",
                false,
            ),
            (
                "search_in",
                "Where to search (search): filename | content | both (default both). filename matches the file's NAME literally and is the predictable, pattern-driven mode — use it for find/grep-style lookups. content searches the AI's understanding of the file (its generated summary plus semantic matches); it is NOT a raw-text grep and will not reliably locate a literal string inside a document.",
                false,
            ),
            (
                "name_match",
                "How the filename is matched (search; applies when search_in is filename or both): auto | exact | prefix | contains | glob (default auto). auto is relevance ranking. glob takes shell wildcards over the WHOLE filename — '*' matches any run, '?' exactly one — so 'Quarterly*.pdf' matches 'Quarterly Report.pdf' across the space. Under exact/prefix/contains, '*' and '?' are ORDINARY characters, which makes contains the way to find a filename that itself contains a '*'. Send the user's raw pattern; the server handles escaping, so never escape it yourself.",
                false,
            ),
            (
                "case_sensitive",
                "Match filenames case-sensitively (search; applies to exact/prefix/contains/glob only; default false). Case-insensitive matching folds non-ASCII (accented, umlauted) filenames correctly — do NOT lower-case the query yourself.",
                false,
            ),
            ("sort_by", "Sort field (list)", false),
            ("sort_dir", "Sort direction: asc/desc", false),
            ("page_size", "Page size (list, trash)", false),
            (
                "cursor",
                "Pagination cursor (list, trash). On `content` it WALKS one file: pass back the `next_cursor` from the MOST RECENT response VERBATIM — never a value you built, never one saved from an earlier walk — and stop when next_cursor is null. It carries the file version it was issued against, so a walk can never straddle two versions: if the file is replaced mid-walk the next call answers an empty chunks list with next_cursor null. Not valid with query, and not available on content-many.",
                false,
            ),
            (
                "type",
                "Filter by node type: file/folder/link/note (recent)",
                false,
            ),
            // ONE entry only — `action_schema` inserts params into a map by
            // name, so a second `share_id` here would silently overwrite this
            // description in the advertised schema and the loser would never
            // ship. Both consumers are covered in this one string.
            (
                "share_id",
                "Share ID. On `add-link`, the share to link into. On EVERY OTHER action it targets SHARE storage instead of a workspace — supply this OR workspace_id, never both, and never an empty string. Share storage follows identical patterns to workspace storage for the common operations (list, info, create-folder, move, copy, delete, restore, purge, trash, versions, version-restore, recent, read, update, add-file, lock-acquire/status/release) and each of those routes is documented for shares. `content` also works on a share, but it requires DOWNLOAD permission rather than mere view — extracted text is the interior of the file — while `content-many` is WORKSPACE ONLY and rejects a share_id. Two search caveats: workspace-backed (folder) shares do not support search at all, and `filters` is workspace-only — a share ignores it and still answers 200, so the response is checked and a warning attached when the predicate did not run.",
                false,
            ),
            ("to_workspace", "Target workspace ID (transfer)", false),
            ("version_id", "Version ID (version-restore)", false),
            (
                "duration",
                "Lock duration in seconds, 60-3600 (lock-acquire)",
                false,
            ),
            (
                "client_info",
                "Lock client metadata as a JSON object (lock-acquire)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "search",
        description: "Unified search — ONE query across everything in a workspace or share, grouped into buckets: `files` (filename + content, blended with semantic matches when workspace intelligence is on), `metadata` (extracted metadata field values; workspace only), and `comments`. START HERE for any 'find X' request. Each bucket paginates independently and reports its own health, so a `degraded` bucket returns empty while the others still answer normally. Use `files search` instead only when you specifically want a single flat file list with no metadata or comment hits.",
        actions: &[
            "workspace",
            "share",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("workspace_id", "Workspace ID (action=workspace)", false),
            ("share_id", "Share ID (action=share)", false),
            (
                "query",
                "Search query (max 1024 characters; must not be blank)",
                true,
            ),
            (
                "only",
                "Bucket names to KEEP in the response — a comma-separated string ('files,comments') or an array (['files','comments']). Client-side display filter: the server always searches every applicable bucket, so this saves tokens, not server work. A value of any other type is rejected rather than silently ignored.",
                false,
            ),
            (
                "files_offset",
                "Offset for the files bucket (default 0)",
                false,
            ),
            (
                "files_limit",
                "Page size for the files bucket (default 25)",
                false,
            ),
            (
                "metadata_offset",
                "Offset for the metadata bucket (default 0; workspace only — never sent on a share)",
                false,
            ),
            (
                "metadata_limit",
                "Page size for the metadata bucket (default 25; workspace only — never sent on a share)",
                false,
            ),
            (
                "comments_offset",
                "Offset for the comments bucket (default 0)",
                false,
            ),
            (
                "comments_limit",
                "Page size for the comments bucket (default 25)",
                false,
            ),
            (
                "details",
                "true adds `metadata_facts` — the file's EXTRACTED metadata field values — to file and note items in the FILES bucket. WORKSPACE ONLY: the share route accepts the parameter and never returns facts, and folder and link items never carry the key on either route. Costs at most one batched facts read per files-bucket page, not one per item.",
                false,
            ),
            (
                "search_in",
                "FILES BUCKET ONLY: filename | content | both (default both). filename is literal, pattern-driven name matching; content searches the AI summary plus semantic index, not raw file text. The metadata and comments buckets ignore this.",
                false,
            ),
            (
                "name_match",
                "FILES BUCKET ONLY: auto | exact | prefix | contains | glob (default auto). glob takes shell wildcards over the whole filename; under exact/prefix/contains, '*' and '?' are ordinary characters. Send the raw pattern — the server escapes it.",
                false,
            ),
            (
                "case_sensitive",
                "FILES BUCKET ONLY: match filenames case-sensitively (exact/prefix/contains/glob only; default false). Do NOT lower-case the query yourself.",
                false,
            ),
        ],
    },
    ToolDef {
        name: "upload",
        description: "Upload files: text content, URL import, session management, chunk ops, web imports, limits. \
                      UPLOADING INTO A CLOUD-SYNC GRAFT IS NOT THE SAME AS ORDINARY STORAGE — success here \
                      means the bytes reached Fastio, NOT the provider. Each file must additionally be \
                      written back, and write-backs serialise PER SOURCE, so several files form a QUEUE \
                      rather than pushing in parallel. The upload response does NOT carry import_state: read \
                      the node with files action=info — this server sends no detail parameter, so you get the \
                      server's DEFAULT shape, which carries import_state for an imported node (measured \
                      2026-08-22 through the CLI: import_state is ABSENT at detail=terse and present at \
                      standard and above; you cannot select terse here, so absence from THIS tool means the \
                      node is not imported rather than that a tier hid it) and check \
                      import_state.status — 'completed' = bytes ARE at the provider; 'pending'/'uploading' = \
                      still queued, WAIT, do not re-upload and do not call import refresh (that competes for \
                      the same per-source lock and drains SLOWER); 'failed'/'conflict'/'canceled' = never \
                      arrived, re-upload. 'synced' is NOT the success value for a file YOU uploaded, and NOT \
                      evidence it came from the provider: a file you uploaded reads 'synced' whenever the \
                      write-back was never enqueued or bailed (coalesced, lock not acquired, job creation \
                      failing), because the node is marked imported BEFORE the enqueue. Reading it as success \
                      passes a file that was never pushed — that is the danger, not the waiting. Not \
                      independently verified here: \
                      'synced' is a fall-through DEFAULT for a node with no stamped write-back status while \
                      'completed' is stamped — so do not treat the two as peer values. import \
                      list-writebacks gives the source-level queue, but the route behind it returns an \
                      EMPTY list for a hyphenated source id (measured 2026-08-22: 0 rows formatted vs 26 \
                      canonical) — this server strips the hyphens for you, a direct API call does not, and \
                      an empty array with a 200 reads exactly like 'nothing outstanding'.",
        actions: &[
            "text",
            "url",
            "create-session",
            "finalize",
            "status",
            "cancel",
            "list-sessions",
            "cancel-all",
            "chunk-status",
            "chunk-delete",
            "web-list",
            "web-cancel",
            "web-status",
            "limits",
            "algos",
            "extensions",
            "stream",
            "create-stream-session",
            "stream-send",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("folder", "Target folder ID (defaults to root)", false),
            (
                "name",
                "Filename (text, create-session, create-stream-session, stream)",
                false,
            ),
            (
                "content",
                "Text/UTF-8 content (text upload, stream, stream-send). Binary data is not supported via MCP.",
                false,
            ),
            ("url", "Source URL (url import)", false),
            (
                "upload_key",
                "Upload key/ID (finalize, status, cancel, chunk-status, chunk-delete, stream-send)",
                false,
            ),
            ("filesize", "File size in bytes (create-session)", false),
            ("chunk_num", "Chunk number (chunk-delete)", false),
            ("upload_id", "Upload ID (web-cancel, web-status)", false),
            (
                "max_size",
                "Max upload size in bytes (create-stream-session, stream)",
                false,
            ),
            (
                "hash",
                "Pre-computed hash of file content (stream, stream-send)",
                false,
            ),
            (
                "hash_algo",
                "Hash algorithm e.g. sha256 (stream, stream-send)",
                false,
            ),
            (
                "limit",
                "Maximum number of jobs to return (web-list)",
                false,
            ),
            ("offset", "Offset for pagination (web-list)", false),
            (
                "status",
                "Filter web-import jobs by status (web-list)",
                false,
            ),
            (
                "plan",
                "Plan whose extension limits to return (extensions)",
                false,
            ),
            (
                "limit_action",
                "limits context selector: create or update (limits). NOTE: distinct \
                 from the tool's routing `action` (always 'limits' here). instance_id \
                 is required for both create and update; file_id is also required for update.",
                false,
            ),
            (
                "org",
                "Organization ID for limit resolution (limits, no limit_action)",
                false,
            ),
            (
                "instance_id",
                "Target workspace/share ID for limits (limits, limit_action=create or update)",
                false,
            ),
            (
                "folder_id",
                "Target folder OpaqueId or root (limits)",
                false,
            ),
            (
                "file_id",
                "File ID for update-context limits (limits, limit_action=update)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "download",
        description: "Downloads: get file download URLs, folder ZIP URLs. file-url returns a secret-bearing URL (short-lived scoped read token) — do not log or share it.",
        actions: &[
            "file-url", "zip-url",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("node_id", "File/folder node ID", false),
            ("version_id", "Version ID (file-url)", false),
        ],
    },
    ToolDef {
        name: "share",
        description: "Shares (data rooms): list, create, view, update, delete, archive/unarchive, password-auth, guest-auth, discovery.",
        actions: &[
            "list",
            "create",
            "info",
            "update",
            "delete",
            "files-list",
            "members-list",
            "members-add",
            "public-details",
            "archive",
            "unarchive",
            "password-auth",
            "guest-auth",
            "available",
            "check-name",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("share_id", "Share ID", false),
            ("workspace_id", "Workspace ID (create)", false),
            (
                "name",
                "Share display title (create) / display name (update)",
                false,
            ),
            ("title", "Display title (update); \"null\" to clear", false),
            (
                "custom_name",
                "URL-friendly custom name (create/update); \"null\" to clear",
                false,
            ),
            (
                "share_type",
                "Direction: send, receive, exchange (create default exchange)",
                false,
            ),
            ("description", "Description", false),
            ("access_options", "Access options", false),
            (
                "invite",
                "Who can manage invitations: owners, guests",
                false,
            ),
            (
                "storage_mode",
                "Storage mode: independent (portal, default) or workspace_folder (create)",
                false,
            ),
            (
                "folder_node_id",
                "Backing workspace folder opaque ID (workspace_folder mode)",
                false,
            ),
            (
                "create_folder",
                "Create a new backing folder (true/false, with folder_name)",
                false,
            ),
            ("folder_name", "Name for the new backing folder", false),
            (
                "password",
                "Share password (Send + 'Anyone with the link')",
                false,
            ),
            (
                "expires",
                "Expiration datetime YYYY-MM-DD HH:MM:SS (portal mode); \"null\" to clear",
                false,
            ),
            (
                "notify",
                "Notification: never, notify_on_file_received, notify_on_file_sent_or_received",
                false,
            ),
            (
                "download_enabled",
                "Allow downloads (true/false, legacy — prefer download_security)",
                false,
            ),
            (
                "download_security",
                "Download security level: high (disabled), medium (preview only for guests), off (no restrictions)",
                false,
            ),
            ("comments_enabled", "Allow comments (true/false)", false),
            (
                "guest_chat_enabled",
                "Enable guest AI chat (true/false)",
                false,
            ),
            ("display_type", "Visual display mode: grid, list", false),
            ("workspace_style", "Workspace visual style", false),
            (
                "anonymous_uploads_enabled",
                "Allow anonymous uploads (true/false)",
                false,
            ),
            (
                "intelligence",
                "Enable AI intelligence (true/false; create defaults to false — required server-side)",
                false,
            ),
            (
                "accent_color",
                "Accent color (JSON color object), or \"null\"",
                false,
            ),
            (
                "background_color1",
                "Primary background color (JSON), or \"null\"",
                false,
            ),
            (
                "background_color2",
                "Secondary background color (JSON), or \"null\"",
                false,
            ),
            (
                "background_image",
                "Background image selection (numeric)",
                false,
            ),
            (
                "link_1",
                "Custom link #1 (JSON link object), or \"null\"",
                false,
            ),
            (
                "link_2",
                "Custom link #2 (JSON link object), or \"null\"",
                false,
            ),
            (
                "link_3",
                "Custom link #3 (JSON link object), or \"null\"",
                false,
            ),
            (
                "owner_defined",
                "Custom owner-defined properties (JSON or \"null\")",
                false,
            ),
            (
                "share_link_node_id",
                "Remove the workspace share-link node (update; pass \"null\" — the only accepted value)",
                false,
            ),
            ("confirm", "Confirmation (delete)", false),
            ("folder", "Folder ID (files-list)", false),
            (
                "email",
                "Member email (invite) or 19-digit user ID (members-add)",
                false,
            ),
            ("role", "Member role: admin, member, guest, view", false),
            (
                "notify_options",
                "Member notification preference (members-add)",
                false,
            ),
            (
                "force_notification",
                "Resend notification email (true/false, members-add)",
                false,
            ),
            (
                "message",
                "Invitation email message (members-add by email)",
                false,
            ),
            (
                "invitation_expires",
                "Invitation expiration datetime (members-add by email)",
                false,
            ),
            ("sort_by", "Sort field (files-list)", false),
            ("sort_dir", "Sort direction (files-list)", false),
            ("page_size", "Page size", false),
            ("cursor", "Pagination cursor", false),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
        ],
    },
    ToolDef {
        name: "ripley",
        description: "Offload multi-step work to Ripley, Fast.io's AI agent: ask a question and get the answer (ask — creates a chat and waits), lower-level chat create/list/details/update/delete/publish/cancel, message send/list/details/read, generate AI shares (share-generate), transactions, and autotitle. Searching is NOT a Ripley action. (Formerly the `ai` tool; `ai` still works as a hidden alias.)",
        actions: &[
            "ask",
            "chat-create",
            "chat-list",
            "chat-details",
            "chat-update",
            "chat-delete",
            "chat-publish",
            "chat-cancel",
            "message-send",
            "message-list",
            "message-details",
            "message-read",
            "share-generate",
            "transactions",
            "autotitle",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "context_type",
                "Context: workspace or share (chat/message/share/transactions/autotitle)",
                false,
            ),
            ("context_id", "Workspace or share ID", false),
            (
                "query_text",
                "Question text (ask, chat-create, message-send)",
                false,
            ),
            (
                "idempotency_key",
                "Replay guard for a turn (ask, chat-create, message-send; max 64 chars). \
                 One is generated for you, so you normally omit this. On a retryable 409 the \
                 error hands you the key that was used — re-send that SAME key to replay the \
                 turn instead of creating (and billing) a second one.",
                false,
            ),
            (
                "type",
                "[deprecated/ignored] Chat type — dead on the migrated /ai/agent/ \
                 endpoint; accepted but not sent",
                false,
            ),
            ("chat_id", "Chat ID", false),
            ("message_id", "Message ID", false),
            ("name", "Chat name (chat-create, chat-update)", false),
            (
                "privacy",
                "private or public (chat-create; workspace-only, ignored for share with a warning)",
                false,
            ),
            (
                "kind",
                "user, agent, or all. Chat kind on chat-create/ask (workspace-only, ignored \
                 for share with a warning); kind filter on chat-list (user|agent|all)",
                false,
            ),
            (
                "no_wait",
                "ask: return chat/message IDs immediately without waiting for the answer",
                false,
            ),
            (
                "files_scope",
                "Comma-separated file nodeId[:versionId] pairs (chat-create, message-send, ask). \
                 Version is optional (auto-resolved). Becomes file items in the `references` array; \
                 may be combined with folders_scope/files_attach.",
                false,
            ),
            (
                "folders_scope",
                "Comma-separated folder nodeId pairs (chat-create, message-send, ask). Any \
                 :depth suffix is ignored. Becomes folder items in the `references` array; \
                 may be combined with files_scope/files_attach.",
                false,
            ),
            (
                "files_attach",
                "Comma-separated file nodeId[:versionId] pairs (chat-create, message-send, ask). \
                 Collapses to the same file items in the `references` array as files_scope; \
                 may be combined with files_scope/folders_scope.",
                false,
            ),
            (
                "node_ids",
                "Comma-separated file IDs (share-generate; converted to a JSON `files` array)",
                false,
            ),
            (
                "files",
                "Comma-separated file IDs (share-generate; converted to a JSON `files` array)",
                false,
            ),
            (
                "personality",
                "[deprecated/ignored] Response style — dead on the migrated /ai/agent/ \
                 endpoint; accepted but not sent",
                false,
            ),
            (
                "include_deleted",
                "List deleted chats instead (chat-list)",
                false,
            ),
            ("context", "Hint for autotitle", false),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
        ],
    },
    ToolDef {
        name: "member",
        description: "Members: list, add, remove, update, details, transfer-ownership, leave, join, join-invitation.",
        actions: &[
            "list",
            "add",
            "remove",
            "update",
            "info",
            "transfer-ownership",
            "leave",
            "join",
            "join-invitation",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("entity_type", "Entity type: workspace or share", false),
            ("entity_id", "Workspace or share ID", false),
            ("member_id", "Member ID", false),
            ("email", "Member email (add)", false),
            ("role", "Member role", false),
            ("invitation_key", "Invitation key (join-invitation)", false),
            (
                "invitation_action",
                "accept or decline (join-invitation)",
                false,
            ),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
        ],
    },
    ToolDef {
        name: "comment",
        description: "Comments: list, list-all, create, reply, update, delete, bulk-delete, details, reaction-add, reaction-remove, list-attachments, attach, detach. create accepts an optional anchoring reference + properties metadata and inline attachment(s) (target_id / target_ids, ≤25). attach/detach/list-attachments are author-only (the server enforces).",
        actions: &[
            "list",
            "list-all",
            "create",
            "reply",
            "update",
            "delete",
            "bulk-delete",
            "details",
            "reaction-add",
            "reaction-remove",
            "list-attachments",
            "attach",
            "detach",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "entity_type",
                "Entity type: workspace, share, or fileshare. `fileshare` is the File Share recipient's own comment surface and REQUIRES node_id — a File Share comment always targets a specific file, never the whole container, so `fileshare` is rejected on action='list-all'.",
                false,
            ),
            (
                "entity_id",
                "Workspace, share, or File Share ID (matches entity_type)",
                false,
            ),
            ("node_id", "File node ID", false),
            ("text", "Comment text", false),
            ("comment_id", "Comment ID", false),
            (
                "comment_ids",
                "Comma-separated comment IDs (bulk-delete)",
                false,
            ),
            ("emoji", "Emoji character (reaction-add)", false),
            (
                "reference",
                "Anchoring reference as a JSON object (create)",
                false,
            ),
            (
                "properties",
                "Arbitrary metadata as a JSON object (create)",
                false,
            ),
            (
                "target_id",
                "Single attachment object ID (create inline / attach / detach)",
                false,
            ),
            (
                "target_ids",
                "Comma-separated attachment object IDs, ≤25 (create inline / attach)",
                false,
            ),
            (
                "sort",
                "Sort order (list): asc or desc (default asc)",
                false,
            ),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
        ],
    },
    ToolDef {
        name: "event",
        description: "Events & audit log: search, summarize, details, ack, activity-list, activity-poll. PAGING: STOP ON AN EMPTY PAGE, NEVER A SHORT ONE. SUMMARIZE SPENDS AI CREDITS AND SUMMARISES ONLY WHAT THIS CALLER CAN SEE: it is built from the same page search returns, after inaccessible rows are dropped, so on partial access it narrates a fraction of what happened in confident prose with NO marker that anything was withheld — and the reported date range is derived from the surviving rows, so it can assert a period it never saw. Credits are spent the same either way. Do not present a summary as complete without checking search with identical filters first. Rows you cannot see are removed AFTER the page is cut, so a short page does NOT mean end-of-data — the usual loop-while-page-size-equals-limit exits early and reports success, silently reading a fraction of what exists. A small count is therefore not a measure of how much happened. Narrow with event/category so the page is spent on rows of the kind you asked for. search AND summarize forward the same audit-log filters — visibility (external_audit_log|external), created_min/created_max time bounds, parent_event_id (serial/batch drill; cannot combine with filters other than acknowledged/limit/offset), acknowledged, calling_user_id (distinct from user_id), object_id, and subcategory; summarize additionally takes user_context. Pass the global --detail (terse|standard|full) on the CLI to select the server output verbosity.",
        actions: &[
            "search",
            "summarize",
            "details",
            "ack",
            "activity-list",
            "activity-poll",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("share_id", "Share ID", false),
            (
                "user_id",
                "Narrows by user id, but DOES NOT RELIABLY IDENTIFY ANYONE — do not read a match as \
                 'this event is about that person'. Reported from the platform side 2026-08-24 (not \
                 measurable here): the column is derived rather than emitted, and the derivation \
                 tracks WHICH MEMBER A QUERY HAPPENED TO RETURN FIRST for the workspace, not who the \
                 event concerns. Measured there across all event families: 135 event types affected, \
                 and on 107 of 136 workspaces the subject NEVER VARIES — it is effectively a \
                 per-workspace constant. NEVER present a user_id-filtered list to a user as that \
                 person's activity, \
                 and never answer 'who is this about' with it. Fine as an OPAQUE narrowing filter where \
                 the meaning of the match does not matter. It is also NOT the actor — that is \
                 calling_user_id, which has its own caveats",
                false,
            ),
            ("org_id", "Organization ID", false),
            ("event_id", "Event ID (details, ack)", false),
            ("event", "Event type filter", false),
            (
                "category",
                "Category filter. Cloud sync/import activity is category 'import' — the LEGACY name, \
                 persisted on every historical row. 'cloudsync' is NOT a category and matches nothing \
                 here (it is the activity-poll field); the subcategory is 'cloud_import', a third \
                 spelling of the same domain. Filtering by event name — e.g. \
                 'import_source_sync_failed' — is exact and avoids the mismatch entirely. \
                 IMPORT EVENTS REQUIRE ADMIN-LEVEL WORKSPACE ACCESS: owner and admin receive them, \
                 member and viewer receive NONE. The rows are fetched then dropped by a per-row \
                 permission check, so a member gets a clean EMPTY result, not an error — and reads \
                 OTHER event families from that same workspace normally, which is what makes the \
                 zero so easy to misread. So do NOT report 'no import activity' from a member or \
                 viewer token: report that this caller lacks the access to see them, and that an \
                 admin-level token may return more. BUT A MEMBER IS NOT BLIND TO SYNC ACTIVITY: \
                 files arriving via a graft also emit workspace_storage_file_sync_added (and \
                 _sync_updated for changes), which carry MEMBER permission under category \
                 workspace — NOT import — so they return for a member token that gets nothing \
                 from the import family. Use those for 'did anything land'; use the import \
                 family for what a SOURCE is doing. REMOVALS EMIT NEITHER: there is no \
                 sync-delete counterpart, so a file disappearing from a graft is invisible on \
                 this route to every caller. \
                 AND 'node' IS NOT FILE ACTIVITY — enumerate, do not infer from the name: measured \
                 2026-08-22, every node row was node_ai_summary_created (the AI pipeline). That category \
                 has FOUR members, all AI-pipeline, and this API can only ever show ONE — the other three \
                 (node_ai_state_set, node_ai_added_to_rag, node_ai_removed_from_rag) are internal-visibility \
                 and reach no caller, so enumerating this category through the API cannot see three quarters \
                 of it (the full member list is not enumerable through the API). Meanwhile file \
                 operations are workspace_storage_file_added/_deleted/_moved/_sync_added/_sync_updated \
                 under category workspace. So category=node answers a question about the AI pipeline, \
                 and asking it for uploads or deletes returns plausible-looking rows about something \
                 else. Contents of the other categories (metadata, share, user, apps, ai, email, org, \
                 invitation, workflow) were NOT enumerated — treat those names as labels you have not \
                 opened.",
                false,
            ),
            ("subcategory", "Subcategory filter", false),
            (
                "parent_event_id",
                "Parent event ID for serial/batch drill (search, summarize)",
                false,
            ),
            (
                "calling_user_id",
                "Triggering user ID (search, summarize). NOT a synonym for user_id: it returns NO 'node' events. \
                 Measured 2026-08-22, one workspace, paged to an EMPTY page: 1492 rows across import/metadata/share/ \
                 user/workflow/workspace and not one node row, where user_id for the SAME user returned 421 rows \
                 including them. THAT IS MOSTLY CORRECT BEHAVIOUR, NOT LOST FILE ACTIVITY — measured on the same \
                 data, every node row was node_ai_summary_created (the AI pipeline, which has no acting user). USER \
                 FILE ACTIVITY IS CATEGORY 'workspace' (workspace_storage_file_added/_deleted/_moved/_sync_added/ \
                 _sync_updated) and this filter DOES return it. The surviving caution is narrower: a by-actor filter \
                 omits whole event families, so do not use one to audit everything that happened in a \
                 workspace. AN ABSENT ACTOR DOES NOT MEAN NOBODY WAS RESPONSIBLE: sync/import/write-back \
                 lifecycle events and authorizing or revoking a storage provider carry no actor even though \
                 a specific user caused them — the split runs by event TYPE, so the FILE a sync lands is \
                 attributed while the SYNC that landed it is not (reported from the platform side, not \
                 measurable here)",
                false,
            ),
            (
                "object_id",
                "Related object ID filter (search, summarize)",
                false,
            ),
            (
                "visibility",
                "Audit-log filter: external_audit_log or external (search, summarize)",
                false,
            ),
            (
                "acknowledged",
                "Acknowledgment status: true or false (search, summarize)",
                false,
            ),
            (
                "created_min",
                "Lower bound for event creation time (search, summarize)",
                false,
            ),
            (
                "created_max",
                "Upper bound for event creation time (search, summarize)",
                false,
            ),
            (
                "user_context",
                "Focus guidance for summary (summarize)",
                false,
            ),
            ("profile_id", "Profile ID (activity-list)", false),
            ("entity_id", "Entity ID (activity-poll)", false),
            ("lastactivity", "Last activity timestamp", false),
            ("wait", "Long-poll seconds (activity-poll)", false),
            ("cursor", "Cursor (activity-list)", false),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
        ],
    },
    ToolDef {
        name: "dashboard",
        description: "Per-workspace dashboard: the calling member's ranked, paginated feed of ACTIONABLE cards (@mentions, file activity, file versions, synthesis, and — when E-Sign is enabled — pending signatures). Actions: get (read the feed; paginate with limit 1-200 / offset), dismiss (hide a card from YOUR feed — permanently, or snooze it until a future time via snooze_until 'YYYY-MM-DD HH:MM:SS UTC'), undismiss (restore a card; idempotent). Dismiss / snooze / undismiss are PER-MEMBER and OUT-OF-BAND — they change only YOUR view and never advance, resolve, or modify the underlying card subject. card_key comes from a card's card_key field (URL-encoding is handled for you). Signature cards carry a sign-ceremony link for their primary action (requires E-Sign to be enabled).",
        actions: &[
            "get",
            "dismiss",
            "undismiss",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "workspace_id",
                "Workspace ID (19-digit) or folder name",
                false,
            ),
            (
                "card_key",
                "Card key from the feed (dismiss / undismiss), e.g. mention:123…",
                false,
            ),
            (
                "snooze_until",
                "Snooze expiry 'YYYY-MM-DD HH:MM:SS UTC' (dismiss; must be in the future). Omit for a permanent dismiss.",
                false,
            ),
            ("limit", "Cards per page, 1-200 (get)", false),
            ("offset", "Cards to skip for pagination (get)", false),
        ],
    },
    ToolDef {
        name: "invitation",
        description: "Invitations: list user invitations, list-entity (a workspace/share's invitations by optional state), accept, decline, update (state/role/notification/expiration), delete.",
        actions: &[
            "list",
            "list-entity",
            "accept",
            "decline",
            "update",
            "delete",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "invitation_id",
                "Invitation ID or email (accept/decline/update/delete)",
                false,
            ),
            (
                "entity_type",
                "Entity type: workspace or share (list-entity/accept/decline/update/delete)",
                false,
            ),
            (
                "entity_id",
                "Entity ID (list-entity/accept/decline/update/delete)",
                false,
            ),
            (
                "state",
                "State filter (list-entity) or new state (update): pending, accepted, declined",
                false,
            ),
            (
                "role",
                "Updated permission role (update): admin, member, guest, view",
                false,
            ),
            (
                "notify_options",
                "Updated notification preference (update)",
                false,
            ),
            (
                "expires",
                "Updated membership expiration datetime (update)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "preview",
        description: "Previews: get preview URLs and image transform URLs for files. For transform, the response is {transform_name, token, read_url}; read_url is the /read/ URL carrying the token AND the image params (so they actually apply when fetched). Both read_url and token are secret-bearing read capabilities — do not log or share them.",
        actions: &[
            "get",
            "thumbnail",
            "transform",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("node_id", "File node ID", false),
            (
                "preview_type",
                "Preview type (get; REQUIRED for get): bin, thumbnail, image, hlsstream, pdf, spreadsheet, audio, mp4",
                false,
            ),
            (
                "transform_name",
                "Transform name (transform) — must be 'image' (the only valid value; default 'image')",
                false,
            ),
            ("width", "Target width in px (transform; integer)", false),
            ("height", "Target height in px (transform; integer)", false),
            (
                "output_format",
                "Output format (transform): png, jpg, or jpeg (NOT webp)",
                false,
            ),
            (
                "size",
                "Size preset (transform): IconTiny, IconSmall, IconMedium, or Preview",
                false,
            ),
            (
                "crop_width",
                "Crop rectangle width (transform; integer; all four crop_* required together)",
                false,
            ),
            (
                "crop_height",
                "Crop rectangle height (transform; integer)",
                false,
            ),
            (
                "crop_x",
                "Crop rectangle x offset (transform; integer)",
                false,
            ),
            (
                "crop_y",
                "Crop rectangle y offset (transform; integer)",
                false,
            ),
            (
                "rotate",
                "Rotation in degrees (transform; integer): 0, 90, 180, or 270",
                false,
            ),
        ],
    },
    ToolDef {
        name: "asset",
        description: "Assets: list, list types, delete brand assets on orgs/workspaces.",
        actions: &[
            "list", "types", "remove",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "entity_type",
                "Entity type: org, workspace, share, or user",
                false,
            ),
            ("entity_id", "Entity ID", false),
            ("asset_type", "Asset type name (remove)", false),
        ],
    },
    ToolDef {
        name: "apps",
        description: "List the app INSTALLATION RECORDS registered to the authenticated user. These \
                      are not a catalog of available apps: each row is an app that registered itself \
                      against this account, with a CALLER-DEFINED app_id (e.g. 'com.example.desktop'), \
                      version, platform, install/uninstall times and last heartbeat. Requires auth. \
                      The 'details', 'launch' and 'get-tool-apps' actions were REMOVED in 2026.8: \
                      they called a '/apps/' path prefix that has never existed on the server, so \
                      they failed on every call. They described app WIDGETS, which are an MCP-server \
                      concept with no REST API in the published contract — there is nothing to point \
                      them at. Do not re-add them without a deployed route.",
        actions: &[
            "list",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[],
    },
    ToolDef {
        name: "import",
        description: "Cloud import: manage provider identities, sources, sync jobs, write-backs. \
                      Connect Google Drive, Dropbox, Box, OneDrive. \
                      SIDE EFFECTS — 'delete-source' does NOT require disconnecting first: it accepts a \
                      HEALTHY, actively-synced source and deletes it in one call. The server refuses only \
                      IN-FLIGHT states (syncing, discovering, disconnecting, pending), and that refusal is \
                      TRANSIENT — retry after the run, do not read it as a permission problem. Treat the \
                      rule as a denylist of in-flight states, not an allowlist of safe ones. A write-back \
                      still 'pending'/'uploading'/'conflict' NEVER REACHES THE PROVIDER once the source is \
                      deleted, so local edits are DISCARDED, not flushed — push or resolve them FIRST, and \
                      check the NODES' 'import_state.status' BEFORE calling. 'list-writebacks' also answers \
                      this at source level, with the hyphenated-id caveat noted on the upload tool — an \
                      empty queue is exactly the answer that would tell you it is safe to proceed, so be \
                      sure it is real. DO NOT EXPECT THE QUEUE TO BE TIDIED: the rows are reported to be \
                      left behind rather than cancelled, so a pending or conflicted write-back can still be \
                      VISIBLE after its source is gone while being permanently inert — do NOT report that \
                      as a failed delete or as work that might still run (reported from the platform \
                      source, not measured here). \
                      Prefer 'disconnect' (explicit keep/delete) when the intent is to stop \
                      syncing rather than to destroy. \
                      TWO ASYNC ACTIONS — do not treat \
                      either as complete when it returns: (1) 'provision-identity' returns status \
                      'provisioning', NOT 'active', with identity_email set to a SYNTHETIC placeholder that is \
                      not a real address and DIFFERS BY PROVIDER ('provisioning-pending', or \
                      'dropbox-oauth-pending' for dropbox) — treat any pending value as 'no address \
                      yet', do not match one literal — poll 'identity-details' until status is 'active' or \
                      'error' BEFORE creating a source. 'error' is TERMINAL — do not keep polling it; an \
                      un-consented tenant needs an ADMINISTRATOR to approve, a revoked/stale \
                      credential needs the USER to reconnect, and these are different actions. The \
                      ALL FOUR providers connect the caller's OWN account via a browser consent \
                      screen and return an authorize_url a human must open — google_drive and box \
                      included; POLLING ALONE NEVER ACTIVATES ANY OF THEM, and there is no address \
                      to share a folder with on any provider (the connected account IS the access). \
                      A 'provisioning' identity with properties.oauth_pending and NO authorize_url \
                      is TERMINAL, not slow: the URL is issued only on the provision response and \
                      never on a poll. Re-provisioning is the fix but NOT immediately — within ~30 \
                      minutes it returns the SAME row with already_exists and still no URL, so \
                      REVOKE first to recover now rather than retrying in a loop. 'already_exists': true \
                      means an EXISTING identity was returned instead of a new one, with NO fresh \
                      authorize_url — if it is 'active' you are done UNLESS you sent account_type, \
                      which is recorded only where a connect BEGINS: an existing row keeps the \
                      account type it was connected with, so your value is dropped and the 200 \
                      still looks like progress. One connection exists per user per workspace per \
                      provider and work/personal share the single onedrive_business provider, so \
                      switching requires revoking that connection first. If still 'provisioning' it \
                      is an abandoned consent and is re-provisionable only after ~30 minutes, or \
                      revoke it to recover sooner. Its 'instructions' field \
                      is returned ONLY by \
                      provision-identity — 'identity-details' does not carry it, so capture it from \
                      the provision response. (2) 'discover' returns a job_id, NOT a folder list — \
                      poll 'job-details' with source_id='discovery' (a literal marker; discovery \
                      jobs have no source). A 404 from that poll is NOT 'not ready yet' — a running \
                      job returns the job, so 404 never means 'still working'. It is almost always \
                      terminal (job absent, or not a discovery job), but the server returns the SAME \
                      404 for a transient datastore read failure, so retry ONCE before giving up; a \
                      second 404 is final. Do NOT branch on the numeric code to tell those apart — \
                      the codes there are randomly generated debug markers, not a contract. \
                      (3) A 500 here is NOT reliably transient, so do not treat the status class \
                      as a retry signal. Measured 2026-08-25: 'push-writeback' against a \
                      node that is not an imported file returns HTTP 500 with the message 'Node \
                      is not an imported file' — a permanent refusal no retry can clear. \
                      Read the server's MESSAGE before retrying any 5xx from this tool; a 5xx \
                      that names a precondition will never succeed on a repeat. JUDGE that \
                      message, do NOT string-match it — the wording is not a contract and is \
                      actively being revised; a match on it breaks silently on the next rewording. \
                      PERMISSIONS: 'provision-identity' requires full workspace MEMBER for ALL \
                      FOUR providers — a Viewer is refused for google_drive/box/dropbox exactly \
                      as for onedrive_business, since all four connect by delegated OAuth; \
                      'list-drives' and 'refresh-drives' are OWNER ONLY — a workspace admin cannot \
                      read or rebuild someone else's catalog, so do not suggest asking an admin to \
                      do it for them.",
        actions: &[
            "list-providers",
            "list-identities",
            "provision-identity",
            "identity-details",
            "revoke-identity",
            "list-sources",
            "list-drives",
            "refresh-drives",
            "discover",
            "create-source",
            "source-details",
            "update-source",
            "delete-source",
            "disconnect",
            "refresh",
            "list-jobs",
            "job-details",
            "cancel-job",
            "list-writebacks",
            "writeback-details",
            "push-writeback",
            "retry-writeback",
            "resolve-conflict",
            "cancel-writeback",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            (
                "provider",
                "Cloud provider: google_drive, box, onedrive_business, dropbox",
                false,
            ),
            (
                "account_type",
                "onedrive_business only (provision-identity): 'work' (server default) or                  'personal'. A PERSONAL Microsoft account cannot complete a work consent, so                  connecting one REQUIRES account_type='personal' — omitting it silently targets                  work and the user is refused at Microsoft. Ignored by other providers.",
                false,
            ),
            ("identity_id", "Provider identity ID", false),
            ("source_id", "Import source ID", false),
            ("job_id", "Import job ID", false),
            (
                "remote_path",
                "Remote folder path. On create-source: the folder to bind. On discover: \
                 enumerate INSIDE that folder instead of the provider root — the server \
                 returns ONE LEVEL per call, so drill down by discovering the root, picking a \
                 folder, then discovering again with its remote_path.",
                false,
            ),
            (
                "drive_id",
                "Drive (SharePoint library) id from list-drives. Required for onedrive_business on \
                 create-source, and pass the SAME drive to discover so the paths you discover \
                 belong to the library the source binds to. Create-time only — rejected on \
                 update-source. Raw Microsoft Graph id (b!-prefixed), not a Fast.io opaque id.",
                false,
            ),
            (
                "destination_node_id",
                "Storage FOLDER node id to graft the import under (create-source). Defaults to the \
                 workspace Imports folder. Must be a folder, must not be inside an existing import \
                 tree, and is create-time only — rejected on update-source.",
                false,
            ),
            (
                "site_path",
                "Site path. On refresh-drives it names the site to ENUMERATE (required when \
                 drives_state is requires_site_path). On list-drives it only FILTERS \
                 already-stored rows and cannot populate an empty catalog.",
                false,
            ),
            (
                "remote_name",
                "Display name (create-source, update-source)",
                false,
            ),
            (
                "sync_interval",
                "Sync interval in seconds (create-source, update-source)",
                false,
            ),
            ("access_mode", "Access mode: read_only or read_write", false),
            (
                "disconnect_action",
                "disconnect only. 'keep' leaves imported files in storage and STRIPS their import \
                 markers (they become ordinary files); 'delete' moves them to TRASH rather than \
                 erasing them — that is the MECHANISM only, NOT a promise the files can be \
                 restored; do not report recoverability you have not checked. CHECK \
                 'data_deleted' IN THE RESPONSE: a delete can FAIL while the \
                 disconnect SUCCEEDS, so a 200 does not mean the files are gone. false = files \
                 still present (the 'message' says so); null = 'keep', nothing to delete. Only an \
                 EXPLICIT false means failure — an ABSENT field means the server did not report, \
                 not that the delete failed, so do not raise a failure on a missing key. And read \
                 it from THIS response: the action is stored in source properties, which no source \
                 read returns, so nothing later can be asked what this call did.",
                false,
            ),
            (
                "status",
                "EFFECTIVE ON update-source ONLY. On list-sources it is accepted and IGNORED — \
                 measured: filtering a two-source workspace by either status returns BOTH rows, and \
                 an unknown value returns both rather than erroring, so results come back \
                 unfiltered while looking filtered. On list-writebacks it is ALSO accepted and \
                 IGNORED — measured 2026-08-22 on a source holding write-backs in THREE states \
                 (23 completed, 2 conflict, 1 canceled): every status value, including a nonsense \
                 one, returned all 26 rows with the full mixed distribution. Do not narrow a single page \
                 and treat it as filtered — limit/offset count UNFILTERED rows. On \
                 update-source it MUTATES a live sync: 'paused' stops it, 'synced' asks for it back \
                 — the stop meant to be undone, and the one to use instead of 'disconnect' or \
                 'delete-source' when syncing should resume later (both of those are permanent). \
                 AFTER A RESUME, RE-READ source-details AND CONFIRM THE SOURCE LEFT 'paused' — do \
                 not treat the success response as proof. Until 2026-08-21 the server returned 200 \
                 while failing the status change, leaving the source paused and off the sync \
                 schedule for good; confirmed and fixed upstream that day, but a fix only reaches \
                 the endpoint it is deployed to, so the re-read is correct on both sides of a \
                 rollout. On \
                 update-source those two are the ONLY accepted values, and each has a precondition \
                 on the source's CURRENT status: pause is accepted only from 'synced' or 'error', \
                 resume only from 'paused'. Any other value is now REJECTED — it previously sent a \
                 request carrying no action at all, which alongside another updatable field \
                 returned SUCCESS with the pause/resume silently skipped (on its own it hit 'no \
                 valid fields to update'). The combined case is the dangerous one: the other field \
                 applied and the status change did not.",
                false,
            ),
            ("writeback_id", "Write-back job ID", false),
            ("node_id", "Storage node ID (push-writeback)", false),
            (
                "conflict_resolution",
                "Resolution: keep_local or keep_remote",
                false,
            ),
            ("limit", "Pagination limit (integer)", false),
            ("offset", "Pagination offset (integer)", false),
        ],
    },
    ToolDef {
        name: "lock",
        description: "File locking: acquire, status, release, heartbeat (renew) exclusive locks on files in workspaces or shares.",
        actions: &[
            "acquire",
            "status",
            "release",
            "heartbeat",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("node_id", "File node ID", false),
            ("lock_token", "Lock token (release, heartbeat)", false),
            (
                "duration",
                "Lock duration in seconds, 60-3600 (acquire)",
                false,
            ),
            (
                "client_info",
                "Client metadata as a JSON object (acquire)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "metadata",
        description: "Metadata extraction: list eligible files, read a node's metadata, async single-file extraction (returns job_id; poll via workspace jobs-status), and lexical keyword search over metadata values. SIDE EFFECTS — these actions SPEND AI CREDITS: 'extract', 'extract-and-wait'. The 'extract-and-wait' action enqueues a single-file extraction and polls workspace jobs-status to a terminal state before returning (the offload-friendly compound).",
        actions: &[
            "eligible",
            "extract",
            "extract-and-wait",
            "search",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("node_id", "File node ID (extract, extract-and-wait)", false),
            (
                "limit",
                "Pagination limit (integer) — SEARCH ONLY. The 'eligible' action is \
                 cursor-paginated and IGNORES this; use page_size/cursor there",
                false,
            ),
            (
                "offset",
                "Pagination offset (integer) — SEARCH ONLY. The 'eligible' action is \
                 cursor-paginated and IGNORES this; use page_size/cursor there",
                false,
            ),
            (
                "page_size",
                "Records per page (eligible). The server clamps to 1-250 then SNAPS to the \
                 nearest of 25/100/250 in BOTH directions (<=62 -> 25, 63-175 -> 100, >=176 -> \
                 250), so asking for 50 returns 25 and asking for 200 returns 250. The \
                 response's page_size reports what was actually used",
                false,
            ),
            (
                "cursor",
                "Opaque cursor from a previous response's `cursor` field (eligible). Send it \
                 back unchanged; omit for the first page",
                false,
            ),
            (
                "mimetype",
                "Return only nodes with this MIME type (eligible)",
                false,
            ),
            (
                "extension",
                "Return only nodes with this file extension (eligible)",
                false,
            ),
            (
                "fields",
                "JSON-encoded array of field NAMES (extract, extract-and-wait). Omit for a full \
                 extraction. Naming fields makes the request EXCLUSIVE — exactly those fields are \
                 written and anything else the model returns is discarded — but the file is still \
                 read in full and a named field is written only if the document contains it. \
                 Every name must be a field the workspace has already produced (an unknown name \
                 is rejected, not ignored); max 50 distinct names per request. It is also a \
                 BILLING KEY: changing the list mints a new claim and re-runs the model.",
                false,
            ),
            (
                "poll_interval",
                "Seconds between job-status polls, 1-60, default 3 (extract-and-wait)",
                false,
            ),
            (
                "confirm_ai_spend",
                "REQUIRED to proceed (true/false) for the AI-credit-spending actions extract, extract-and-wait. These actions are rejected unless confirm_ai_spend=true. Ignored by read-only actions.",
                false,
            ),
            ("query", "Search keyword(s), max 1024 chars (search)", false),
        ],
    },
    ToolDef {
        name: "sign",
        description: "E-signature (SignEnvelope): draft and drive electronic-signature envelopes (PDFs sent to recipients). Every envelope is parented to a workspace (workspace_id; the former org surface was removed). This tool exposes READ, reversible-DRAFT-drive, and idempotent-RECOVERY actions: envelope-create (creates a DRAFT — reversible), envelope-update (draft-only; recipients are a full replacement; expires_at/policy_json are DECLARATIVE — omitting them CLEARS those fields, re-send to retain), envelope-list (filter via envelope_status / created_after / created_before), envelope-get, envelope-retry (re-drives a STUCK envelope through self-healing recovery — admin; idempotent + no-op-success; notifies no one; a permanent failure cascades to Failed), envelope-my-sign-link (mints YOUR OWN signing link for an envelope — the dashboard signature-card primary action; reversible/idempotent and notifies no one; requires a WRITE-scope token, so a read-only token is rejected with 10754; the structured result tells you the state — sign_url non-null = sign now, is_terminal = completed/void/declined, reauth_required = re-authenticate first, else you are blocked by routing order per blocked_signers), document-download (covers preview needs — the download bytes ARE the source/preview PDF, so there is no separate MCP preview action), signed-download, audit-download, describe. SIGN TEMPLATES (reusable envelope blueprints, template id sa…): template-list, template-details, and template-instantiate (resolves recipient_bindings/documents against the blueprint and creates a reversible DRAFT envelope) are exposed over MCP (reads + reversible draft creation); template-create, template-update, and template-delete are intentionally CLI-binary-only (`fastio sign template create|update|delete …`) and are NOT routable over MCP (mirrors the send/void boundary). The OUTWARD-FACING / TERMINAL actions — send (EMAILS REAL RECIPIENTS) and void (terminal) — are intentionally CLI-binary-only (`fastio sign envelope send|void …`) and are NOT routable over MCP. Envelopes are voided, not deleted — there is no delete action. Binary downloads write to the agent's local filesystem and return a path + byte count (NOT base64). Signing is a paid-plan feature (a non-entitled org returns 1670; access also requires workspace membership). Call action='describe' for the authoritative per-action reference.",
        actions: &[
            "describe",
            "envelope-create",
            "envelope-update",
            "envelope-list",
            "envelope-get",
            "envelope-retry",
            "envelope-my-sign-link",
            "template-list",
            "template-details",
            "template-instantiate",
            "document-download",
            "signed-download",
            "audit-download",
        ],
        params: &[
            // Required for every action EXCEPT describe / send / void / delete,
            // which short-circuit before workspace extraction. Marked schema-
            // optional (false) — matching the registry convention for
            // multi-action tools (e.g. `apps`) — with the real
            // per-action requirements communicated via action='describe'
            // (common_required + each action's `required` list). A schema-strict
            // MCP client would otherwise reject action='describe' for lacking
            // workspace_id.
            ("workspace_id", "Workspace ID (19-digit)", false),
            (
                "envelope_id",
                "Envelope ID (envelope-get / -update / downloads)",
                false,
            ),
            (
                "document_id",
                "Document OpaqueId (document-download / signed-download)",
                false,
            ),
            ("name", "Display name (envelope-create / -update)", false),
            (
                "expires_at",
                "UTC auto-expiry timestamp. envelope-create: optional. envelope-update: DECLARATIVE — omitting it CLEARS the expiry (resets to null); re-send the current value to keep it.",
                false,
            ),
            (
                "body_json",
                "Whole create request as a JSON object STRING (envelope-create; overrides the other create params)",
                false,
            ),
            (
                "policy_json",
                "Policy bag as a JSON object STRING. envelope-create: optional. envelope-update: DECLARATIVE — omitting it CLEARS the policy (resets to null); re-send the current value to keep it.",
                false,
            ),
            (
                "documents_json",
                "Documents as a JSON array STRING (envelope-create; declarative replace on envelope-update)",
                false,
            ),
            (
                "recipients_json",
                "Recipients as a JSON array STRING (full replace on envelope-update). Required on envelope-update; must contain at least one recipient.",
                false,
            ),
            (
                "fields_json",
                "Field placements as a JSON array STRING (full replace on envelope-update). \
                 Each item: recipient_email, document_index, page, x_norm/y_norm/w_norm/h_norm, \
                 type, required, optional value_json (a JSON STRING), and an optional \
                 `validation` OBJECT constraining what the signer may submit — \
                 min_length / max_length / pattern for text-style fields, date_min / date_max \
                 for date fields. A `validation` on a signature/initial/checkbox field, or an \
                 unsupported key, is rejected server-side with 1605.",
                false,
            ),
            (
                "source_node_id",
                "Simple single-document create: source storage node id",
                false,
            ),
            (
                "source_version_id",
                "Simple single-document create: pinned source version id",
                false,
            ),
            (
                "recipient_email",
                "Simple single-signer create: signer email",
                false,
            ),
            (
                "recipient_name",
                "Simple single-signer create: signer display name",
                false,
            ),
            (
                "auth_method",
                "Simple single-signer create: none / email_otp / sms_otp",
                false,
            ),
            (
                "output_path",
                "Local destination FILE path for a download (defaults under .fastio/downloads/)",
                false,
            ),
            (
                "envelope_status",
                "envelope-list filter: a single lifecycle status or a CSV of draft,sent,in_progress,completed,declined,expired,voided,failed",
                false,
            ),
            (
                "created_after",
                "envelope-list filter: only envelopes created after this time (Y-m-d H:i:s UTC)",
                false,
            ),
            (
                "created_before",
                "envelope-list filter: only envelopes created before this time (Y-m-d H:i:s UTC)",
                false,
            ),
            (
                "limit",
                "Pagination limit (envelope-list / template-list)",
                false,
            ),
            (
                "offset",
                "Pagination offset (envelope-list / template-list)",
                false,
            ),
            (
                "template_id",
                "Sign-template OpaqueId (sa…) — template-details / template-instantiate",
                false,
            ),
            (
                "recipient_bindings",
                "template-instantiate: REQUIRED JSON OBJECT/map STRING keyed by slot_key → {email, display_name?, auth_method?}. An array is rejected.",
                false,
            ),
            (
                "documents",
                "template-instantiate: optional JSON ARRAY STRING of {document_slot_index, source_node_id, source_version_id?}",
                false,
            ),
            (
                "envelope_name",
                "template-instantiate: optional name override for the created DRAFT envelope",
                false,
            ),
        ],
    },
    ToolDef {
        name: "fileshare",
        description: "File Shares: durable, link-shareable views of a SINGLE workspace file — the replacement for the retired QuickShare (use this tool's 'create' action). A File Share binds one file node and serves it via a stable link with optional password protection, an access option (named_people / anyone_with_link / …), an expiry, and per-user grants (view < download < edit). This tool exposes READ + DRIVE actions: create, list, info (details + effective_capability), update, grants-list, grants-add, versions, download (streams the bound file to the agent's local fs), preview (streams a derived preview asset), activity (single poll), describe. CONFIRM-GATED destructive actions: 'delete' REQUIRES confirm_delete=true (revokes the link + cascades grants; the bound file is untouched); 'grants-remove' REQUIRES confirm_revoke=true. CLI-BINARY-ONLY actions (NOT routable over MCP): 'upload' — the write-back that pushes a NEW VERSION of the bound file — needs the local file bytes and is destructive, so run `fastio fileshare upload …`; and 'ws-token' — the realtime WebSocket-token mint — is CLI-only because the token is a long-lived secret that must NOT be parked in an MCP transcript (run `fastio fileshare ws-token --token-file <path>`). The 'password' arg protects/authorizes a link (travels only in the x-ve-password header; NEVER logged or echoed in results/errors). info/download/versions/preview may be ANONYMOUS on a public (anyone_with_link) share. Binary downloads write to the agent's local filesystem and return a path + byte count (NOT base64). Call action='describe' for the authoritative per-action reference.",
        actions: &[
            "describe",
            "create",
            "list",
            "info",
            "update",
            "grants-list",
            "grants-add",
            "grants-remove",
            "delete",
            "versions",
            "download",
            "preview",
            "activity",
        ],
        params: &[
            (
                "workspace_id",
                "Workspace ID (19-digit) — create / list",
                false,
            ),
            (
                "fileshare_id",
                "File Share ID — info / update / delete / grants-* / versions / download / preview / activity",
                false,
            ),
            (
                "node_id",
                "Bound file node id (create) — must be a FILE node (not a folder or note)",
                false,
            ),
            ("title", "Display title (create / update)", false),
            (
                "access_option",
                "Access option (create / update): e.g. named_people, anyone_with_link",
                false,
            ),
            (
                "password",
                "Link password (create / update set; info / download / versions / preview supply it to authorize a protected link). Travels only in the x-ve-password header; NEVER logged. On update, pass clear_password=true to REMOVE it (do not also pass password).",
                false,
            ),
            (
                "expires",
                "Relative expiry in seconds (create / update). Mutually exclusive with expires_at.",
                false,
            ),
            (
                "expires_at",
                "Absolute expiry timestamp (create / update; naive = UTC). Mutually exclusive with expires.",
                false,
            ),
            (
                "clear_password",
                "update only: true REMOVES the link password (do not also pass password).",
                false,
            ),
            (
                "clear_expires",
                "update only: true CLEARS the expiry.",
                false,
            ),
            (
                "user",
                "Grant target user id (grants-add / grants-remove). Supply exactly one of user / email.",
                false,
            ),
            (
                "email",
                "Grant target email (grants-add / grants-remove). Supply exactly one of user / email. (Unregistered emails send a real invite.)",
                false,
            ),
            (
                "capability",
                "Grant capability (grants-add): view / download / edit (ordered view < download < edit).",
                false,
            ),
            (
                "version",
                "download only: a historical version id to fetch instead of the current bytes.",
                false,
            ),
            (
                "preview_type",
                "preview only: the preview/derived-asset type to fetch (e.g. a thumbnail/format key).",
                false,
            ),
            (
                "output_path",
                "Local destination FILE path for download / preview (defaults under .fastio/downloads/).",
                false,
            ),
            (
                "confirm_delete",
                "REQUIRED to proceed (true) for delete; the action is rejected unless confirm_delete=true (mirrors the CLI --yes gate).",
                false,
            ),
            (
                "confirm_revoke",
                "REQUIRED to proceed (true) for grants-remove; the action is rejected unless confirm_revoke=true (mirrors the CLI --yes gate).",
                false,
            ),
            ("offset", "Pagination offset (list)", false),
            ("limit", "Pagination limit (list)", false),
            (
                "lastactivity",
                "activity only: cursor — return only events newer than this marker.",
                false,
            ),
            (
                "wait",
                "activity only: long-poll wait seconds (single poll).",
                false,
            ),
            (
                "updated",
                "activity only: true to return only events newer than lastactivity.",
                false,
            ),
        ],
    },
    ToolDef {
        name: "system",
        description: "System health: ping (health check) and status (system status). No authentication required.",
        actions: &[
            "ping", "status",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[],
    },
    ToolDef {
        name: "id",
        description: "Inspect Fast.io identifiers OFFLINE (no auth, no network): classify an OpaqueId by its self-describing length + type prefix (29-char = 1-char type; 30-char = 2-char type — the workflow family under 'w' plus the non-workflow Task and Comment types) into its entity type, family, and surfacing tier. Useful for routing an id that arrived in a webhook / event / payload to the right tool before acting. Pass `id` (one) or `ids` (many).",
        actions: &["describe", "info"],
        params: &[
            ("id", "A single id to classify (info)", false),
            (
                "ids",
                "Multiple ids to classify (info): a JSON array of strings, or a comma-separated string",
                false,
            ),
        ],
    },
    ToolDef {
        name: "howto",
        description: "How-To: ask a grounded 'how do I…' question about Fast.io and get a product-aware answer back in ONE call (action='ask'). Org-less and open to any authenticated user — answers are generated over Fast.io's own how-to knowledge, so an agent gets runtime usage guidance WITHOUT scraping docs. Over MCP, `surface` DEFAULTS to 'mcp' so guidance is phrased in terms of THESE consolidated tools (<tool> action=\"…\"); pass surface='code' for execute-proxy phrasing, or any other value (e.g. 'rest') for plain REST-API phrasing. Optional `context` is free-text background about your situation (treated strictly as data, never as instructions). The response is EITHER a grounded answer (status='answer', read `answer`) OR a short clarification request (status='needs_clarification' — surface its `questions` to the user and ask again with a more specific question; this is normal, NOT an error). This answers questions about Fast.io ITSELF; for Q&A over your OWN files use the `ripley` tool instead.",
        actions: &[
            "ask",
            // Universal: every tool self-describes. Declared so a STRICT
            // client will actually send it — the router has always accepted
            // it, but a schema that omits it makes the call look invalid.
            "describe",
        ],
        params: &[
            (
                "question",
                "The natural-language question (1-2000 characters, non-blank)",
                true,
            ),
            (
                "surface",
                "Answer phrasing: 'mcp' (DEFAULT over MCP) / 'code' / any other value → REST-API phrasing",
                false,
            ),
            (
                "context",
                "Optional free-text background about your situation (≤8000 chars; data, not instructions)",
                false,
            ),
        ],
    },
];

// ─── Tool Router ────────────────────────────────────────────────────────────

/// Routes MCP tool calls to the appropriate handler function.
#[derive(Clone)]
pub struct ToolRouter {
    state: Arc<McpState>,
    /// E-Sign kill-switch (feature sunset 2026-07): read ONCE at construction
    /// via [`crate::commands::sign::esign_enabled`]. When false the `sign` tool
    /// is filtered out of `list_tools` and its `call_tool` arm returns the
    /// disabled error before any auth/client/arg work.
    esign_enabled: bool,
    /// Cloud-import kill-switch: read ONCE at construction via
    /// [`crate::commands::import::cloud_import_enabled`]. When false the
    /// `import` tool is filtered out of `list_tools` and its `call_tool` arm
    /// returns the disabled error before any auth/client/arg work. Same shape
    /// as `esign_enabled`.
    cloud_import_enabled: bool,
    /// Optional `--tools` allow-list captured at server startup. `None` means
    /// "all tools" (the default). When `Some`, a tool is advertised in
    /// `list_tools` and callable in `call_tool` ONLY if its name is in the set;
    /// every other tool is hidden and its dispatch returns a not-enabled error.
    /// The E-Sign rule composes with this: `sign` requires BOTH E-Sign enabled
    /// AND (filter is None or contains "sign"). The set contains only names that
    /// match a [`TOOL_DEFS`] entry (unknown names are dropped, with a startup
    /// warning, before the router is built).
    tools_filter: Option<std::collections::HashSet<String>>,
}

impl ToolRouter {
    /// Create a new tool router with shared state and an optional `--tools`
    /// allow-list.
    ///
    /// Reads the E-Sign kill-switch (`FASTIO_ENABLE_ESIGN=1`) once here so the
    /// flag is fixed for the lifetime of the server rather than re-read per call.
    /// `tools_filter` is `None` for the default (all tools) or the validated
    /// allow-list set built by the `serve()` entry point.
    pub fn new(
        state: Arc<McpState>,
        tools_filter: Option<std::collections::HashSet<String>>,
    ) -> Self {
        Self {
            state,
            esign_enabled: crate::commands::sign::esign_enabled(),
            cloud_import_enabled: crate::commands::import::cloud_import_enabled(),
            tools_filter,
        }
    }

    /// Construct a router with an explicit E-Sign flag (no `--tools` filter),
    /// for unit tests that must assert both the enabled and disabled surfaces
    /// without mutating the process environment (unsafe under Rust 2024 and
    /// process-global).
    #[cfg(test)]
    pub fn new_with_esign(state: Arc<McpState>, esign_enabled: bool) -> Self {
        Self {
            state,
            esign_enabled,
            // Tests that pin the E-Sign surface must not also trip the
            // cloud-import gate; enable it so those assertions stay about sign.
            cloud_import_enabled: true,
            tools_filter: None,
        }
    }

    /// Construct a router with an explicit cloud-import flag (no `--tools`
    /// filter), for unit tests that must assert both the enabled and disabled
    /// import surfaces without mutating the process environment.
    #[cfg(test)]
    pub fn new_with_cloud_import(state: Arc<McpState>, cloud_import_enabled: bool) -> Self {
        Self {
            state,
            esign_enabled: false,
            cloud_import_enabled,
            tools_filter: None,
        }
    }

    /// Construct a router with an explicit E-Sign flag AND a `--tools`
    /// allow-list, for the filter unit tests.
    #[cfg(test)]
    pub fn new_with_filter(
        state: Arc<McpState>,
        esign_enabled: bool,
        tools_filter: Option<std::collections::HashSet<String>>,
    ) -> Self {
        Self {
            state,
            esign_enabled,
            // Filter tests assert on the allow-list, not the import gate.
            cloud_import_enabled: true,
            tools_filter,
        }
    }

    /// List all registered tools as MCP `Tool` descriptors, honoring the E-Sign
    /// kill-switch AND the `--tools` allow-list captured at router construction.
    /// This instance method is the sole production listing path: it reads
    /// `self.esign_enabled` and `self.tools_filter` (both fixed at construction),
    /// so the advertised tool surface can never diverge from the callable surface
    /// `call_tool` gates on the same fields.
    pub fn list_tools(&self) -> ListToolsResult {
        let tools = TOOL_DEFS
            .iter()
            .filter(|def| self.tool_visible(def.name))
            .map(|def| {
                Tool::new(
                    def.name,
                    def.description,
                    action_schema(def.actions, def.params),
                )
            })
            .collect();
        ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        }
    }

    /// Whether the `--tools` allow-list admits `name` (a CANONICAL tool name).
    /// `None` filter = admits everything. Shared by [`Self::tool_visible`] (the
    /// listing gate) and [`Self::call_tool`] (the dispatch gate) so the allow-list
    /// half is defined exactly once and the advertised set can never diverge from
    /// the callable set. The E-Sign rule is applied ALONGSIDE this (see
    /// `tool_visible` / the `sign` match arm), not inside it, because the two
    /// gates surface DIFFERENT refuse messages (not-enabled vs E-Sign-disabled).
    fn filter_admits(&self, name: &str) -> bool {
        match &self.tools_filter {
            None => true,
            Some(set) => set.contains(name),
        }
    }

    /// Whether a tool `name` is exposed in `list_tools` (the advertised set). A
    /// tool is visible iff the E-Sign rule allows it (`sign` requires
    /// `esign_enabled`) AND the `--tools` allow-list admits it
    /// ([`Self::filter_admits`]). `call_tool` applies the SAME two rules at
    /// dispatch time (allow-list via `filter_admits`, E-Sign via its `sign` match
    /// arm), so advertised and callable stay in lockstep.
    fn tool_visible(&self, name: &str) -> bool {
        if name == "sign" && !self.esign_enabled {
            return false;
        }
        if name == "import" && !self.cloud_import_enabled {
            return false;
        }
        self.filter_admits(name)
    }

    /// Whether, after applying BOTH the E-Sign rule and the `--tools` allow-list,
    /// any tool at all remains visible. Used at startup to refuse a server that
    /// would advertise an empty tool surface (e.g. `--tools sign` with E-Sign
    /// disabled — `sign` is a known name so name-validation passes, but the
    /// effective surface is empty).
    pub fn has_visible_tools(&self) -> bool {
        TOOL_DEFS.iter().any(|def| self.tool_visible(def.name))
    }

    /// Whether a `--tools` allow-list is active on this router (i.e. the surface
    /// is a strict subset of all tools). Used by the server intro so the
    /// `instructions` text only enumerates the allowed tools when a filter is in
    /// effect, instead of pitching the full default surface.
    pub fn has_tools_filter(&self) -> bool {
        self.tools_filter.is_some()
    }

    /// The canonical names of the tools currently VISIBLE on this router (both
    /// the E-Sign rule and the `--tools` allow-list applied), in `TOOL_DEFS`
    /// order. The advertised set `list_tools` returns, as names — used by the
    /// server intro to enumerate a filtered surface honestly.
    pub fn visible_tool_names(&self) -> Vec<&'static str> {
        TOOL_DEFS
            .iter()
            .map(|def| def.name)
            .filter(|name| self.tool_visible(name))
            .collect()
    }

    /// Core of the legacy `list_tools`, parameterized on the E-Sign flag so
    /// tests can exercise both surfaces without touching the process environment
    /// or a `--tools` filter. When `esign_enabled` is false the `sign` tool is
    /// filtered out; the `TOOL_DEFS` static array is left intact. (The
    /// production listing path is the instance [`Self::list_tools`], which also
    /// applies the `--tools` allow-list.)
    #[cfg(test)]
    pub fn list_tools_with(esign_enabled: bool) -> ListToolsResult {
        let tools = TOOL_DEFS
            .iter()
            .filter(|def| esign_enabled || def.name != "sign")
            .map(|def| {
                Tool::new(
                    def.name,
                    def.description,
                    action_schema(def.actions, def.params),
                )
            })
            .collect();
        ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        }
    }

    /// Dispatch a tool call to the correct handler.
    pub async fn call_tool(
        &self,
        name: &str,
        args: Map<String, Value>,
    ) -> Result<CallToolResult, McpError> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("");

        // `--tools` allow-list gate: refuse a tool the server was not started
        // with, BEFORE any dispatch. The check uses the CANONICAL tool name (via
        // the SAME `filter_admits` predicate `list_tools` uses) so the hidden
        // aliases (`ai` → `ripley`, `how-to` → `howto`) are gated by the name a
        // caller would pass to `--tools`. The E-Sign `sign` gate stays below; a
        // `sign` call must satisfy BOTH the filter and the flag.
        let canonical = canonical_tool_name(name);
        if !self.filter_admits(canonical) {
            return Ok(error_text(&format!(
                "Tool '{canonical}' is not enabled on this server (started with --tools)."
            )));
        }

        // A kill-switched tool must not self-describe either. This check has
        // to sit ABOVE the universal describe below, not in the `match` arms:
        // describe short-circuits before dispatch ever runs, so without it
        // `import describe` would answer in full on a server where every other
        // `import` action is refused. `sign` would only escape that because it
        // is named in the fall-through list for an unrelated reason (a richer
        // hand-written payload) — an accident, not a gate. Both are gated
        // by the same explicit check, using the same strings the arms return.
        if !self.esign_enabled && canonical == "sign" {
            return Ok(error_text(ESIGN_DISABLED_MSG));
        }
        if !self.cloud_import_enabled && canonical == "import" {
            return Ok(error_text(CLOUD_IMPORT_DISABLED_MSG));
        }

        // Universal describe (cloud-convention parity): EVERY tool
        // self-describes, no auth needed. Tools with richer hand-written
        // payloads (fileshare, id, sign) fall through to their handlers'
        // own describe paths.
        if action == "describe"
            && !matches!(canonical, "fileshare" | "id" | "sign")
            && let Some(def) = TOOL_DEFS.iter().find(|d| d.name == canonical)
        {
            return Ok(generic_describe(def));
        }

        match name {
            "auth" => handle_auth(&self.state, action, &args).await,
            "user" => handle_user(&self.state, action, &args).await,
            "org" => handle_org(&self.state, action, &args).await,
            "workspace" => handle_workspace(&self.state, action, &args).await,
            "files" => handle_files(&self.state, action, &args).await,
            "search" => handle_search(&self.state, action, &args).await,
            "upload" => handle_upload(&self.state, action, &args).await,
            "download" => handle_download(&self.state, action, &args).await,
            "share" => handle_share(&self.state, action, &args).await,
            // `ai` is the hidden back-compat alias for the renamed `ripley`
            // tool. Both route to the same handler, which already issues the
            // migrated `/ai/agent/` paths and the corrected form bodies, so
            // legacy `ai` callers transparently get the fixed behavior.
            "ripley" | "ai" => handle_ai(&self.state, action, &args).await,
            "member" => handle_member(&self.state, action, &args).await,
            "comment" => handle_comment(&self.state, action, &args).await,
            "event" => handle_event(&self.state, action, &args).await,
            "dashboard" => handle_dashboard(&self.state, action, &args).await,
            // `how-to` is accepted as an alias for the `howto` tool name so a
            // caller that mirrors the CLI's `how-to` spelling still routes.
            "howto" | "how-to" => handle_howto(&self.state, action, &args).await,
            "invitation" => handle_invitation(&self.state, action, &args).await,
            "preview" => handle_preview(&self.state, action, &args).await,
            "asset" => handle_asset(&self.state, action, &args).await,
            "apps" => handle_apps(&self.state, action, &args).await,
            "import" if !self.cloud_import_enabled => Ok(error_text(CLOUD_IMPORT_DISABLED_MSG)),
            "import" => handle_import(&self.state, action, &args).await,
            "lock" => handle_lock(&self.state, action, &args).await,
            "metadata" => handle_metadata(&self.state, action, &args).await,
            "sign" if !self.esign_enabled => Ok(error_text(ESIGN_DISABLED_MSG)),
            "sign" => handle_sign(&self.state, action, &args).await,
            "fileshare" => handle_fileshare(&self.state, action, &args).await,
            "system" => handle_system(&self.state, action, &args).await,
            "id" => Ok(handle_id(action, &args)),
            _ => Ok(error_text(&format!("Unknown tool: {name}"))),
        }
    }
}

// ─── Tool Handlers ──────────────────────────────────────────────────────────

/// Registry-generated `action=describe` payload for tools without a
/// hand-written reference — cloud-convention parity (every consolidated tool
/// self-describes, without auth).
///
/// Mirrors the cloud payload's structural headings (tool / summary / actions /
/// `param_details`). The registry does not model per-action required/optional
/// splits structurally — each param's action-scoping lives in its description
/// prose — so those keys are OMITTED rather than emitted empty-and-wrong.
fn generic_describe(def: &ToolDef) -> CallToolResult {
    let mut text = String::new();
    text.push_str("# tool\n");
    text.push_str(def.name);
    text.push_str("\n\n# summary\n");
    text.push_str(def.description);
    text.push_str("\n\n# actions\n");
    for a in def.actions {
        text.push_str("- ");
        text.push_str(a);
        text.push('\n');
    }
    if !def.params.is_empty() {
        text.push_str(
            "\n# param_details\n| param | required | description |\n| --- | --- | --- |\n",
        );
        for (name, desc, required) in def.params {
            let cell = desc.replace('|', "\\|");
            text.push_str("| ");
            text.push_str(name);
            text.push_str(" | ");
            text.push_str(if *required {
                "always"
            } else {
                "action-dependent"
            });
            text.push_str(" | ");
            text.push_str(&cell);
            text.push_str(" |\n");
        }
    }
    text.push_str(
        "\n# note\nRegistry-generated reference. `action` is always required; each param's \
         action-scoping is stated in its description.\n",
    );
    CallToolResult::success(vec![Content::text(text)])
}
/// Auth tool handler.
async fn handle_auth(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    match action {
        "signin" => handle_auth_signin(state, args).await,
        "signout" => Ok(handle_auth_signout(state, args).await),
        "status" => handle_auth_status(state, args).await,
        "set-api-key" => handle_auth_set_api_key(state, args).await,
        "api-key-create" => handle_auth_api_key_create(state, args).await,
        "api-key-list" => handle_auth_api_key_list(state, args).await,
        "api-key-delete" => handle_auth_api_key_delete(state, args).await,
        "api-key-get" => handle_auth_api_key_get(state, args).await,
        "api-key-update" => handle_auth_api_key_update(state, args).await,
        "check" => handle_auth_check(state, args).await,
        "session" => handle_auth_session(state, args).await,
        "email-check" => handle_auth_email_check(state, args).await,
        "password-reset-request" => handle_auth_password_reset_request(state, args).await,
        "password-reset" => handle_auth_password_reset(state, args).await,
        "2fa-status" => handle_auth_2fa_status(state, args).await,
        "2fa-send" => handle_auth_2fa_send(state, args).await,
        "2fa-verify-setup" => handle_auth_2fa_verify_setup(state, args).await,
        "oauth-list" => handle_auth_oauth_list(state, args).await,
        "oauth-details" => handle_auth_oauth_details(state, args).await,
        "oauth-rename" => handle_auth_oauth_rename(state, args).await,
        "oauth-revoke" => handle_auth_oauth_revoke(state, args).await,
        "oauth-revoke-all" => handle_auth_oauth_revoke_all(state, args).await,
        "scopes" => handle_auth_scopes(state, args).await,
        "password-reset-check" => handle_auth_password_reset_check(state, args).await,
        _ => Ok(error_text(&format!("Unknown auth action: {action}"))),
    }
}

async fn handle_auth_signin(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let email = match required_str(args, "email") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let password = match required_str(args, "password") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::sign_in(&client, email, password).await {
        Ok(resp) => {
            // Store the token in memory for this session
            let token = resp.auth_token.clone();
            drop(client);
            state.set_token(token.clone()).await;
            // Also persist to credentials file
            if let Ok(dir) = fastio_cli::config::Config::default_dir()
                && let Ok(mut creds_file) = CredentialsFile::load(&dir)
                && let Err(e) = creds_file.set(
                    "default",
                    StoredCredentials {
                        token: Some(SecretString::from(token)),
                        expires_at: Some(chrono::Utc::now().timestamp() + resp.expires_in),
                        email: Some(email.to_owned()),
                        auth_method: Some("basic".to_owned()),
                        ..StoredCredentials::default()
                    },
                    &dir,
                )
            {
                tracing::warn!("failed to persist credentials: {e}");
            }
            Ok(success_json(&json!({
                "status": "authenticated",
                "two_factor_required": resp.two_factor,
                "expires_in": resp.expires_in,
            })))
        }
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Sign out of the LOCAL MCP session only.
///
/// The MCP server holds a single ambiguous bearer token (resolved at startup
/// from `--token` / env / stored creds, or set live via `signin` / `set-api-key`)
/// that is most commonly an **API key** — the documented MCP auth path
/// (`FASTIO_MCP_API_KEY` → `set-api-key`). The server-side sign-out
/// (`POST /user/auth/sign-out/`) is the WRONG operation for that model: per
/// the published API docs it does NOT revoke API keys (so it would not touch
/// the dominant MCP credential) yet it invalidates EVERY revocable browser JWT
/// for the user (a broad, surprising side effect an agent calling "signout"
/// would not expect).
/// So this action is deliberately local-only: it clears this session's in-memory
/// token (genuinely de-authenticating the live MCP session) and removes the
/// locally stored `default` credential. Server-side revocation has dedicated
/// paths — `fastio auth signout` (revocable session) and `api-key-delete`.
async fn handle_auth_signout(state: &McpState, _args: &Map<String, Value>) -> CallToolResult {
    // Genuinely de-authenticate the live MCP session (the previous behavior left
    // the in-memory token in place, so the session stayed authenticated).
    state.clear_token().await;
    if let Ok(dir) = fastio_cli::config::Config::default_dir()
        && let Ok(mut creds_file) = CredentialsFile::load(&dir)
        && let Err(e) = creds_file.remove("default", &dir)
    {
        tracing::warn!("failed to clear stored credentials: {e}");
    }
    success_json(&json!({
        "status": "local_session_cleared",
        "note": "Cleared this MCP session's in-memory token and removed the locally \
                 stored 'default' credential. This does NOT revoke the server-side \
                 session or API key. To revoke a revocable server session, run \
                 `fastio auth signout` in a terminal; to revoke an API key, use \
                 action=api-key-delete.",
    }))
}

async fn handle_auth_status(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let authenticated = state.is_authenticated().await;
    if !authenticated {
        return Ok(success_json(&json!({
            "authenticated": false,
            "hint": "Run `fastio auth login` or use action=signin"
        })));
    }
    let client = state.client().read().await;
    match api::auth::check_token(&client).await {
        Ok(resp) => Ok(success_json(&json!({
            "authenticated": true,
            "user_id": resp.id,
        }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_set_api_key(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let key = match required_str(args, "api_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    state.set_token(key.to_owned()).await;
    if let Ok(dir) = fastio_cli::config::Config::default_dir()
        && let Ok(mut creds_file) = CredentialsFile::load(&dir)
        && let Err(e) = creds_file.set(
            "default",
            StoredCredentials {
                api_key: Some(SecretString::from(key.to_owned())),
                auth_method: Some("api_key".to_owned()),
                ..StoredCredentials::default()
            },
            &dir,
        )
    {
        tracing::warn!("failed to persist credentials: {e}");
    }
    Ok(success_json(&json!({ "status": "api_key_set" })))
}

async fn handle_auth_api_key_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::api_key_create(
        &client,
        optional_str(args, "name"),
        optional_str(args, "scopes"),
        optional_str(args, "agent_name"),
        optional_str(args, "expires"),
    )
    .await
    {
        Ok(resp) => Ok(success_json(&json!({ "api_key": resp.api_key }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_api_key_list(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::api_key_list(&client).await {
        Ok(resp) => Ok(success_json(&json!({
            "count": resp.results,
            "api_keys": resp.api_keys,
        }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_api_key_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let key_id = match required_str(args, "key_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::api_key_delete(&client, key_id).await {
        Ok(_) => Ok(success_json(&json!({ "status": "deleted" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_api_key_get(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let key_id = match required_str(args, "key_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::api_key_get(&client, key_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_api_key_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let key_id = match required_str(args, "key_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = optional_str(args, "name");
    let scopes = optional_str(args, "scopes");
    let agent_name = optional_str(args, "agent_name");
    let expires = optional_str(args, "expires");
    if name.is_none() && scopes.is_none() && agent_name.is_none() && expires.is_none() {
        return Ok(error_text(
            "at least one update field is required (name, scopes, agent_name, expires)",
        ));
    }
    let client = state.client().read().await;
    match api::auth::api_key_update(&client, key_id, name, scopes, agent_name, expires).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_check(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::check_token(&client).await {
        Ok(resp) => Ok(success_json(&json!({ "valid": true, "user_id": resp.id }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_session(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::session_info(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_email_check(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let email = match required_str(args, "email") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::auth::email_check(&client, email).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_password_reset_request(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let email = match required_str(args, "email") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::password_reset_request(&client, email).await {
        Ok(_) => Ok(success_json(&json!({ "status": "sent", "email": email }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_password_reset(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let code = match required_str(args, "code") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let password1 = match required_str(args, "password1") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let password2 = match required_str(args, "password2") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::password_reset_complete(&client, code, password1, password2).await {
        Ok(_) => Ok(success_json(&json!({ "status": "reset_complete" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_2fa_status(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::two_factor_status(&client).await {
        Ok(resp) => Ok(success_json(
            &json!({ "state": resp.state, "totp": resp.totp }),
        )),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_2fa_send(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let channel = match required_str(args, "channel") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::two_factor_send(&client, channel).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_2fa_verify_setup(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let token = match required_str(args, "token") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::two_factor_verify_setup(&client, token).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_oauth_list(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::oauth_list(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_oauth_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let session_id = match required_str(args, "session_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::oauth_details(&client, session_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_oauth_revoke(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let session_id = match required_str(args, "session_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    match api::auth::oauth_revoke(&client, session_id).await {
        Ok(_) => Ok(success_json(
            &json!({ "status": "revoked", "session_id": session_id }),
        )),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_oauth_rename(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let session_id = match required_str(args, "session_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let device_name = optional_str(args, "device_name");
    let agent_name = optional_str(args, "agent_name");
    if device_name.is_none() && agent_name.is_none() {
        return Ok(error_text(
            "at least one of device_name or agent_name is required",
        ));
    }
    let client = state.client().read().await;
    match api::auth::oauth_rename(&client, session_id, device_name, agent_name).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_oauth_revoke_all(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::oauth_revoke_all(&client, optional_str(args, "current_session_id")).await {
        Ok(_) => Ok(success_json(&json!({ "status": "all_revoked" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_scopes(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::scopes(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_auth_password_reset_check(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let code = match required_str(args, "code") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // No auth required for this endpoint
    let client = state.client().read().await;
    match api::auth::password_reset_check(&client, code).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// User tool handler.
async fn handle_user(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    // Account `close` PERMANENTLY closes the user's account (irreversible). Like
    // the credential mutations (password / email change, invalidate-all) it is
    // CLI-binary-only by policy — an agent must not be able to trigger it. Guard
    // FIRST (before auth) so the intent is clear regardless of auth state.
    if action == "close" {
        return Ok(error_text(
            "user account close is CLI-binary-only: it PERMANENTLY closes your account \
             (irreversible). Run it via the CLI — `fastio user close <your-email-address>`. \
             The argument is the account's OWN email address (the server's confirmation \
             check), not a confirmation word. Rehearse first with `--dryrun`, which checks \
             eligibility without closing anything.",
        ));
    }
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "info" => handle_user_info(state, args).await,
        "update" => handle_user_update(state, args).await,
        "search" => handle_user_search(state, args).await,
        "details" => handle_user_details(state, args).await,
        "profiles" => handle_user_profiles(state, args).await,
        "allowed" => handle_user_allowed(state, args).await,
        "org-limits" => handle_user_org_limits(state, args).await,
        "shares" => handle_user_shares(state, args).await,
        "invitations-list" => handle_user_invitations_list(state, args).await,
        "invitations-details" => handle_user_invitations_details(state, args).await,
        "invitations-accept-all" => handle_user_invitations_accept_all(state, args).await,
        "asset-types" => handle_user_asset_types(state, args).await,
        "asset-list" => handle_user_asset_list(state, args).await,
        "asset-delete" => handle_user_asset_delete(state, args).await,
        "asset-upload" => handle_user_asset_upload(state, args).await,
        "asset-read" => handle_user_asset_read(state, args).await,
        "autosync" => handle_user_autosync(state, args).await,
        "pin" => handle_user_pin(state, args).await,
        "phone" => handle_user_phone(state, args).await,
        _ => Ok(error_text(&format!("Unknown user action: {action}"))),
    }
}

async fn handle_user_info(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::get_me(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    // display_name is an alias for first_name; merge them.
    let effective_first =
        optional_str(args, "first_name").or_else(|| optional_str(args, "display_name"));
    // Phone country and number must be sent together (server requires the pair).
    let phone_country = optional_str(args, "phone_country");
    let phone_number = optional_str(args, "phone_number");
    if phone_country.is_some() != phone_number.is_some() {
        return Ok(error_text(
            "phone_country and phone_number must be provided together",
        ));
    }
    // Password / email changes are credential mutations — CLI-only by policy,
    // so this MCP surface intentionally omits password and current_password.
    match api::user::update_user(
        &client,
        &api::user::UserUpdate {
            first_name: effective_first,
            last_name: optional_str(args, "last_name"),
            phone_country,
            phone_number,
            ..Default::default()
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_search(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let query = match required_str(args, "query") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::search_users(&client, query).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let user_id = match required_str(args, "user_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::get_user_by_id(&client, user_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_profiles(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::get_profiles(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_allowed(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::user_allowed(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_org_limits(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::user_org_limits(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_shares(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::list_user_shares(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_invitations_list(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::list_invitations(&client, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_invitations_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let invitation_id = match required_str(args, "invitation_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::get_invitation_details(&client, invitation_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_invitations_accept_all(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::accept_all_invitations(&client, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_asset_types(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::get_asset_types(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_asset_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let user_id = match required_str(args, "user_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::list_user_assets(&client, user_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_asset_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let asset_type = match required_str(args, "asset_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let me = match api::user::get_me(&client).await {
        Ok(v) => v,
        Err(e) => return Ok(cli_err_to_result(&e)),
    };
    let user_id = me
        .get("id")
        .or_else(|| me.get("profile_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if user_id.is_empty() {
        return Ok(error_text("Could not determine user ID"));
    }
    match api::user::delete_user_asset(&client, user_id, asset_type).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_asset_upload(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let asset_type = match required_str(args, "asset_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let file = match required_str(args, "file") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let me = match api::user::get_me(&client).await {
        Ok(v) => v,
        Err(e) => return Ok(cli_err_to_result(&e)),
    };
    let user_id = me
        .get("id")
        .or_else(|| me.get("profile_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if user_id.is_empty() {
        return Ok(error_text("Could not determine user ID"));
    }
    match api::user::upload_user_asset(&client, user_id, asset_type, file).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_asset_read(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let user_id = match required_str(args, "user_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let asset_type = match required_str(args, "asset_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let output = match required_str(args, "output") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::read_user_asset(&client, user_id, asset_type, std::path::Path::new(output))
        .await
    {
        Ok(bytes) => Ok(success_json(&json!({
            "status": "downloaded",
            "asset_type": asset_type,
            "output": output,
            "bytes": bytes,
        }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_autosync(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sync_state = match required_str(args, "state") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::autosync(&client, sync_state).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_pin(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::user::get_pin(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_user_phone(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let country_code = match required_str(args, "country_code") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let phone_number = match required_str(args, "phone_number") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::validate_phone(&client, country_code, phone_number).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Org tool handler.
async fn handle_org(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "list" => handle_org_list(state, args).await,
        "create" => handle_org_create(state, args).await,
        "info" => handle_org_info(state, args).await,
        "update" => handle_org_update(state, args).await,
        "delete" => handle_org_delete(state, args).await,
        // `billing-info` is the hidden back-compat alias for `billing-details`.
        "billing-details" | "billing-info" => handle_org_billing_details(state, args).await,
        "billing-plans" => handle_org_billing_plans(state, args).await,
        "billing-usage" => handle_org_billing_usage(state, args).await,
        "billing-meters" => handle_org_billing_meters(state, args).await,
        "members-list" => handle_org_members_list(state, args).await,
        "members-invite" => handle_org_members_invite(state, args).await,
        "members-remove" => handle_org_members_remove(state, args).await,
        "members-update-role" => handle_org_members_update_role(state, args).await,
        "transfer" => handle_org_transfer(state, args).await,
        "discover" | "discover-available" => handle_org_discover(state, args).await,
        "billing-cancel" => handle_org_billing_cancel(state, args).await,
        "billing-reactivate" => handle_org_billing_reactivate(state, args).await,
        "billing-members" => handle_org_billing_members(state, args).await,
        // `billing-create` is the hidden back-compat alias for `billing-subscribe`.
        "billing-subscribe" | "billing-create" => handle_org_billing_subscribe(state, args).await,
        "billing-invoices" => handle_org_billing_invoices(state, args).await,
        "members-details" => handle_org_members_details(state, args).await,
        "members-leave" => handle_org_members_leave(state, args).await,
        "members-join" => handle_org_members_join(state, args).await,
        "public-details" => handle_org_public_details(state, args).await,
        "limits" => handle_org_limits(state, args).await,
        "invitations-list" => handle_org_invitations_list(state, args).await,
        "invitations-update" => handle_org_invitations_update(state, args).await,
        "invitations-delete" => handle_org_invitations_delete(state, args).await,
        "transfer-token-create" => handle_org_transfer_token_create(state, args).await,
        "transfer-token-list" => handle_org_transfer_token_list(state, args).await,
        "transfer-token-delete" => handle_org_transfer_token_delete(state, args).await,
        "transfer-claim" => handle_org_transfer_claim(state, args).await,
        "discover-all" => handle_org_discover_all(state, args).await,
        "discover-check-domain" => handle_org_discover_check_domain(state, args).await,
        "discover-external" => handle_org_discover_external(state, args).await,
        "workspaces" => handle_org_workspaces(state, args).await,
        "shares" => handle_org_shares(state, args).await,
        "asset-types" => handle_org_asset_types(state, args).await,
        "asset-list" => handle_org_asset_list(state, args).await,
        "asset-delete" => handle_org_asset_delete(state, args).await,
        "create-workspace" => handle_org_create_workspace(state, args).await,
        _ => Ok(error_text(&format!("Unknown org action: {action}"))),
    }
}

async fn handle_org_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::org::list_orgs(
        &client,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let domain = match required_str(args, "domain") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::create_org(
        &client,
        domain,
        name,
        optional_str(args, "description"),
        optional_str(args, "industry"),
        optional_str(args, "billing_email"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_info(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::get_org(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::update_org(
        &client,
        &api::org::UpdateOrgParams {
            org_id,
            name: optional_str(args, "name"),
            domain: optional_str(args, "domain"),
            description: optional_str(args, "description"),
            industry: optional_str(args, "industry"),
            billing_email: optional_str(args, "billing_email"),
            homepage_url: optional_str(args, "homepage_url"),
            accent_color: optional_str(args, "accent_color"),
            background_color: optional_str(args, "background_color"),
            background_mode: optional_str(args, "background_mode"),
            use_background: optional_bool(args, "use_background"),
            facebook_url: optional_str(args, "facebook_url"),
            twitter_url: optional_str(args, "twitter_url"),
            instagram_url: optional_str(args, "instagram_url"),
            youtube_url: optional_str(args, "youtube_url"),
            perm_member_manage: optional_str(args, "perm_member_manage"),
            perm_authorized_domains: optional_str(args, "perm_authorized_domains"),
            owner_defined: optional_str(args, "owner_defined"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let confirm = match required_str(args, "confirm") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::close_org(&client, org_id, confirm).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Map a billing API error to an MCP result, appending the shared billing
/// recovery hint (`CliError::suggestion()` — which covers 402 / 1688 / 1695 /
/// 1696) so a subscription/credit error steers the agent to the billing-plan
/// surface.
/// The generic [`cli_err_to_result`] drops the suggestion; billing actions need
/// it to surface, mirroring `sign_err_to_result`.
fn billing_err_to_result(err: &fastio_cli::error::CliError) -> CallToolResult {
    if let Some(hint) = err.suggestion() {
        return error_text(&format!("{err} ({hint})"));
    }
    error_text(&err.to_string())
}

async fn handle_org_billing_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::get_billing_details(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_billing_usage(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::get_credit_usage(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_billing_plans(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::org::list_billing_plans(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_billing_meters(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let meter = match required_str(args, "meter") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::get_billing_meters(
        &client,
        &api::org::BillingMetersParams {
            org_id,
            meter,
            start_time: optional_str(args, "start_time"),
            end_time: optional_str(args, "end_time"),
            workspace_id: optional_str(args, "workspace_id"),
            share_id: optional_str(args, "share_id"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_members_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::list_org_members(
        &client,
        org_id,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_members_invite(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let email = match required_str(args, "email") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::invite_org_member(&client, org_id, email, optional_str(args, "role")).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_members_remove(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let member_id = match required_str(args, "member_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::remove_org_member(&client, org_id, member_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_members_update_role(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let member_id = match required_str(args, "member_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let role = match required_str(args, "role") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::update_org_member_role(&client, org_id, member_id, role).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_transfer(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let new_owner_id = match required_str(args, "new_owner_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::transfer_org_ownership(&client, org_id, new_owner_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_discover(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::org::discover_orgs(&client, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_billing_cancel(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    // The interactive CLI gates this DELETE behind `--yes`. The MCP surface is
    // non-interactive, so the same protection is a required-to-proceed
    // `confirm_cancel` boolean: reject BEFORE any API call unless it is
    // explicitly true, so an agent cannot cancel a paid subscription unprompted.
    if optional_bool(args, "confirm_cancel") != Some(true) {
        return Ok(error_text(BILLING_CANCEL_REJECTION));
    }
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // DELETE schedules cancellation at the end of the current billing period;
    // the org keeps access until cancel_at. Reversible via billing-reactivate.
    match api::org::billing_cancel(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

/// Error message returned when `billing-cancel` is invoked without an explicit
/// `confirm_cancel=true`. Mirrors the CLI `--yes` confirmation.
const BILLING_CANCEL_REJECTION: &str = "billing-cancel schedules the subscription to end at the close of the current billing \
     period; pass confirm_cancel=true to proceed (reverse it with billing-reactivate before \
     it executes).";

async fn handle_org_billing_reactivate(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // PUT — owner-only. Reverses a scheduled cancellation (no-op if none).
    match api::org::billing_reactivate(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_billing_members(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::billing_members(
        &client,
        org_id,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_billing_subscribe(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // POST — starts/updates a paid subscription. Returns a setup_intent +
    // public_key the user completes via the onboarding flow. plan_id is passed
    // through unvalidated (plan IDs drift; the server returns 1605 for a bad
    // plan). client_secret / public_key are sensitive and are NOT logged at
    // trace (the client redacts client_secret) and not cached here.
    match api::org::billing_create(&client, org_id, optional_str(args, "plan_id")).await {
        Ok(mut v) => {
            // The server response carries setup_intent / public_key but NOT the
            // hosted onboarding URL (a client-side constant). Surface the link
            // the tool promises (only when a new subscription still needs a
            // payment method) and strip the one-time secret + public_key so the
            // agent context never receives them.
            inject_onboarding_url(&mut v);
            sanitize_subscribe_response(&mut v);
            Ok(success_json(&v))
        }
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

/// Strip the sensitive fields from a `billing-subscribe` response BEFORE it is
/// returned to the MCP caller.
///
/// The 201 create response (see the published API docs) carries
/// `setup_intent.client_secret` (a real one-time secret) and a top-level
/// `public_key`. The caller completes payment via the injected
/// `onboarding_url`, never by handling the raw secret, so both are removed;
/// `setup_intent.id` / `setup_intent.status` and the rest of the response are
/// retained. Call AFTER [`inject_onboarding_url`] so the onboarding decision
/// still sees the untouched `setup_intent` / `is_active`.
fn sanitize_subscribe_response(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("public_key");
        if let Some(si) = obj.get_mut("setup_intent").and_then(Value::as_object_mut) {
            si.remove("client_secret");
        }
    }
}

/// Add the hosted onboarding URL to a subscribe response when the subscription
/// still needs a payment method (a `setup_intent` is present and the
/// subscription is not yet active). Mutates the response in place; a no-op for
/// the already-active update path. Does NOT echo `client_secret`/`public_key`.
fn inject_onboarding_url(value: &mut Value) {
    let needs_payment = value.get("setup_intent").is_some_and(|si| !si.is_null())
        && value.get("is_active").and_then(Value::as_bool) != Some(true);
    if needs_payment && let Some(obj) = value.as_object_mut() {
        obj.insert(
            "onboarding_url".to_owned(),
            Value::String("https://go.fast.io/onboarding".to_owned()),
        );
    }
}

async fn handle_org_billing_invoices(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::billing_invoices(
        &client,
        org_id,
        optional_u32(args, "limit"),
        optional_str(args, "starting_after"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_members_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let user_id = match required_str(args, "user_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::get_member_details(&client, org_id, user_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_members_leave(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::leave_org(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_members_join(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::join_org(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_public_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::get_public_details(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_limits(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // `limits` reaches the credit-usage billing endpoint, so its errors must
    // surface the shared billing hint (402 / 1688 / 1695 / 1696) too — route
    // through the billing-specific error mapper, not the generic one.
    match api::org::get_limits(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(billing_err_to_result(&e)),
    }
}

async fn handle_org_invitations_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::list_invitations(&client, org_id, optional_str(args, "state"), None, None).await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_invitations_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let inv_id = match required_str(args, "invitation_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::update_invitation(
        &client,
        org_id,
        inv_id,
        optional_str(args, "state"),
        optional_str(args, "role"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_invitations_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let inv_id = match required_str(args, "invitation_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::delete_invitation(&client, org_id, inv_id).await {
        Ok(_) => Ok(success_json(&json!({ "status": "deleted" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_transfer_token_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::transfer_token_create(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_transfer_token_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::transfer_token_list(&client, org_id, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_transfer_token_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let tok_id = match required_str(args, "token_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::transfer_token_delete(&client, org_id, tok_id).await {
        Ok(_) => Ok(success_json(&json!({ "status": "deleted" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_transfer_claim(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let token = match required_str(args, "token") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::transfer_claim(&client, token).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_discover_all(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::org::discover_all(&client, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_discover_check_domain(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let domain = match required_str(args, "domain_name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::discover_check_domain(&client, domain).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_discover_external(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::org::discover_external(&client, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_workspaces(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::list_workspaces(
        &client,
        org_id,
        None,
        None,
        match optional_bool_strict(args, "archived") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_shares(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::list_org_shares(&client, org_id, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_asset_types(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::org::org_asset_types(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_asset_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::list_org_assets(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_asset_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let asset_type = match required_str(args, "asset_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::delete_org_asset(&client, org_id, asset_type).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_create_workspace(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder_name = match optional_str_strict(args, "folder_name") {
        Ok(v) => v.unwrap_or(name),
        Err(e) => return Ok(e),
    };
    // The server has no defaults for these three required fields; mirror the
    // CLI defaults so MCP workspace-create succeeds without forcing the caller
    // to supply them.
    let perm_join = match optional_str_strict(args, "perm_join") {
        Ok(v) => v.unwrap_or("Member or above"),
        Err(e) => return Ok(e),
    };
    let perm_member_manage = match optional_str_strict(args, "perm_member_manage") {
        Ok(v) => v.unwrap_or("Admin or above"),
        Err(e) => return Ok(e),
    };
    // Parse the boolean toggles strictly: a PRESENT-but-invalid value (e.g.
    // `intelligence: "tru"`) must surface a clear error rather than silently
    // default to `false`/omitted. ABSENT stays None → default/omit.
    // ABSENT stays None so the field is OMITTED and the server applies its own
    // default of `true`; `unwrap_or(false)` here used to defeat the very thing
    // the comment above promises.
    let intelligence = match optional_bool_strict(args, "intelligence") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let params = api::org::CreateWorkspaceParams {
        folder_name,
        name,
        perm_join,
        perm_member_manage,
        intelligence,
        // Same opt-OUT contract as `intelligence`: ABSENT stays None so the
        // field is OMITTED and the server applies its own default of `true`.
        metadata_extraction: match optional_bool_strict(args, "metadata_extraction") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        },
        description: optional_str(args, "description"),
        accent_color: optional_str(args, "accent_color"),
        background_color1: optional_str(args, "background_color1"),
        background_color2: optional_str(args, "background_color2"),
    };
    match api::org::create_workspace(&client, org_id, &params).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Build the form-field map for an MCP `workspace` update from the tool args.
///
/// Forwards the advertised `intelligence` toggle as the string `"true"`/
/// `"false"` (the `/workspace/{id}/update/` endpoint takes it as a string form
/// field, per the published API docs) so AI indexing can be toggled through
/// MCP.
fn build_workspace_update_fields(
    args: &Map<String, Value>,
) -> std::collections::HashMap<String, String> {
    let mut fields = std::collections::HashMap::new();
    if let Some(v) = optional_str(args, "name") {
        fields.insert("name".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "description") {
        fields.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "folder_name") {
        fields.insert("folder_name".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_bool(args, "intelligence") {
        fields.insert("intelligence".to_owned(), v.to_string());
    }
    if let Some(v) = optional_bool(args, "metadata_extraction") {
        fields.insert("metadata_extraction".to_owned(), v.to_string());
    }
    if let Some(v) = optional_str(args, "perm_join") {
        fields.insert("perm_join".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "perm_member_manage") {
        fields.insert("perm_member_manage".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "accent_color") {
        fields.insert("accent_color".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "background_color1") {
        fields.insert("background_color1".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "background_color2") {
        fields.insert("background_color2".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "owner_defined") {
        fields.insert("owner_defined".to_owned(), v.to_owned());
    }
    fields
}

/// Workspace tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_workspace(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "list" => match api::workspace::list_workspaces(
            &client,
            optional_str(args, "org_id"),
            optional_u32(args, "limit"),
            optional_u32(args, "offset"),
            match optional_bool_strict(args, "archived") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            },
        )
        .await
        {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "create" => {
            let org_id = match required_str(args, "org_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let name = match required_str(args, "name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let folder_name = match optional_str_strict(args, "folder_name") {
                Ok(v) => v.unwrap_or(name),
                Err(e) => return Ok(e),
            };
            match api::workspace::create_workspace(
                &client,
                &api::workspace::CreateWorkspaceParams {
                    org_id,
                    folder_name,
                    name,
                    description: optional_str(args, "description"),
                    intelligence: optional_bool(args, "intelligence"),
                    metadata_extraction: optional_bool(args, "metadata_extraction"),
                },
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "info" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::get_workspace(&client, ws_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "update" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fields = build_workspace_update_fields(args);
            match api::workspace::update_workspace(&client, ws_id, &fields).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "delete" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let confirm = match required_str(args, "confirm") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::delete_workspace(&client, ws_id, confirm).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "search" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let query = match required_str(args, "query") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Route through the unified grouped-bucket search
            // (`/workspace/{id}/search/`) with the same `files`-bucket paging the
            // CLI `workspace search` uses (commands/workspace.rs), so MCP and CLI
            // return the identical API/shape/semantics (CLI/MCP parity).
            let params = api::search::UnifiedSearchParams::new()
                .files(optional_u32(args, "offset"), optional_u32(args, "limit"))
                .modes(build_search_modes(args));
            match api::search::unified_search_workspace(&client, ws_id, query, params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "limits" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Limits come from the dedicated `/workspace/{id}/limits/` endpoint
            // (distinct payload from `details`), matching the CLI
            // `workspace limits` command (CLI/MCP parity).
            match api::workspace::get_workspace_limits(&client, ws_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "archive" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::archive_workspace(&client, ws_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "unarchive" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::unarchive_workspace(&client, ws_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "members" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::list_workspace_members(
                &client,
                ws_id,
                optional_u32(args, "limit"),
                optional_u32(args, "offset"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "list-shares" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let archived = match optional_bool_strict(args, "archived") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::list_workspace_shares(
                &client,
                ws_id,
                optional_u32(args, "limit"),
                optional_u32(args, "offset"),
                archived,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "import-share" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::import_share(&client, ws_id, share_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "available" => match api::workspace::available_workspaces(&client).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "check-name" => {
            let name = match required_str(args, "name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::check_workspace_name(&client, name).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "create-note" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let parent = match optional_str_strict(args, "parent_id") {
                Ok(v) => v.unwrap_or("root"),
                Err(e) => return Ok(e),
            };
            let name = match required_str(args, "name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // `content` is required by `createnote/` (published API docs).
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::create_note(&client, ws_id, parent, name, content).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "update-note" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // STRICT for the same reason as `files update`: `optional_str` is
            // `as_str()`, so a present non-string would read as ABSENT and
            // silently drop the compare-and-swap, performing an unguarded
            // last-write-wins update the caller believed was protected.
            let if_version_id = match optional_str_strict(args, "if_version_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::update_note(
                &client,
                ws_id,
                node_id,
                optional_str(args, "name"),
                optional_str(args, "content"),
                if_version_id,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "read-note" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // STRICT: absence means "current version", so a present non-string
            // read as absent would silently return the CURRENT note to a caller
            // who pinned a historical one.
            let version_id = match optional_str_strict(args, "version_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::read_note(&client, ws_id, node_id, version_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "jobs-status" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::jobs_status(&client, ws_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "enable-import" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::enable_import(&client, ws_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "disable-import" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::disable_import(&client, ws_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-delete" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!("storage/{}/metadata/", urlencoding::encode(nid));
            // Forward the documented `keys` parameter (published API docs) as
            // a query string on the DELETE. `keys` is a JSON-encoded array of
            // key names to remove; when OMITTED the server deliberately
            // deletes ALL metadata keys for the node. `resolve_delete_keys`
            // distinguishes an ABSENT `keys` (purposeful delete-all, allowed)
            // from a PRESENT-but-blank/whitespace value (rejected, so a
            // malformed input cannot silently degrade into a destructive
            // delete-all).
            let params = match resolve_delete_keys(args) {
                Ok(None) => None,
                Ok(Some(raw)) => {
                    let mut m: std::collections::HashMap<String, String> =
                        std::collections::HashMap::new();
                    m.insert("keys".to_owned(), raw.to_owned());
                    Some(m)
                }
                Err(msg) => return Ok(error_text(msg)),
            };
            match api::workspace::metadata_api(
                &client,
                ws_id,
                &sub,
                "DELETE",
                None,
                None,
                params.as_ref(),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-details" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Accept either `node_ids` (JSON-encoded array of strings
            // OR a comma-separated list, up to 25 unique ids per the
            // bulk endpoint cap) or `node_id` (legacy single-id form).
            // When more than one id is provided, route to the bulk
            // endpoint and return the multi-format `{objects,
            // templates, errors}` response. Otherwise behave exactly
            // as the legacy tool.
            let raw_ids = optional_str(args, "node_ids")
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let raw_single = optional_str(args, "node_id")
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let parsed: Vec<String> = match (raw_ids, raw_single) {
                (Some(s), _) => {
                    if s.starts_with('[') {
                        match serde_json::from_str::<Vec<String>>(s) {
                            Ok(v) => v
                                .into_iter()
                                .map(|p| p.trim().to_owned())
                                .filter(|p| !p.is_empty())
                                .collect(),
                            Err(e) => {
                                return Ok(CallToolResult::error(vec![Content::text(format!(
                                    "node_ids must be a JSON array of strings or a comma-separated list: {e}"
                                ))]));
                            }
                        }
                    } else {
                        s.split(',')
                            .map(|p| p.trim().to_owned())
                            .filter(|p| !p.is_empty())
                            .collect()
                    }
                }
                (None, Some(s)) => vec![s.to_owned()],
                (None, None) => {
                    return Ok(CallToolResult::error(vec![Content::text(
                        "Missing required parameter: node_id (or node_ids)",
                    )]));
                }
            };
            if parsed.is_empty() {
                return Ok(CallToolResult::error(vec![Content::text(
                    "node_ids must contain at least one non-empty id",
                )]));
            }
            // Dedupe case-insensitively to match server normalization.
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut unique: Vec<String> = Vec::with_capacity(parsed.len());
            for id in parsed {
                if seen.insert(id.to_ascii_lowercase()) {
                    unique.push(id);
                }
            }
            // MCP always renders markdown (`success_json` → `to_markdown`), so
            // the declared-type strip is unconditional here — there is no
            // machine-readable JSON passthrough on this path to preserve.
            if unique.len() == 1 {
                match api::metadata::get_node_metadata_details(&client, ws_id, &unique[0]).await {
                    Ok(mut v) => {
                        api::metadata::strip_declared_metadata_types(&mut v);
                        Ok(success_json(&v))
                    }
                    Err(e) => Ok(cli_err_to_result(&e)),
                }
            } else {
                match api::metadata::get_bulk_node_metadata_details(&client, ws_id, &unique).await {
                    Ok(resp) => {
                        let mut payload = serde_json::json!({
                            "format": "multi",
                            "count_total": unique.len(),
                            "count_succeeded": resp.objects.len(),
                            "count_errored": resp.errors.len(),
                            "objects": resp.objects,
                            "templates": Value::Object(resp.templates),
                            "errors": resp
                                .errors
                                .iter()
                                .map(|e| serde_json::json!({
                                    "node_id": e.node_id,
                                    "code": e.code,
                                    "message": e.message,
                                }))
                                .collect::<Vec<_>>(),
                        });
                        api::metadata::strip_declared_metadata_types(&mut payload);
                        Ok(success_json(&payload))
                    }
                    Err(e) => Ok(cli_err_to_result(&e)),
                }
            }
        }
        "metadata-extract" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            match api::metadata::extract_node_metadata(&client, ws_id, nid, fields).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-extract-and-wait" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            let poll_interval = optional_u64(args, "poll_interval");
            Ok(metadata_extract_and_wait(&client, ws_id, nid, fields, poll_interval).await)
        }
        _ => Ok(error_text(&format!("Unknown workspace action: {action}"))),
    }
}

/// Files tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_files(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "list" => handle_files_list(state, args).await,
        "info" => handle_files_info(state, args).await,
        "create-folder" => handle_files_create_folder(state, args).await,
        "move" => handle_files_move(state, args).await,
        "copy" => handle_files_copy(state, args).await,
        "rename" => handle_files_rename(state, args).await,
        "update" => handle_files_update(state, args).await,
        "add-file" => handle_files_add_file(state, args).await,
        "delete" => handle_files_delete(state, args).await,
        "restore" => handle_files_restore(state, args).await,
        "purge" => handle_files_purge(state, args).await,
        "trash" => handle_files_trash(state, args).await,
        "versions" => handle_files_versions(state, args).await,
        "search" => handle_files_search(state, args).await,
        "recent" => handle_files_recent(state, args).await,
        "add-link" => handle_files_add_link(state, args).await,
        "transfer" => handle_files_transfer(state, args).await,
        "version-restore" => handle_files_version_restore(state, args).await,
        "lock-acquire" => handle_files_lock_acquire(state, args).await,
        "lock-status" => handle_files_lock_status(state, args).await,
        "lock-release" => handle_files_lock_release(state, args).await,
        "read" => handle_files_read(state, args).await,
        "content" => handle_files_content(state, args).await,
        "content-many" => handle_files_content_many(state, args).await,
        _ => Ok(error_text(&format!("Unknown files action: {action}"))),
    }
}

async fn handle_files_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    match api::storage::list_files(
        &client,
        context_type,
        ws_id,
        folder,
        optional_str(args, "sort_by"),
        optional_str(args, "sort_dir"),
        optional_u32(args, "page_size"),
        optional_str(args, "cursor"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}
async fn handle_files_info(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::get_file_details(&client, context_type, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_create_folder(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let parent = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    let force = match optional_bool_strict(args, "force") {
        Ok(v) => v.unwrap_or(false),
        Err(e) => return Ok(e),
    };
    match api::storage::create_folder(&client, context_type, ws_id, parent, name, force).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}
async fn handle_files_move(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let to = match required_str(args, "to") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::move_node(&client, context_type, ws_id, node_id, to).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_copy(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let to = match required_str(args, "to") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::copy_node(&client, context_type, ws_id, node_id, to).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_rename(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let new_name = match required_str(args, "new_name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::rename_node(&client, context_type, ws_id, node_id, new_name).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Update a node: rename, replace content (`from`), or set metadata overrides.
///
/// Workspace-scoped (mirrors the rest of the files tool). At least one of
/// `name`, `from`, `metadata_title`, or `metadata_short` must be provided.
async fn handle_files_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = optional_str(args, "name");
    let from = optional_str(args, "from");
    let metadata_title = optional_str(args, "metadata_title");
    let metadata_short = optional_str(args, "metadata_short");
    // STRICT, and the strictness is load-bearing: `optional_str` is `as_str()`,
    // so a present non-string (`{"if_version": 5}`) would read as ABSENT. Both
    // guards below key on `Some(..)`, so a mistyped precondition would slip past
    // both and send an UNGUARDED last-write-wins update while the agent believed
    // a compare-and-swap protected it. The CLI cannot hit this (clap yields a
    // String); only this JSON surface can, which is exactly why it belongs here.
    let if_version = match optional_str_strict(args, "if_version") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Same refusal the CLI applies: an empty precondition is not "no
    // precondition". One shared guard, so the two surfaces cannot diverge.
    if let Err(e) = api::storage::reject_unsupported_if_version(if_version) {
        return Ok(cli_err_to_result(&e));
    }
    if name.is_none() && from.is_none() && metadata_title.is_none() && metadata_short.is_none() {
        return Ok(error_text(
            "Provide at least one of: name, from, metadata_title, metadata_short",
        ));
    }
    // Same refusal the CLI applies, for the same measured reason: the server
    // creates no version for a metadata-only update, so `if_version_id` cannot
    // go stale and two writers on one base both succeed. One rule, both
    // surfaces — they must not diverge.
    if if_version.is_some() && name.is_none() && from.is_none() {
        return Ok(error_text(
            "if_version cannot protect a metadata-only update: the server creates no new version \
             for metadata_title/metadata_short, so the base never goes stale and a concurrent \
             writer on the same base also succeeds. Pair it with name or from (which do create a \
             version), or omit if_version and accept last-write-wins for metadata.",
        ));
    }
    match api::storage::update_node(
        &client,
        context_type,
        ws_id,
        node_id,
        name,
        from,
        metadata_title,
        metadata_short,
        if_version,
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Add a file to a folder from a completed upload or by content-hash dedup.
///
/// Workspace-scoped. Provide either `upload_id`, or `hash` with `hash_type`.
async fn handle_files_add_file(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let parent = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    // MCP add-file is workspace-scoped, and the workspace add-file handler does
    // not support a hash source (only `update` does). Reject a hash arg with the
    // same guard/message the CLI uses, BEFORE building or sending the request.
    if let Err(e) = crate::commands::files::validate_addfile_hash_context(
        "workspace",
        optional_str(args, "hash"),
    ) {
        return Ok(error_text(&e.to_string()));
    }
    let from = match crate::commands::files::build_addfile_from(
        optional_str(args, "upload_id"),
        optional_str(args, "hash"),
        optional_str(args, "hash_type"),
    ) {
        Ok(f) => f,
        Err(e) => return Ok(error_text(&e.to_string())),
    };
    match api::storage::add_file(&client, context_type, ws_id, parent, name, &from).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::delete_node(&client, context_type, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_restore(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::restore_node(&client, context_type, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_purge(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::purge_node(&client, context_type, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_trash(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::list_trash(
        &client,
        context_type,
        ws_id,
        optional_str(args, "sort_by"),
        optional_str(args, "sort_dir"),
        optional_u32(args, "page_size"),
        optional_str(args, "cursor"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_versions(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::list_versions(&client, context_type, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_search(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    /// Which profile this flat search targets. Local mirror of the CLI's
    /// `commands::files::SearchTarget`, borrowing the arg map's strings.
    enum Target<'a> {
        Workspace(&'a str),
        Share(&'a str),
    }

    let client = state.client().read().await;
    // Workspace XOR share — EXACTLY one, matching the CLI's clap rule rather
    // than preferring either.
    //
    // Preferring `share_id` when both are present would be the exact defect the
    // CLI half refuses ("silently searching somewhere the caller did not ask
    // for"), and it is reachable here in a way it is not on the CLI: `share_id`
    // is ALSO this tool's `add-link` parameter, so an agent reusing an argument
    // map from a previous step would have had its workspace search silently
    // redirected to a share.
    //
    // A present EMPTY string is rejected rather than treated as absent:
    // `optional_str_strict` reports `Some("")`, which would otherwise send an
    // empty id to the wire as a real target.
    let share_id = match optional_str_strict(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ws_id = match optional_str_strict(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let target = match (share_id, ws_id) {
        (Some(s), None) if !s.trim().is_empty() => Ok(Target::Share(s)),
        (None, Some(w)) if !w.trim().is_empty() => Ok(Target::Workspace(w)),
        (Some(_), Some(_)) => Err("supply workspace_id OR share_id, not both"),
        (None, None) => Err("Missing required parameter: workspace_id or share_id"),
        // Exactly one was supplied, but it was blank.
        _ => Err("workspace_id / share_id must not be empty"),
    };
    let target = match target {
        Ok(t) => t,
        Err(msg) => return Ok(error_text(msg)),
    };
    let query = match required_str(args, "query") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let filters = match search_filters_arg(args) {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let params = build_search_files_params(args).filters(filters.as_deref());
    // `filters` is workspace-only server-side; it is forwarded on the share leg
    // rather than refused, and the response-side check below reports a
    // predicate that did not run. Same policy as the CLI.
    let is_share = matches!(target, Target::Share(_));
    let result = match target {
        Target::Share(id) => api::storage::search_files_share(&client, id, query, params).await,
        Target::Workspace(id) => {
            api::storage::search_files(&client, "workspace", id, query, params).await
        }
    };
    // Folder-share remap, matching the CLI's `map_share_file_search_error` and
    // the unified tool's arm. Uses the FLAT wording — the unified constant says
    // "unified search", which would be wrong here. Every other error, including
    // the orphaned share (`179101` on this route), keeps the server's message.
    if is_share
        && let Err(ref e) = result
        && crate::commands::search::is_folder_share_search_error(e)
    {
        return Ok(error_text(
            crate::commands::files::FILE_SEARCH_UNAVAILABLE_ON_FOLDER_SHARE,
        ));
    }
    match result {
        // Normalize the node-id-keyed `files` MAP into a one-row-per-file ARRAY
        // so MCP matches the CLI search renderer (CLI/MCP parity). The
        // `details` (results[]) shape is left untouched by the normalizer.
        Ok(v) => {
            let mut v = api::storage::normalize_search_response(v);
            attach_content_search_warning(&mut v);
            if filters.is_some() {
                attach_metadata_filter_warnings(&mut v);
            }
            Ok(success_json(&v))
        }
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// The unified `search` tool: one query across a workspace or share, returned
/// as independently paginated buckets (`files`, `metadata`, `comments`).
///
/// Routes to the same `api::search` builders as the CLI `fastio search
/// workspace` / `fastio search share` commands, and reuses the CLI's own
/// `only` bucket filter and folder-share error wording, so the two surfaces
/// cannot drift (CLI/MCP parity).
async fn handle_search(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    let query = match required_str(args, "query") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let only = match only_buckets_arg(args) {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // STRICT pagination parsing: the lenient `optional_u32` reads a
    // present-but-unparseable value (`files_limit: "ten"`) as ABSENT, which
    // would silently run a DIFFERENT search than the caller asked for — a full
    // default page where they requested a narrow one — and answer 200 with no
    // way to tell the two apart. Pagination decides what the caller sees, so it
    // gets the strict form; a bad value is a loud error.
    //
    // Written as a macro so each parameter NAME sits at the call site next to
    // the bucket it pages. An earlier form parsed the six names into a
    // positional array and read them back by index, which worked but coupled
    // the list's order to the read order with nothing checking the two agreed —
    // reordering the list would have silently paged the wrong bucket.
    macro_rules! strict_u32 {
        ($key:literal) => {
            match optional_u32_strict(args, $key) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            }
        };
    }
    // `metadata_*` is workspace-only; the builder drops it on the share path,
    // so setting it unconditionally here is safe and keeps the two arms
    // symmetric.
    let params = api::search::UnifiedSearchParams::new()
        .files(strict_u32!("files_offset"), strict_u32!("files_limit"))
        .metadata(
            strict_u32!("metadata_offset"),
            strict_u32!("metadata_limit"),
        )
        .comments(
            strict_u32!("comments_offset"),
            strict_u32!("comments_limit"),
        )
        // Sent on BOTH actions. `details` is workspace-only in EFFECT, not in
        // acceptance: the share route takes it and returns no facts, so
        // suppressing it here would be a second, divergent client-side rule
        // for something the server already handles.
        .details(
            match optional_bool_strict(args, "details") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            }
            .unwrap_or(false),
        )
        .modes(match build_search_modes_strict(args) {
            Ok(v) => v,
            Err(e) => return Ok(e),
        });
    let result = match action {
        "workspace" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            api::search::unified_search_workspace(&client, ws_id, query, params).await
        }
        "share" => {
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            api::search::unified_search_share(&client, share_id, query, params).await
        }
        _ => {
            return Ok(error_text(&format!(
                "Unknown search action: {action} (expected `workspace` or `share`)"
            )));
        }
    };
    match result {
        Ok(mut v) => {
            crate::commands::search::apply_only_filter(&mut v, only.as_deref());
            Ok(success_json(&v))
        }
        // A share search against a workspace-backed (folder) share. The
        // classifier is code-specific, NOT a bare 404 — this endpoint also
        // 404s for an orphaned share and for a director miss, and those keep
        // the server's own (more accurate) message.
        Err(e)
            if action == "share" && crate::commands::search::is_folder_share_search_error(&e) =>
        {
            Ok(error_text(
                crate::commands::search::FOLDER_SHARE_SEARCH_UNAVAILABLE,
            ))
        }
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Resolve the `only` bucket filter of the unified `search` tool.
///
/// Deliberately **not** routed through `optional_str`, for the same reason
/// [`search_filters_arg`] is not: `only` is naturally written as a LIST of
/// bucket names, so an agent passing a real JSON array (`["files"]`) rather
/// than a comma-separated string is an expected spelling, and `optional_str`
/// reads any non-string as ABSENT. That silent default is not harmless here —
/// the response comes back carrying every bucket, which is indistinguishable
/// from "the filter ran and kept them all".
///
/// A **string** is forwarded as-is (the comma-separated form
/// [`crate::commands::search::apply_only_filter`] already parses), an **array**
/// of strings is joined into that form, and any other type is a loud error.
fn only_buckets_arg(args: &Map<String, Value>) -> Result<Option<String>, CallToolResult> {
    /// `only` was supplied but names no bucket (`[]`, `[""]`, `""`, `" , "`).
    ///
    /// Loud rather than a silent no-filter: supplying the parameter at all is a
    /// statement of intent to narrow, so answering with EVERY bucket is the
    /// same silent-default failure the strict parsing above exists to prevent —
    /// and it is indistinguishable from a filter that legitimately kept them.
    /// (The CLI's `--only ""` stays a no-op; that spelling is long-standing and
    /// separately tested. This is the MCP entry point only.)
    fn empty() -> CallToolResult {
        error_text(
            "Parameter `only` was supplied but names no bucket — omit it, or send null, \
             to show every bucket",
        )
    }
    let joined = match args.get("only") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                let Some(s) = item.as_str() else {
                    return Err(error_text(
                        "Parameter `only` must be a bucket name, a comma-separated list of \
                         them, or an array of them — every array entry must be a string",
                    ));
                };
                names.push(s);
            }
            names.join(",")
        }
        Some(_) => {
            return Err(error_text(
                "Parameter `only` must be a string (comma-separated bucket names) or an array \
                 of strings (omit it, or send null, to show every bucket)",
            ));
        }
    };
    // Mirror `apply_only_filter`'s own tokenizing so "present but names
    // nothing" is judged exactly as that function would judge it.
    if joined.split(',').all(|s| s.trim().is_empty()) {
        return Err(empty());
    }
    Ok(Some(joined))
}

/// Build [`api::storage::SearchFilesParams`] from MCP tool arguments for the
/// flat `files search` leg (`/storage/search/`). The unified `search` tool has
/// its own per-bucket parameters and does not use this.
fn build_search_files_params(args: &Map<String, Value>) -> api::storage::SearchFilesParams<'_> {
    api::storage::SearchFilesParams::new()
        .files_scope(optional_str(args, "files_scope"))
        .folders_scope(optional_str(args, "folders_scope"))
        .limit(optional_u32(args, "limit"))
        .offset(optional_u32(args, "offset"))
        // NOT converted to the strict form in the 2026-08-25 silent-default
        // sweep, and the reason is the enclosing signature, not the semantics:
        // this builder returns `SearchFilesParams` directly, so rejecting here
        // would mean threading a `Result` through a pure constructor. `details`
        // is a VERBOSITY flag — a discarded value costs the caller detail in the
        // response, never a different resource acted upon — so it is the one
        // site where that churn is not justified. Revisit if this fn ever gains
        // a fallible return.
        .details(optional_bool(args, "details").unwrap_or(false))
        .output(optional_str(args, "output"))
        .modes(build_search_modes(args))
}

/// Resolve the `filters` search argument to the exact query-parameter text.
///
/// Deliberately **not** routed through `optional_str`, and deliberately
/// fallible. `filters` is naturally written as a JSON **array**, so an MCP
/// caller passing a real array (rather than a JSON-encoded string) is the
/// expected case, not the exotic one — and `optional_str` reads any non-string
/// as ABSENT. That silent default is intolerable here in a way it is not for a
/// verbosity flag: a dropped predicate returns a `200` full of the wrong files
/// with nothing on the response to distinguish it from a filter that matched
/// broadly.
///
/// So: a **string** is shape-checked and then forwarded **verbatim** (already-
/// encoded callers keep working and large integers cannot lose precision), an
/// **array** is serialized, and any other type is a loud error rather than a
/// silent omission.
///
/// **All THREE input spellings must agree** — CLI inline text, CLI `@file`,
/// and this tool's array and encoded-string forms. The encoded string is the
/// easy one to get wrong: checking only for blank would forward `"[]"` as a
/// live predicate. The server reads `[]` as *"no filter at all"*
/// (the `filters` bullet list in the published API docs) and returns no
/// `metadata_filter` block, which then trips
/// [`METADATA_FILTER_ABSENT_WARNING`] — telling a caller who asked for exactly
/// no filter that their predicate silently failed. Shape errors would slip
/// through the same gap, so the string arm runs the CLI's own validator.
///
/// It deliberately calls [`crate::commands::validate_json_object_array`] rather
/// than `parse_json_object_array_arg`: the latter resolves the **`@path` file
/// form**, which must never be reachable from an MCP parameter.
///
/// Returned as an owned `String` because `SearchFilesParams` borrows its
/// values — the caller holds it alive across the request.
fn search_filters_arg(args: &Map<String, Value>) -> Result<Option<String>, CallToolResult> {
    match args.get("filters") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                return Ok(None);
            }
            crate::commands::validate_json_object_array(s, "`filters`")
                .map_err(|e| error_text(&e.to_string()))
        }
        Some(v @ Value::Array(items)) => {
            // `[]` is DEFINED as "no filter at all" (the `filters` bullet list
            // in the published API docs), so drop
            // it rather than refusing it or forwarding it. Dropping reproduces
            // the server's own semantics and keeps the caller out of the
            // "results are UNFILTERED" advisory, which would otherwise fire for
            // someone who asked for exactly that.
            if items.is_empty() {
                return Ok(None);
            }
            if !items.iter().all(Value::is_object) {
                return Err(error_text(
                    "every `filters` entry must be an object with `field` and `operator` keys, \
                     e.g. [{\"field\":\"category\",\"operator\":\"=\",\"value\":\"Legal\"}]",
                ));
            }
            Ok(Some(v.to_string()))
        }
        Some(_) => Err(error_text(
            "`filters` must be a JSON array of clause objects (or the equivalent JSON string); \
             it was neither, and it has NOT been applied",
        )),
    }
}

/// Warning text for a search that carried `filters` but came back with no
/// top-level `metadata_filter` acknowledgement.
///
/// The block is emitted only when a filter actually reached the server and ran,
/// so its absence means the results are **unfiltered** — an intermediary
/// dropped the unrecognised parameter, or this deployment does not accept it
/// yet. Both look like an ordinary successful search, which is exactly why this
/// is surfaced in-band rather than left for the caller to notice.
const METADATA_FILTER_ABSENT_WARNING: &str = "`filters` was sent but the response carries no `metadata_filter` block, so the predicate \
     DID NOT run — these results are UNFILTERED. Do not treat them as satisfying the filter, and \
     do not read a short list as 'few files matched'.";

/// Attach the metadata-filter advisories to a search payload.
///
/// Call only when `filters` was actually sent; for an unfiltered search the
/// block's absence is the normal response shape and carries no meaning.
/// `truncated` and `scope_incomplete` stay separate messages because one is
/// deterministic (narrow the filter — retrying is pointless) and the other is
/// transient (retrying is the correct action).
/// Attach a warning when the server reports that content search cannot work on
/// this profile, so an agent does not read an empty result as "nothing matched".
///
/// The MCP counterpart of the CLI's `warn_if_content_search_unavailable`. It
/// matters most on the SHARE leg: per the search contract,
/// `content_search_available` is reachable as `false` **only on share routes**,
/// so before `files search` accepted a `share_id` this state could not occur
/// here at all.
///
/// Reads both documented placements — top level on `/storage/search/`, and
/// `buckets.files.search_metadata` on the unified route — and treats absence as
/// "no report", never as a failure.
fn attach_content_search_warning(payload: &mut Value) {
    let meta = payload
        .get("search_metadata")
        .or_else(|| payload.pointer("/buckets/files/search_metadata"));
    let Some(meta) = meta else { return };
    if meta.get("content_search_available") != Some(&Value::Bool(false)) {
        return;
    }
    // `reason` is share-only and its value set may grow; an unknown value falls
    // back to the generic advice rather than being reported as a known cause.
    let detail = match meta.get("reason").and_then(Value::as_str) {
        Some("intelligence_disabled") => " (AI intelligence is disabled here)",
        Some("summary_permission_denied") => {
            " (this share does not grant access to file summaries)"
        }
        _ => "",
    };
    let warning = format!(
        "content search is NOT available on this profile{detail}, so an empty result reflects \
         that capability, not an absence of matching files. Re-run with search_in=\"filename\" \
         to search filenames instead."
    );
    attach_warning(payload, Some(&warning));
}

fn attach_metadata_filter_warnings(payload: &mut Value) {
    let Some(block) = api::storage::metadata_filter_block(payload) else {
        attach_warning(payload, Some(METADATA_FILTER_ABSENT_WARNING));
        return;
    };
    let truncated = block.get("truncated") == Some(&Value::Bool(true));
    let scope_incomplete = block.get("scope_incomplete") == Some(&Value::Bool(true));
    if truncated {
        attach_warning(
            payload,
            Some(
                "the filter's candidate set exceeded a cap and was clipped (`truncated`): \
                 results are incomplete and `matched` is a floor, not an exact count. This is \
                 deterministic — re-running returns the same answer. Narrow the filter instead.",
            ),
        );
    }
    if scope_incomplete {
        attach_warning(
            payload,
            Some(
                "a transient fault dropped candidates that genuinely match \
                 (`scope_incomplete`): this list is short for a reason unrelated to your filter. \
                 Re-run the same request — it may return more.",
            ),
        );
    }
}

/// Build the shared search-mode parameters (`search_in` / `name_match` /
/// `case_sensitive`) from MCP tool arguments.
///
/// Used by every MCP search handler — the unified `search` tool, `files
/// search`, and `workspace search` — so an agent gets the same vocabulary
/// everywhere.
/// Absent arguments leave the request byte-identical to today's.
/// Strict counterpart of [`build_search_modes`], for the unified `search` tool.
///
/// The lenient builder reads a present-but-wrong-typed value as ABSENT, so
/// `case_sensitive: "tru"` silently becomes `false` and a non-string
/// `search_in` silently becomes "unset" — each a 200 carrying results the
/// caller did not ask for, indistinguishable from ones they did.
///
/// Deliberately a SEPARATE function rather than a change to
/// [`build_search_modes`]: that one is shared with the pre-existing `files
/// search` / `workspace search` handlers, and tightening their accepted input
/// is a behaviour change to surfaces this work did not touch. New surface gets
/// the strict form; the older ones keep theirs until someone decides otherwise.
///
/// Note the VALUE of a well-typed string is still validated downstream by
/// `SearchModeParams::validate` (which rejects an unknown `search_in` /
/// `name_match`); this adds the TYPE check that ran nowhere before.
fn build_search_modes_strict(
    args: &Map<String, Value>,
) -> Result<api::types::SearchModeParams, CallToolResult> {
    Ok(api::types::SearchModeParams::new()
        .search_in(optional_str_strict(args, "search_in")?)
        .name_match(optional_str_strict(args, "name_match")?)
        .case_sensitive(optional_bool_strict(args, "case_sensitive")?))
}

fn build_search_modes(args: &Map<String, Value>) -> api::types::SearchModeParams {
    api::types::SearchModeParams::new()
        .search_in(optional_str(args, "search_in"))
        .name_match(optional_str(args, "name_match"))
        .case_sensitive(optional_bool(args, "case_sensitive"))
}

async fn handle_files_recent(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::list_recent(
        &client,
        context_type,
        ws_id,
        None,
        None,
        optional_str(args, "type"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_add_link(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = match required_str(args, "folder") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::add_link(&client, ws_id, folder, share_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_transfer(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let to_ws = match required_str(args, "to_workspace") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::transfer_node(&client, ws_id, node_id, to_ws).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_version_restore(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let version_id = match required_str(args, "version_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::version_restore(&client, context_type, ws_id, node_id, version_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_lock_acquire(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::lock_acquire(
        &client,
        context_type,
        ws_id,
        node_id,
        optional_u32(args, "duration"),
        optional_str(args, "client_info"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_lock_status(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::lock_status(&client, context_type, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_lock_release(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let lock_token = match required_str(args, "lock_token") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // The generic `api::locking` module owns all four lock operations; the
    // `api::storage` wrappers are workspace-context specializations.
    match api::locking::lock_release(&client, context_type, ws_id, node_id, lock_token).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_read(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, ws_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::read_content(&client, context_type, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `files action=content` — one file's extracted text as ordered chunks.
///
/// The window parameters are parsed STRICTLY. `optional_u32` reads a
/// present-but-unparseable value as ABSENT, which here would answer a
/// different read than the caller asked for — `chunk_from: "12a"` becoming
/// "start of file" — with a 200 and no way to tell the two apart. Every bound
/// beyond parseability belongs to `ContentReadParams::validate`, which both
/// front ends share, so a bad value comes back as the same readable message
/// on either surface rather than as a server `406`.
async fn handle_files_content(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let (context_type, profile_id) = match resolve_storage_context(args, "files") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    macro_rules! strict_u32 {
        ($key:literal) => {
            match optional_u32_strict(args, $key) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            }
        };
    }
    macro_rules! strict_str {
        ($key:literal) => {
            match optional_str_strict(args, $key) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            }
        };
    }
    let params = api::storage::ContentReadParams::new()
        .query(strict_str!("query"))
        .page(strict_u32!("page"))
        .chunks(strict_u32!("chunk_from"), strict_u32!("chunk_to"))
        .cursor(strict_str!("cursor"))
        .limit(strict_u32!("limit"))
        .max_bytes(strict_u32!("max_bytes"))
        .output(strict_str!("output"));
    // Validated here as well as inside the API fn: the message names the WIRE
    // parameters this tool advertises, so surfacing it before the request is
    // sent tells the agent exactly which argument to change.
    if let Err(e) = params.validate() {
        return Ok(cli_err_to_result(&e));
    }
    match api::storage::read_content_chunks(&client, context_type, profile_id, node_id, &params)
        .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `files action=content-many` — up to ten files scored against one query.
///
/// WORKSPACE ONLY. A `share_id` is refused with an explanation rather than
/// quietly ignored: `share_id` is also this tool's `add-link` parameter, so an
/// agent reusing an argument map from an earlier step would otherwise have its
/// share read silently answered from a workspace.
async fn handle_files_content_many(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    if let Some(share_id) = match optional_str_strict(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    } && !share_id.is_empty()
    {
        return Ok(error_text(
            "content-many reads several files at once and is WORKSPACE ONLY — there is no \
             share form of this route. Send workspace_id, or read the share's files one \
             at a time with action=content and node_id.",
        ));
    }
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let nodes = match optional_str_strict(args, "nodes") {
        Ok(Some(v)) => v,
        Ok(None) => {
            return Ok(error_text(
                "Missing required parameter: nodes (1-10 comma-separated file node IDs)",
            ));
        }
        Err(e) => return Ok(e),
    };
    let query = match required_str(args, "query") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let nodes: Vec<String> = nodes.split(',').map(str::to_owned).collect();
    let params = api::storage::ContentManyParams::new(nodes, query)
        .limit(match optional_u32_strict(args, "limit") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        })
        .max_bytes(match optional_u32_strict(args, "max_bytes") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        })
        .output(match optional_str_strict(args, "output") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        });
    if let Err(e) = params.validate() {
        return Ok(cli_err_to_result(&e));
    }
    match api::storage::read_content_many(&client, ws_id, &params).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Upload tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_upload(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "text" => handle_upload_text(state, args).await,
        "url" => handle_upload_url(state, args).await,
        "create-session" => handle_upload_create_session(state, args).await,
        "finalize" => handle_upload_finalize(state, args).await,
        "status" => handle_upload_status(state, args).await,
        "cancel" => handle_upload_cancel(state, args).await,
        "list-sessions" => handle_upload_list_sessions(state, args).await,
        "cancel-all" => handle_upload_cancel_all(state, args).await,
        "chunk-status" => handle_upload_chunk_status(state, args).await,
        "chunk-delete" => handle_upload_chunk_delete(state, args).await,
        "web-list" => handle_upload_web_list(state, args).await,
        "web-cancel" => handle_upload_web_cancel(state, args).await,
        "web-status" => handle_upload_web_status(state, args).await,
        "limits" => handle_upload_limits(state, args).await,
        "algos" => handle_upload_algos(state, args).await,
        "extensions" => handle_upload_extensions(state, args).await,
        "stream" => handle_upload_stream(state, args).await,
        "create-stream-session" => handle_upload_create_stream_session(state, args).await,
        "stream-send" => handle_upload_stream_send(state, args).await,
        _ => Ok(error_text(&format!("Unknown upload action: {action}"))),
    }
}

async fn handle_upload_text(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let content = match required_str(args, "content") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    // Create session, upload as single chunk, complete
    let content_bytes = content.as_bytes().to_vec();
    let size = u64::try_from(content_bytes.len()).unwrap_or(u64::MAX);
    match api::upload::create_upload_session(&client, ws_id, "workspace", folder, name, size).await
    {
        Ok(session) => {
            let upload_id = session.get("id").and_then(Value::as_str).unwrap_or("");
            if upload_id.is_empty() {
                return Ok(error_text(
                    "Failed to create upload session: no ID returned",
                ));
            }
            // Extract the token from the client (already holding the read lock).
            let token = client.get_token().unwrap_or_default().to_owned();
            match api::upload::upload_chunk(&token, state.api_base(), upload_id, 1, content_bytes)
                .await
            {
                Ok(_) => match api::upload::complete_upload(&client, upload_id).await {
                    Ok(v) => Ok(success_json(&v)),
                    Err(e) => Ok(cli_err_to_result(&e)),
                },
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}
async fn handle_upload_url(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let url = match required_str(args, "url") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    match api::upload::web_import(&client, ws_id, folder, url, optional_str(args, "name")).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_create_session(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let filesize_str = match required_str(args, "filesize") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let filesize = filesize_str.parse::<u64>().unwrap_or(0);
    let folder = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    match api::upload::create_upload_session(&client, ws_id, "workspace", folder, name, filesize)
        .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_finalize(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let upload_key = match required_str(args, "upload_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::upload::complete_upload(&client, upload_key).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_status(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let upload_key = match required_str(args, "upload_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::upload::get_upload_status(&client, upload_key).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_cancel(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let upload_key = match required_str(args, "upload_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::upload::cancel_upload(&client, upload_key).await {
        Ok(_) => Ok(success_json(&json!({ "status": "cancelled" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_list_sessions(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::upload::list_sessions(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_cancel_all(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::upload::cancel_all(&client).await {
        Ok(_) => Ok(success_json(&json!({ "status": "all_cancelled" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_chunk_status(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let upload_key = match required_str(args, "upload_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::upload::chunk_status(&client, upload_key).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_chunk_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let upload_key = match required_str(args, "upload_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chunk_num = match optional_u32_strict(args, "chunk_num") {
        Ok(v) => v.unwrap_or(0),
        Err(e) => return Ok(e),
    };
    match api::upload::chunk_delete(&client, upload_key, chunk_num).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_web_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    // Validate the optional `status` filter against the set the server accepts
    // (the CLI enforces the same set via a clap `value_parser`) so a bad value
    // gets a clear error instead of a server 400.
    let status = optional_str(args, "status");
    if let Some(s) = status
        && !WEB_UPLOAD_STATUSES.contains(&s)
    {
        return Ok(error_text(&format!(
            "Invalid status '{s}' (one of: {})",
            WEB_UPLOAD_STATUSES.join(", ")
        )));
    }
    match api::upload::web_list(
        &client,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
        status,
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_web_cancel(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let upload_id = match required_str(args, "upload_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::upload::web_cancel(&client, upload_id).await {
        Ok(_) => Ok(success_json(&json!({ "status": "cancelled" }))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_web_status(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let upload_id = match required_str(args, "upload_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::upload::web_import_status(&client, upload_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_limits(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    // `args["action"]` is the MCP routing action (always "limits" here), NOT the
    // API's create/update selector. Read that selector from `limit_action` so we
    // never forward `action=limits` (invalid) to `/upload/limits/`.
    let limit_action = optional_str(args, "limit_action");
    let instance_id = optional_str(args, "instance_id");
    let file_id = optional_str(args, "file_id");
    if let Err(e) =
        crate::commands::upload::validate_limits_action_context(limit_action, instance_id, file_id)
    {
        return Ok(error_text(&e.to_string()));
    }
    let client = state.client().read().await;
    match api::upload::upload_limits(
        &client,
        limit_action,
        optional_str(args, "org"),
        instance_id,
        optional_str(args, "folder_id"),
        file_id,
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_algos(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::upload::algos(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_extensions(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::upload::upload_extensions(&client, optional_str(args, "plan")).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Stream upload: create stream session, push content, auto-finalize.
async fn handle_upload_stream(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let content = match required_str(args, "content") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    let max_size = optional_str(args, "max_size").and_then(|s| s.parse::<u64>().ok());
    let hash = optional_str(args, "hash");
    let hash_algo = optional_str(args, "hash_algo");

    // Acquire the read lock, extract what we need, then drop it before the
    // potentially long-running stream upload so we don't block token refreshes.
    let (token, session_result) = {
        let client = state.client().read().await;
        let token = match client.get_token() {
            Some(t) => t.to_owned(),
            None => return Ok(error_text("No authentication token available")),
        };
        let result =
            api::upload::create_stream_session(&client, ws_id, "workspace", folder, name, max_size)
                .await;
        (token, result)
    };

    let content_bytes = bytes::Bytes::from(content.as_bytes().to_vec());
    match session_result {
        Ok(session) => {
            let upload_id = session.get("id").and_then(Value::as_str).unwrap_or("");
            if upload_id.is_empty() {
                return Ok(error_text(
                    "Failed to create stream session: no ID returned",
                ));
            }
            match api::upload::stream_upload(
                &token,
                state.api_base(),
                upload_id,
                content_bytes,
                hash,
                hash_algo,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => {
                    // Best-effort cleanup — the server may have already
                    // finalized the stream data before returning an error.
                    let client = state.client().read().await;
                    let _ = api::upload::cancel_upload(&client, upload_id).await;
                    Ok(cli_err_to_result(&e))
                }
            }
        }
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Create a streaming upload session (manual, no data sent).
async fn handle_upload_create_stream_session(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    let max_size = optional_str(args, "max_size").and_then(|s| s.parse::<u64>().ok());
    match api::upload::create_stream_session(&client, ws_id, "workspace", folder, name, max_size)
        .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Send data to an existing streaming upload session (auto-finalizes).
async fn handle_upload_stream_send(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let upload_key = match required_str(args, "upload_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let content = match required_str(args, "content") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let hash = optional_str(args, "hash");
    let hash_algo = optional_str(args, "hash_algo");

    // Extract token then drop the read guard before the potentially
    // long-running stream upload so we don't block token refreshes.
    let token = {
        let client = state.client().read().await;
        match client.get_token() {
            Some(t) => t.to_owned(),
            None => return Ok(error_text("No authentication token available")),
        }
    };

    let content_bytes = bytes::Bytes::from(content.as_bytes().to_vec());
    match api::upload::stream_upload(
        &token,
        state.api_base(),
        upload_key,
        content_bytes,
        hash,
        hash_algo,
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Download tool handler.
async fn handle_download(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "file-url" => {
            let ctx_type = match optional_str_strict(args, "context_type") {
                Ok(v) => v.unwrap_or("workspace"),
                Err(e) => return Ok(e),
            };
            let ctx_id = match required_str(args, "context_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // STRICT: a version PIN is a field where absence CHANGES the
            // request — omitted means "current". A present non-string read as
            // absent would silently serve the CURRENT file to a caller who asked
            // for a specific historical version, and nothing in the response says
            // the pin was dropped. Hoisted so the mint call and the URL builder
            // below cannot read it two different ways.
            let version_id = match optional_str_strict(args, "version_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::download::get_download_url_ctx(
                &client, ctx_type, ctx_id, node_id, version_id,
            )
            .await
            {
                Ok(resp) => {
                    let token = api::download::extract_download_token(&resp).unwrap_or_default();
                    let url = api::download::build_download_url_ctx(
                        state.api_base(),
                        ctx_type,
                        ctx_id,
                        node_id,
                        &token,
                        version_id,
                    );
                    // The URL already embeds the scoped read token in its
                    // `?token=` query param; surfacing the raw token separately
                    // is redundant secret exposure, so it is omitted. The URL
                    // itself is secret-bearing — do not log or share it.
                    Ok(success_json(&json!({
                        "download_url": url,
                        "note": "download_url is secret-bearing (carries a short-lived scoped read token). Do not log or share it.",
                    })))
                }
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "zip-url" => {
            let ctx_type = match optional_str_strict(args, "context_type") {
                Ok(v) => v.unwrap_or("workspace"),
                Err(e) => return Ok(e),
            };
            let ctx_id = match required_str(args, "context_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let url = api::download::get_zip_url_ctx(state.api_base(), ctx_type, ctx_id, node_id);
            Ok(success_json(&json!({ "zip_url": url })))
        }
        _ => Ok(error_text(&format!("Unknown download action: {action}"))),
    }
}

/// Share tool handler.
async fn handle_share(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "list" => handle_share_list(state, args).await,
        "create" => handle_share_create(state, args).await,
        "info" => handle_share_info(state, args).await,
        "update" => handle_share_update(state, args).await,
        "delete" => handle_share_delete(state, args).await,
        "files-list" => handle_share_files_list(state, args).await,
        "members-list" => handle_share_members_list(state, args).await,
        "members-add" => handle_share_members_add(state, args).await,
        "public-details" => handle_share_public_details(state, args).await,
        "archive" => handle_share_archive(state, args).await,
        "unarchive" => handle_share_unarchive(state, args).await,
        "password-auth" => handle_share_password_auth(state, args).await,
        "guest-auth" => handle_share_guest_auth(state, args).await,
        "available" => handle_share_available(state, args).await,
        "check-name" => handle_share_check_name(state, args).await,
        _ => Ok(error_text(&format!("Unknown share action: {action}"))),
    }
}

async fn handle_share_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::share::list_shares(
        &client,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    // `intelligence` is required server-side; default to false (AI off).
    // STRICT: a present non-bool must not be read as absent — see the
    // silent-default sweep note on `optional_str_strict`.
    let intelligence_flag = match optional_bool_strict(args, "intelligence") {
        Ok(v) => v.unwrap_or(false),
        Err(e) => return Ok(e),
    };
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let download_security = optional_str(args, "download_security");
    if let Some(ds) = download_security
        && !matches!(ds, "high" | "medium" | "off")
    {
        return Ok(error_text(
            "Invalid download_security value: must be \"high\", \"medium\", or \"off\"",
        ));
    }
    match api::share::create_share(
        &client,
        &api::share::CreateShareParams {
            workspace_id: ws_id,
            title: name,
            share_type: optional_str(args, "share_type"),
            description: optional_str(args, "description"),
            access_options: optional_str(args, "access_options"),
            invite: optional_str(args, "invite"),
            storage_mode: optional_str(args, "storage_mode"),
            folder_node_id: optional_str(args, "folder_node_id"),
            create_folder: optional_bool(args, "create_folder"),
            folder_name: optional_str(args, "folder_name"),
            custom_name: optional_str(args, "custom_name"),
            password: optional_str(args, "password"),
            expires: optional_str(args, "expires"),
            notify: optional_str(args, "notify"),
            comments_enabled: optional_bool(args, "comments_enabled"),
            download_security,
            guest_chat_enabled: optional_bool(args, "guest_chat_enabled"),
            display_type: optional_str(args, "display_type"),
            workspace_style: optional_str(args, "workspace_style"),
            anonymous_uploads_enabled: optional_bool(args, "anonymous_uploads_enabled"),
            intelligence: intelligence_flag,
            accent_color: optional_str(args, "accent_color"),
            background_color1: optional_str(args, "background_color1"),
            background_color2: optional_str(args, "background_color2"),
            background_image: optional_u32(args, "background_image").map(i64::from),
            link_1: optional_str(args, "link_1"),
            link_2: optional_str(args, "link_2"),
            link_3: optional_str(args, "link_3"),
            owner_defined: optional_str(args, "owner_defined"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_info(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::get_share_details(&client, share_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let download_security = optional_str(args, "download_security");
    if let Some(ds) = download_security
        && !matches!(ds, "high" | "medium" | "off")
    {
        return Ok(error_text(
            "Invalid download_security value: must be \"high\", \"medium\", or \"off\"",
        ));
    }
    match api::share::update_share(
        &client,
        &api::share::UpdateShareParams {
            share_id,
            name: optional_str(args, "name"),
            title: optional_str(args, "title"),
            custom_name: optional_str(args, "custom_name"),
            description: optional_str(args, "description"),
            share_type: optional_str(args, "share_type"),
            access_options: optional_str(args, "access_options"),
            invite: optional_str(args, "invite"),
            password: optional_str(args, "password"),
            expires: optional_str(args, "expires"),
            notify: optional_str(args, "notify"),
            download_enabled: optional_bool(args, "download_enabled"),
            comments_enabled: optional_bool(args, "comments_enabled"),
            download_security,
            display_type: optional_str(args, "display_type"),
            workspace_style: optional_str(args, "workspace_style"),
            guest_chat_enabled: optional_bool(args, "guest_chat_enabled"),
            intelligence: optional_bool(args, "intelligence"),
            anonymous_uploads_enabled: optional_bool(args, "anonymous_uploads_enabled"),
            accent_color: optional_str(args, "accent_color"),
            background_color1: optional_str(args, "background_color1"),
            background_color2: optional_str(args, "background_color2"),
            background_image: optional_u32(args, "background_image").map(i64::from),
            link_1: optional_str(args, "link_1"),
            link_2: optional_str(args, "link_2"),
            link_3: optional_str(args, "link_3"),
            owner_defined: optional_str(args, "owner_defined"),
            // `"null"` (the only accepted value) is passed through verbatim.
            share_link_node_id: optional_str(args, "share_link_node_id"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let confirm = match required_str(args, "confirm") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::delete_share(&client, share_id, confirm).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_files_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = match optional_str_strict(args, "folder") {
        Ok(v) => v.unwrap_or("root"),
        Err(e) => return Ok(e),
    };
    match api::share::list_share_files(
        &client,
        &api::share::ListShareFilesParams {
            share_id,
            parent_id: folder,
            sort_by: optional_str(args, "sort_by"),
            sort_dir: optional_str(args, "sort_dir"),
            page_size: optional_u32(args, "page_size"),
            cursor: optional_str(args, "cursor"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_members_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::list_share_members(
        &client,
        share_id,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_members_add(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let email = match required_str(args, "email") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::add_share_member(
        &client,
        &api::share::AddShareMemberParams {
            share_id,
            email_or_user_id: email,
            permissions: optional_str(args, "role"),
            notify_options: optional_str(args, "notify_options"),
            expires: optional_str(args, "expires"),
            force_notification: optional_bool(args, "force_notification"),
            message: optional_str(args, "message"),
            invitation_expires: optional_str(args, "invitation_expires"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_public_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::get_share_public_details(&client, share_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_archive(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::archive_share(&client, share_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_unarchive(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::unarchive_share(&client, share_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_password_auth(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let password = match required_str(args, "password") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::password_auth_share(&client, share_id, password).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_guest_auth(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::guest_auth(&client, share_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_available(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::share::available_shares(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_check_name(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::check_share_name(&client, name).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// AI tool handler.
async fn handle_ai(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "ask" => handle_ai_ask(state, args).await,
        "chat-create" => handle_ai_chat_create(state, args).await,
        "chat-list" => handle_ai_chat_list(state, args).await,
        "chat-details" => handle_ai_chat_details(state, args).await,
        "chat-update" => handle_ai_chat_update(state, args).await,
        "chat-delete" => handle_ai_chat_delete(state, args).await,
        "chat-publish" => handle_ai_chat_publish(state, args).await,
        "chat-cancel" => handle_ai_chat_cancel(state, args).await,
        "message-send" => handle_ai_message_send(state, args).await,
        "message-list" => handle_ai_message_list(state, args).await,
        "message-details" | "message-read" => handle_ai_message_details(state, args).await,
        "share-generate" => handle_ai_share_generate(state, args).await,
        "transactions" => handle_ai_transactions(state, args).await,
        "autotitle" => handle_ai_autotitle(state, args).await,
        _ => Ok(error_text(&format!("Unknown ripley action: {action}"))),
    }
}

/// Emit the `/ai/agent/` `references` (SCOPE) and `subjects` (ATTACH) form
/// fields from the MCP `files_scope` / `folders_scope` / `files_attach` args.
///
/// `files_scope` + `folders_scope` → `references`; `files_attach` → the SEPARATE
/// `subjects` array (the platform routes ATTACHED files there — the focus
/// surface — while `references` becomes inline content). Delegates to the shared
/// [`api::ai::build_references`] / [`api::ai::build_subjects`] so the CLI and MCP
/// paths emit byte-identical bodies. Each field is omitted when it has no items;
/// the inputs may be freely combined (no mutual-exclusion check).
fn apply_ai_scope_from_args(
    form: &mut std::collections::HashMap<String, String>,
    args: &Map<String, Value>,
) {
    let mut scope = api::ai::ChatScope::default();
    scope.files_scope = optional_str(args, "files_scope").map(str::to_owned);
    scope.folders_scope = optional_str(args, "folders_scope").map(str::to_owned);
    scope.files_attach = optional_str(args, "files_attach").map(str::to_owned);
    if let Some(references) = api::ai::build_references(&scope) {
        form.insert("references".to_owned(), references);
    }
    if let Some(subjects) = api::ai::build_subjects(&scope) {
        form.insert("subjects".to_owned(), subjects);
    }
}

/// Build the prominent note attached to a `needs_input` MCP `ask` result.
///
/// A `needs_input` turn answered with a clarifying question instead
/// of a full response, so surface the question (and how to reply) so the calling
/// agent treats this as "needs more info" and answers in the SAME chat rather
/// than reading an empty answer. Mirrors the CLI `render_answer` `needs_input`
/// branch; reuses the shared `extract_clarification_question`.
fn mcp_needs_input_note(msg_data: &Value, chat_id: &str) -> String {
    match api::ai::extract_clarification_question(msg_data) {
        Some(q) => format!(
            "Ripley needs more information to continue: {q} Reply by sending your answer as a \
             new message in this same chat (action=message-send, chat_id={chat_id})."
        ),
        None => format!(
            "Ripley needs more information to continue (no question text was included). Reply by \
             sending a new message in this same chat (action=message-send, chat_id={chat_id})."
        ),
    }
}

/// Map an AI chat publish error for MCP. A 403 means publishing is disabled
/// platform-wide (published API docs) — surface that clearly rather than a raw
/// 403. Mirrors the CLI `map_publish_error`; everything else defers to
/// `cli_err_to_result`.
fn ai_publish_err_to_result(err: &fastio_cli::error::CliError) -> CallToolResult {
    if let fastio_cli::error::CliError::Api(api) = err
        && api.http_status == 403
    {
        return error_text(&format!(
            "publishing chats publicly is currently disabled platform-wide (403). Chats published \
             before this change remain public. ({err})"
        ));
    }
    cli_err_to_result(err)
}

/// Map an AI chat send/create error for MCP. A conversation-too-large 409
/// (`STATE_TOO_LARGE`, identified by its per-call-site code) is PERMANENT:
/// surface a clear "start a new chat" message rather than the raw error.
/// Matches the SPECIFIC too-large codes ONLY — the create/message endpoints
/// also return 409 (`APP_CONFLICT`) for a RETRYABLE `SEQUENCE_FAILURE`
/// (retry the same idempotency key, do NOT start a new chat), so a bare 409
/// would mislabel it. Mirrors the CLI `map_ai_send_error`; everything else
/// (incl. other 409s) defers to `cli_err_to_result`.
/// Resolve the caller's `idempotency_key` or mint one, and stamp it on the form.
///
/// Every AI turn created over MCP must carry a replay guard: the error advice
/// above tells the agent to "retry the same idempotency key", and an agent
/// following that advice without one would bill a **second turn** every time.
/// Returns the key so the failure path can hand it back — advice naming a key
/// the caller has never seen is not advice.
fn stamp_idempotency_key(
    form: &mut std::collections::HashMap<String, String>,
    args: &Map<String, Value>,
) -> Result<String, CallToolResult> {
    let supplied = optional_str_strict(args, "idempotency_key")?;
    let key = match supplied {
        Some(k) if !k.trim().is_empty() => k.trim().to_owned(),
        Some(_) | None => api::ai::new_idempotency_key(),
    };
    if !key.is_empty() {
        form.insert("idempotency_key".to_owned(), key.clone());
    }
    Ok(key)
}

/// [`ai_send_err_to_result`], plus the key to replay on a retryable 409.
fn ai_send_err_with_key(
    err: &fastio_cli::error::CliError,
    idempotency_key: &str,
) -> CallToolResult {
    if let fastio_cli::error::CliError::Api(api) = err
        && api.http_status == 409
        && !fastio_cli::api::ai::CONVERSATION_TOO_LARGE_CODES.contains(&api.code)
        && !idempotency_key.is_empty()
    {
        return error_text(&format!(
            "retryable conflict (409) — retry this SAME call with \
             idempotency_key=\"{idempotency_key}\". That replays the turn instead of \
             creating (and billing) a second one. Do NOT start a new chat. ({err})"
        ));
    }
    ai_send_err_to_result(err)
}

fn ai_send_err_to_result(err: &fastio_cli::error::CliError) -> CallToolResult {
    // `api` shadows the `api` module here, so reference the shared const by its
    // fully-qualified path — the single source of truth shared with the CLI.
    if let fastio_cli::error::CliError::Api(api) = err
        && fastio_cli::api::ai::CONVERSATION_TOO_LARGE_CODES.contains(&api.code)
    {
        return error_text(&format!(
            "this conversation is too large to continue (409) — start a new chat (action=ask or \
             action=chat-create) to keep going. Retrying the same chat will not help. ({err})"
        ));
    }
    cli_err_to_result(err)
}

async fn handle_ai_chat_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // The `/ai/agent/` create endpoint is form-encoded. `type`/`personality`
    // are dead on the migrated agent endpoint, and file/folder context is the
    // single structured `references` field (built from `files_scope` /
    // `folders_scope` / `files_attach` via the shared `build_references`) — the
    // retired `nodes`/`folder_id`/`intelligence` and flat scope fields are gone.
    let mut form = std::collections::HashMap::new();
    // `question` is REQUIRED for chat create (published API docs). Reject a
    // create that omits it rather than silently sending a question-less body.
    let question = match required_str(args, "query_text") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    form.insert("question".to_owned(), question.to_owned());
    if let Some(v) = optional_str(args, "name") {
        form.insert("name".to_owned(), v.to_owned());
    }
    // `privacy`/`kind` are workspace-only — a share ignores them, so only
    // forward in a workspace context. For a share, a supplied `kind` OR
    // `privacy` is dropped rather than forwarded; surface a one-line note for
    // each instead of swallowing it silently.
    let mut warnings: Vec<&str> = Vec::new();
    if ctx_type == "workspace" {
        if let Some(v) = optional_str(args, "privacy") {
            form.insert("privacy".to_owned(), v.to_owned());
        }
        if let Some(v) = optional_str(args, "kind") {
            form.insert("kind".to_owned(), v.to_owned());
        }
    } else {
        if optional_str(args, "kind").is_some() {
            warnings.push(KIND_SHARE_WARNING);
        }
        if optional_str(args, "privacy").is_some() {
            warnings.push(PRIVACY_SHARE_WARNING);
        }
    }
    apply_ai_scope_from_args(&mut form, args);
    let idem = match stamp_idempotency_key(&mut form, args) {
        Ok(k) => k,
        Err(e) => return Ok(e),
    };
    match api::ai::ai_api_form(&client, ctx_type, ctx_id, "agent/", &form).await {
        Ok(mut v) => {
            for w in &warnings {
                attach_warning(&mut v, Some(w));
            }
            Ok(success_json(&v))
        }
        Err(e) => Ok(ai_send_err_with_key(&e, &idem)),
    }
}

async fn handle_ai_chat_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let mut params = std::collections::HashMap::new();
    if let Some(l) = optional_str(args, "limit") {
        params.insert("limit".to_owned(), l.to_owned());
    }
    if let Some(o) = optional_str(args, "offset") {
        params.insert("offset".to_owned(), o.to_owned());
    }
    // `kind` filters by chat kind (`user`/`agent`/`all`).
    if let Some(k) = optional_str(args, "kind") {
        params.insert("kind".to_owned(), k.to_owned());
    }
    let include_deleted = match optional_bool_strict(args, "include_deleted") {
        Ok(v) => v.unwrap_or(false),
        Err(e) => return Ok(e),
    };
    let sub = if include_deleted {
        "agent/list/deleted"
    } else {
        "agent/list/"
    };
    let p = if params.is_empty() {
        None
    } else {
        Some(&params)
    };
    match api::ai::ai_api(&client, ctx_type, ctx_id, sub, "GET", None, p).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_chat_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let sub = format!("agent/{}/details/", urlencoding::encode(chat_id));
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "GET", None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_chat_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Update is form-encoded (`-d "name=..."`).
    let mut form = std::collections::HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    let sub = format!("agent/{}/update/", urlencoding::encode(chat_id));
    match api::ai::ai_api_form(&client, ctx_type, ctx_id, &sub, &form).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_chat_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let sub = format!("agent/{}/", urlencoding::encode(chat_id));
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "DELETE", None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_chat_publish(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let sub = format!("agent/{}/publish/", urlencoding::encode(chat_id));
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "POST", None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(ai_publish_err_to_result(&e)),
    }
}

async fn handle_ai_chat_cancel(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::ai::cancel_message(&client, ctx_type, ctx_id, chat_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_message_send(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let query = match required_str(args, "query_text") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Follow-up messages are form-encoded; `type` is inherited from the chat and
    // `personality` is dead on the migrated agent endpoint. File/folder context
    // is the single structured `references` field (built from `files_scope` /
    // `folders_scope` / `files_attach` via the shared `build_references`).
    let mut form = std::collections::HashMap::new();
    form.insert("question".to_owned(), query.to_owned());
    apply_ai_scope_from_args(&mut form, args);
    let idem = match stamp_idempotency_key(&mut form, args) {
        Ok(k) => k,
        Err(e) => return Ok(e),
    };
    let sub = format!("agent/{}/message/", urlencoding::encode(chat_id));
    match api::ai::ai_api_form(&client, ctx_type, ctx_id, &sub, &form).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(ai_send_err_with_key(&e, &idem)),
    }
}

async fn handle_ai_message_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let mut params = std::collections::HashMap::new();
    if let Some(l) = optional_str(args, "limit") {
        params.insert("limit".to_owned(), l.to_owned());
    }
    if let Some(o) = optional_str(args, "offset") {
        params.insert("offset".to_owned(), o.to_owned());
    }
    let sub = format!("agent/{}/messages/list/", urlencoding::encode(chat_id));
    let p = if params.is_empty() {
        None
    } else {
        Some(&params)
    };
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "GET", None, p).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_message_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_id = match required_str(args, "chat_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let msg_id = match required_str(args, "message_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let sub = format!(
        "agent/{}/message/{}/details/",
        urlencoding::encode(chat_id),
        urlencoding::encode(msg_id),
    );
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "GET", None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_share_generate(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Both workspace and share contexts use the same form contract: a single
    // `files` field whose value is a JSON-array string of file IDs (NOT a
    // `nodes` CSV). Accept the IDs from either `files` or the legacy
    // `node_ids` arg for back-compat. `files` may be either a CSV string or a
    // JSON-array string (e.g. `["id1","id2"]`); both are accepted.
    let Some(ids_str) = optional_str(args, "files").or_else(|| optional_str(args, "node_ids"))
    else {
        return Ok(error_text(
            "share-generate requires `files` (comma-separated or JSON-array of file IDs)",
        ));
    };
    let trimmed = ids_str.trim();
    let file_ids: Vec<String> = if trimmed.starts_with('[') {
        match serde_json::from_str::<Vec<String>>(trimmed) {
            Ok(v) => v
                .into_iter()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            Err(e) => {
                return Ok(error_text(&format!(
                    "share-generate `files` looked like a JSON array but failed to parse: {e}"
                )));
            }
        }
    } else {
        trimmed
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    };
    if file_ids.is_empty() {
        return Ok(error_text("share-generate requires at least one file ID"));
    }
    // The `/ai/share/` endpoint caps `files` at 1-25 per the docs; reject
    // oversized requests before the network round-trip.
    if file_ids.len() > 25 {
        return Ok(error_text(&format!(
            "too many files: {} supplied, but AI share accepts at most 25",
            file_ids.len()
        )));
    }
    let form = api::ai::build_share_form(&file_ids);
    match api::ai::ai_api_form(&client, ctx_type, ctx_id, "share/", &form).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_transactions(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // AI transactions is WORKSPACE-ONLY — there is no share
    // equivalent. Reject a share (or any non-workspace) context rather than
    // mis-route to a non-existent `/share/{id}/ai/transactions/` endpoint.
    if ctx_type != "workspace" {
        return Ok(error_text(
            "transactions is workspace-only; set context_type=\"workspace\" (no share equivalent)",
        ));
    }
    // Delegate to the library helper, which builds the documented path.
    match api::ai::transactions(&client, ctx_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_autotitle(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Autotitle is SHARE-ONLY. Reject a workspace (or any
    // non-share) context rather than mis-route to a non-existent
    // `/workspace/{id}/ai/autotitle/` endpoint.
    if ctx_type != "share" {
        return Ok(error_text(
            "autotitle is share-only; set context_type=\"share\" (no workspace equivalent)",
        ));
    }
    // Delegate to the library helper, which form-encodes the optional
    // user-context under the contract key `user_context` (NOT `context`).
    // The MCP arg is advertised as `context`; map it to the library param.
    match api::ai::autotitle(&client, ctx_id, optional_str(args, "context")).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Maximum wall-clock the MCP `ask` wait loop spends before returning the
/// chat/message IDs with a `processing` state, in seconds. Kept under the
/// JWT lifetime so a stuck answer surfaces a clear partial result rather than
/// hanging the tool call. Mirrors the CLI `ask` budget.
const MCP_ASK_MAX_WAIT_SECS: u64 = 120;

/// Per-iteration activity long-poll `wait` hint (seconds); the server caps at 95s.
const MCP_ASK_POLL_WAIT_SECS: u32 = 20;

/// `ask`: create a chat, then synchronously wait (bounded activity-poll +
/// message-details confirmation) and return the final answer. This is the MCP
/// expression of "offload to Ripley" — MCP results are not streams, so a
/// synchronous wait + final answer is the right shape. `no_wait=true` returns
/// the chat/message IDs immediately.
async fn handle_ai_ask(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let question = match required_str(args, "query_text") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // `type`/`personality` are dead on the migrated agent endpoint. File/folder
    // context is the single structured `references` field (built from
    // `files_scope` / `folders_scope` / `files_attach` via the shared
    // `build_references`), so those inputs may be freely combined.
    let mut form = std::collections::HashMap::new();
    form.insert("question".to_owned(), question.to_owned());
    // `kind` is workspace-only — share chats reject it, so forward it only in a
    // workspace context and, for a share, surface a one-line note (rather than
    // silently dropping it). Keep the call lenient — no hard error.
    let mut kind_warning: Option<String> = None;
    if let Some(k) = optional_str(args, "kind") {
        if ctx_type == "workspace" {
            form.insert("kind".to_owned(), k.to_owned());
        } else {
            kind_warning = Some(KIND_SHARE_WARNING.to_owned());
        }
    }
    apply_ai_scope_from_args(&mut form, args);

    let idem = match stamp_idempotency_key(&mut form, args) {
        Ok(k) => k,
        Err(e) => return Ok(e),
    };
    let resp = match api::ai::ai_api_form(&client, ctx_type, ctx_id, "agent/", &form).await {
        Ok(v) => v,
        Err(e) => return Ok(ai_send_err_with_key(&e, &idem)),
    };

    // The migrated create response is {result, thread: {thread_id}, turn:
    // {turn_id}}; probe those FIRST, then legacy fallbacks.
    let chat_id = resp
        .get("thread")
        .and_then(|t| t.get("thread_id"))
        .or_else(|| resp.get("chat_id"))
        .or_else(|| resp.get("chat").and_then(|c| c.get("id")))
        .or_else(|| resp.get("id"))
        .and_then(json_value_id_to_string);
    let message_id = resp
        .get("turn")
        .and_then(|t| t.get("turn_id"))
        .or_else(|| resp.get("message_id"))
        .or_else(|| {
            resp.get("chat")
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("id"))
        })
        .or_else(|| resp.get("message").and_then(|m| m.get("id")))
        .and_then(json_value_id_to_string);

    let (Some(chat_id), Some(message_id)) = (chat_id, message_id) else {
        // No usable IDs — return the raw create response so the caller can see
        // what came back rather than failing opaquely.
        let mut resp = resp;
        attach_warning(&mut resp, kind_warning.as_deref());
        return Ok(success_json(&resp));
    };

    let no_wait = match optional_bool_strict(args, "no_wait") {
        Ok(v) => v.unwrap_or(false),
        Err(e) => return Ok(e),
    };
    if no_wait {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "state": "processing",
        });
        attach_warning(&mut payload, kind_warning.as_deref());
        return Ok(success_json(&payload));
    }

    Ok(mcp_ask_wait(
        &client,
        ctx_type,
        ctx_id,
        &chat_id,
        &message_id,
        kind_warning.as_deref(),
    )
    .await)
}

/// Stderr/response note emitted when `kind` is supplied for a share context.
/// `kind` is workspace-only (a share ignores it), so it is dropped rather than
/// forwarded; this surfaces the drop instead of silently swallowing the arg.
const KIND_SHARE_WARNING: &str = "kind is workspace-only and was ignored for this share";

/// Response note emitted when `privacy` is supplied for a share context.
/// `privacy` is workspace-only (a share ignores it), so it is dropped rather
/// than forwarded; this surfaces the drop instead of silently swallowing it.
const PRIVACY_SHARE_WARNING: &str = "privacy is workspace-only and was ignored for this share";

/// Append a free-form `warning` string to a JSON object payload under a
/// `warnings` array (creating it if absent). No-op for `None` or non-object
/// values. Lets the MCP `ask`/`chat-create` handlers surface a lenient note
/// (e.g. a dropped workspace-only `kind`) without changing the success shape.
fn attach_warning(payload: &mut Value, warning: Option<&str>) {
    let Some(warning) = warning else { return };
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("warnings")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(Value::Array(arr)) = obj.get_mut("warnings") {
            arr.push(Value::String(warning.to_owned()));
        }
    }
}

/// Build the auth-expired recovery message for the MCP `ask` wait loop.
///
/// On a mid-wait 401 the chat already exists; the message embeds the
/// `chat_id`/`message_id` and a re-check hint so an agent can recover after
/// re-authenticating, mirroring the CLI `ask` path.
fn ask_auth_expired_text(chat_id: &str, message_id: &str) -> String {
    format!(
        "authentication expired while waiting for the answer. The chat was created \
         (chat_id={chat_id}, message_id={message_id}); re-authenticate, then re-check with \
         action=message-details (chat_id={chat_id}, message_id={message_id})."
    )
}

/// Bounded wait for an `ask` answer in the MCP path: activity long-poll +
/// message-details confirmation, bounded by [`MCP_ASK_MAX_WAIT_SECS`].
///
/// Returns the completed message-details body on success, a `processing`
/// payload on timeout, or an auth-expired error result on a 401. Extracted
/// from `handle_ai_ask` to keep that handler within the line budget.
async fn mcp_ask_wait(
    client: &fastio_cli::client::ApiClient,
    ctx_type: &str,
    ctx_id: &str,
    chat_id: &str,
    message_id: &str,
    warning: Option<&str>,
) -> CallToolResult {
    // On a mid-wait 401 (JWT expired) the chat was already created, so surface
    // the recovery IDs and a re-check hint — mirroring the CLI `ask` path —
    // rather than an opaque "authentication expired" with no way to recover.
    let auth_expired = || error_text(&ask_auth_expired_text(chat_id, message_id));
    // Build the on-timeout `processing` payload. Used both by the deadline
    // checks and by the rate-limit clamp when the remaining wait is exhausted.
    let timeout_payload = || {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "state": "processing",
            "message": "Timed out waiting for the answer; re-check with action=message-details.",
        });
        attach_warning(&mut payload, warning);
        success_json(&payload)
    };
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(MCP_ASK_MAX_WAIT_SECS);
    let details_sub = format!(
        "agent/{}/message/{}/details/",
        urlencoding::encode(chat_id),
        urlencoding::encode(message_id),
    );
    let mut lastactivity: Option<String> = None;
    loop {
        // Re-check the deadline at the TOP of every iteration, before issuing
        // the message-details GET. Either poll arm below may sleep up to the
        // remaining wait (a 429 clamp can land exactly on the deadline); without
        // this check a woken iteration would issue one more details request that
        // could add the client's request timeout and overrun MCP_ASK_MAX_WAIT_SECS.
        // Mirrors the CLI `wait_for_answer` post-activity deadline check.
        if tokio::time::Instant::now() >= deadline {
            return timeout_payload();
        }
        match api::ai::ai_api(client, ctx_type, ctx_id, &details_sub, "GET", None, None).await {
            Ok(msg_data) => {
                // Unwrap the workspace `message` OR share `turn` detail wrapper
                // — a share `needs_input` turn has no `message` key,
                // so without this its `state` is missed and the loop polls to a
                // misleading timeout. Shared helper keeps CLI + MCP in lockstep.
                let msg = api::ai::message_detail(&msg_data);
                // The field is `status`, NOT `state`. Reading `state` instead
                // never sees a terminal turn, so every MCP `ripley ask` would
                // run its full wait budget.
                // `api::ai::turn_status` owns the `status`-then-legacy-`state`
                // precedence AND the string-or-numeric normalisation, so the CLI
                // and MCP wait loops are in lockstep by construction rather than
                // via two helpers that have to be kept matching.
                let state_str = api::ai::turn_status(msg);
                // Terminal: complete / failed / cancelled / lost / needs_input. A
                // `needs_input` turn answered with a clarifying
                // question is terminal too — without it the loop polls to a
                // misleading timeout. Reuses the shared classifier so the CLI
                // and MCP stay in lockstep.
                if api::ai::is_terminal_turn_status(&state_str) {
                    let mut msg_data = msg_data;
                    // For needs_input, surface the clarifying question
                    // prominently so the calling agent treats this as "needs
                    // more info" (terminal) and replies in the same chat,
                    // rather than reading an empty answer.
                    if state_str == "needs_input" {
                        let note = mcp_needs_input_note(&msg_data, chat_id);
                        attach_warning(&mut msg_data, Some(&note));
                    }
                    attach_warning(&mut msg_data, warning);
                    return success_json(&msg_data);
                }
            }
            Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 401 => {
                return auth_expired();
            }
            // Classify rather than swallow: a transient blip falls through to the
            // long-poll; a persistent 4xx (403/404/402/parse) is surfaced.
            Err(other) => match classify_wf_poll_error(&other) {
                WfPollAction::RateLimited { retry_after_secs } => {
                    if retry_after_secs > 0 {
                        // Clamp the rate-limit backoff to the remaining wait so a
                        // 429 with a long reset cannot push us past the deadline;
                        // a ~0 remaining returns the timeout payload.
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        if remaining.is_zero() {
                            return timeout_payload();
                        }
                        tokio::time::sleep(
                            remaining.min(std::time::Duration::from_secs(retry_after_secs)),
                        )
                        .await;
                    }
                }
                WfPollAction::RetryTransient => {}
                WfPollAction::Fatal(result) => return result,
            },
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return timeout_payload();
        }
        let remaining = deadline.saturating_duration_since(now).as_secs();
        let wait =
            MCP_ASK_POLL_WAIT_SECS.min(u32::try_from(remaining).unwrap_or(MCP_ASK_POLL_WAIT_SECS));
        match api::event::poll_activity(client, ctx_id, lastactivity.as_deref(), Some(wait), false)
            .await
        {
            Ok(poll) => {
                if let Some(ts) = poll.get("lastactivity").and_then(Value::as_str) {
                    lastactivity = Some(ts.to_owned());
                }
            }
            Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 401 => {
                return auth_expired();
            }
            // A transient poll error backs off briefly and retries; a persistent
            // 4xx is surfaced instead of looping to a misleading timeout.
            Err(other) => match classify_wf_poll_error(&other) {
                WfPollAction::RateLimited { retry_after_secs } => {
                    // Clamp the rate-limit backoff (with its 2s floor) to the
                    // remaining wait so a long 429 reset cannot overshoot the
                    // deadline; a ~0 remaining returns the timeout payload.
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return timeout_payload();
                    }
                    tokio::time::sleep(
                        remaining.min(std::time::Duration::from_secs(retry_after_secs.max(2))),
                    )
                    .await;
                }
                WfPollAction::RetryTransient => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                WfPollAction::Fatal(result) => return result,
            },
        }
    }
}

/// Normalise a JSON id value (string or numeric) to `String`.
fn json_value_id_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The message a disabled cloud-import surface returns.
///
/// Shared so the dispatch arm and the describe gate cannot drift apart — they
/// did, which is how `import describe` answered while `import list` refused.
const CLOUD_IMPORT_DISABLED_MSG: &str = "Cloud import is not yet available. Set FASTIO_ENABLE_CLOUD_IMPORT=1 to use import commands (cloud import must also be enabled for your workspace).";

/// The message a disabled E-Sign surface returns.
const ESIGN_DISABLED_MSG: &str = "E-Sign is currently disabled. Set FASTIO_ENABLE_ESIGN=1 to use sign commands (signing must also be enabled for your organization).";

/// Resolve the storage context — a workspace OR a share — for the `files` tool.
///
/// The published API docs document each of these storage endpoints for shares
/// as well as workspaces, and state outright that "share storage follows
/// identical patterns to workspace storage for all common operations". So the
/// context is resolved generically here rather than hardcoding `/workspace/…`,
/// matching the builder `update` and `addfile` already use two functions away.
///
/// `workspace_id` stays the default so every existing call is unchanged;
/// `share_id` selects the share leg. Naming both is an error rather than a
/// silent precedence rule — a caller that sends both does not know which it
/// wants, and picking one for them is how the wrong tree gets modified.
fn resolve_storage_context<'a>(
    args: &'a Map<String, Value>,
    tool: &str,
) -> Result<(&'static str, &'a str), CallToolResult> {
    let ws = optional_str_strict(args, "workspace_id")?;
    let sh = optional_str_strict(args, "share_id")?;
    match (ws, sh) {
        (Some(w), None) => Ok(("workspace", w)),
        (None, Some(s)) => Ok(("share", s)),
        (Some(_), Some(_)) => Err(error_text(
            "provide either workspace_id or share_id, not both",
        )),
        (None, None) => Err(error_text(&format!(
            "Missing required parameter: workspace_id (or share_id to target a share) for {tool}"
        ))),
    }
}

/// Which comment route an `entity_type` is destined for.
///
/// The two allow-lists are deliberately different: `fileshare` is valid on the
/// node-scoped routes and NOT on the container-scoped listing, because
/// the published API docs say a File Share comment "always targets a specific
/// node, never the whole container".
#[derive(Clone, Copy)]
enum EntityScope {
    /// `/comments/{entity_type}/{parent_id}/{node_id}/` — accepts `fileshare`.
    Node,
    /// `/comments/{entity_type}/{parent_id}/` — no node segment, so no fileshare.
    Container,
}

/// Validate a comment `entity_type` against the SAME lists the CLI uses.
///
/// Previously the MCP side hand-rolled only the `fileshare` rejection on
/// `list-all` and did no validation at all on the node-scoped actions — so
/// `entity_type: "org"` was forwarded to the server over MCP while clap refused
/// it on the CLI, and the two surfaces disagreed about the same concept. Reusing
/// `commands::comment`'s constants means there is one list per scope rather than
/// one per call site.
fn validate_comment_entity_type(
    entity_type: &str,
    scope: EntityScope,
) -> Result<(), CallToolResult> {
    use crate::commands::comment::{CONTAINER_ENTITY_TYPES, VALID_ENTITY_TYPES};

    let allowed = match scope {
        EntityScope::Node => VALID_ENTITY_TYPES,
        EntityScope::Container => CONTAINER_ENTITY_TYPES,
    };
    if allowed.contains(&entity_type) {
        return Ok(());
    }
    // `fileshare` on the container route gets the actionable message rather than
    // a bare list — it is a real entity type used on the wrong route.
    if matches!(scope, EntityScope::Container) && entity_type == "fileshare" {
        return Err(error_text(
            "entity_type 'fileshare' requires a node and is not valid for action='list-all': \
             a File Share comment always targets a specific file, never the whole container. \
             Use action='list' with node_id instead.",
        ));
    }
    Err(error_text(&format!(
        "invalid entity_type '{entity_type}'. Must be one of: {}",
        allowed.join(", ")
    )))
}

/// Member tool handler.
async fn handle_member(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let entity_type = match optional_str_strict(args, "entity_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    match action {
        "list" => handle_member_list(state, args, entity_type).await,
        "add" => handle_member_add(state, args, entity_type).await,
        "remove" => handle_member_remove(state, args, entity_type).await,
        "update" => handle_member_update(state, args, entity_type).await,
        "info" => handle_member_info(state, args, entity_type).await,
        "transfer-ownership" => handle_member_transfer_ownership(state, args, entity_type).await,
        "leave" => handle_member_leave(state, args, entity_type).await,
        "join" => handle_member_join(state, args, entity_type).await,
        "join-invitation" => handle_member_join_invitation(state, args, entity_type).await,
        _ => Ok(error_text(&format!("Unknown member action: {action}"))),
    }
}

async fn handle_member_list(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::list_members(
        &client,
        entity_type,
        entity_id,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_member_add(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let email = match required_str(args, "email") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::add_member(
        &client,
        entity_type,
        entity_id,
        email,
        optional_str(args, "role"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_member_remove(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let member_id = match required_str(args, "member_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::remove_member(&client, entity_type, entity_id, member_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_member_update(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let member_id = match required_str(args, "member_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let role = match required_str(args, "role") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::update_member_role(&client, entity_type, entity_id, member_id, role).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_member_info(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let member_id = match required_str(args, "member_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::get_member_details(&client, entity_type, entity_id, member_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_member_transfer_ownership(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let member_id = match required_str(args, "member_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::transfer_ownership(&client, entity_type, entity_id, member_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_member_leave(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::leave(&client, entity_type, entity_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_member_join(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::join(&client, entity_type, entity_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Accept or decline a keyed invitation.
///
/// Takes `entity_type` like the other eight member actions, so an agent
/// accepting a SHARE invitation sends the share id down the share route rather
/// than the workspace one — `api::member::join_invitation` is routed on the
/// same value the member schema advertises ("workspace or share").
async fn handle_member_join_invitation(
    state: &McpState,
    args: &Map<String, Value>,
    entity_type: &str,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let key = match required_str(args, "invitation_key") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let inv_action = match required_str(args, "invitation_action") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::member::join_invitation(&client, entity_type, entity_id, key, inv_action).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Comment tool handler.
async fn handle_comment(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "list" => handle_comment_list(state, args).await,
        "create" => handle_comment_create(state, args).await,
        "reply" => handle_comment_reply(state, args).await,
        "update" => handle_comment_update(state, args).await,
        "delete" => handle_comment_delete(state, args).await,
        "list-all" => handle_comment_list_all(state, args).await,
        "details" => handle_comment_details(state, args).await,
        "bulk-delete" => handle_comment_bulk_delete(state, args).await,
        "reaction-add" => handle_comment_reaction_add(state, args).await,
        "reaction-remove" => handle_comment_reaction_remove(state, args).await,
        "list-attachments" => handle_comment_list_attachments(state, args).await,
        "attach" => handle_comment_attach(state, args).await,
        "detach" => handle_comment_detach(state, args).await,
        _ => Ok(error_text(&format!("Unknown comment action: {action}"))),
    }
}

async fn handle_comment_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_type = match required_str(args, "entity_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if let Err(e) = validate_comment_entity_type(entity_type, EntityScope::Node) {
        return Ok(e);
    }
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let sort = match validate_comment_sort(args) {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::list_comments(
        &client,
        &api::comment::ListCommentsParams {
            entity_type,
            entity_id,
            node_id,
            sort,
            limit: optional_u32(args, "limit"),
            offset: optional_u32(args, "offset"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `comment` create action: add a comment, optionally with an anchoring
/// `reference`, arbitrary `properties`, and inline attachment(s)
/// (`target_id` single OR `target_ids` batch, ≤25 — mutually exclusive).
async fn handle_comment_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_type = match required_str(args, "entity_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if let Err(e) = validate_comment_entity_type(entity_type, EntityScope::Node) {
        return Ok(e);
    }
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let text = match required_str(args, "text") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let reference = match json_object_arg(args, "reference") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let properties = match json_object_arg(args, "properties") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let target_id = optional_str(args, "target_id");
    let target_ids = match string_list_arg(args, "target_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if target_id.is_some() && !target_ids.is_empty() {
        return Ok(error_text(
            "target_id and target_ids are mutually exclusive",
        ));
    }
    if target_ids.len() > 25 {
        return Ok(error_text(&format!(
            "a comment accepts at most 25 attachments (got {})",
            target_ids.len()
        )));
    }
    match api::comment::add_comment(
        &client,
        &api::comment::AddCommentParams {
            entity_type,
            entity_id,
            node_id,
            body: text,
            parent_id: None,
            reference: reference.as_ref(),
            properties: properties.as_ref(),
            target_id,
            target_ids: (!target_ids.is_empty()).then_some(target_ids.as_slice()),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_reply(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_type = match required_str(args, "entity_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if let Err(e) = validate_comment_entity_type(entity_type, EntityScope::Node) {
        return Ok(e);
    }
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let text = match required_str(args, "text") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::add_comment(
        &client,
        &api::comment::AddCommentParams {
            entity_type,
            entity_id,
            node_id,
            body: text,
            parent_id: Some(comment_id),
            reference: None,
            properties: None,
            target_id: None,
            target_ids: None,
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `comment` update action: edit a comment's body by id.
///
/// This is the generic comment-edit path — it edits any comment by its comment
/// id. The new body accepts either `text` (preferred) or `body` (alias).
async fn handle_comment_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let Some(text) = optional_str(args, "text").or_else(|| optional_str(args, "body")) else {
        return Ok(error_text("Missing required parameter: text (or body)"));
    };
    match api::comment::update_comment(&client, comment_id, text).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::delete_comment(&client, comment_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_list_all(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_type = match required_str(args, "entity_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if let Err(e) = validate_comment_entity_type(entity_type, EntityScope::Container) {
        return Ok(e);
    }
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let sort = match validate_comment_sort(args) {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::list_all_comments(
        &client,
        entity_type,
        entity_id,
        sort,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::get_comment_details(&client, comment_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_bulk_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ids = match string_list_arg(args, "comment_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if ids.is_empty() {
        return Ok(error_text(
            "comment_ids must contain at least one non-empty comment ID",
        ));
    }
    if ids.len() > 100 {
        return Ok(error_text(&format!(
            "bulk-delete accepts at most 100 comment ids (got {})",
            ids.len()
        )));
    }
    match api::comment::bulk_delete_comments(&client, &ids).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_reaction_add(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let emoji = match required_str(args, "emoji") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::add_reaction(&client, comment_id, emoji).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_reaction_remove(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::remove_reaction(&client, comment_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `comment` list-attachments action: list a comment's attachments (hydrated,
/// access-gated).
async fn handle_comment_list_attachments(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::list_comment_attachments(&client, comment_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `comment` attach action: attach one object (`target_id`) or many
/// (`target_ids`, ≤25) to a comment. The two are mutually exclusive; exactly one
/// must be supplied. Author-only — the server enforces it.
async fn handle_comment_attach(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let target_id = optional_str(args, "target_id");
    let target_ids = match string_list_arg(args, "target_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let targets = match (target_id, target_ids.is_empty()) {
        (Some(id), true) => api::comment::CommentAttachTargets::Single(id),
        (None, false) => api::comment::CommentAttachTargets::Multiple(target_ids.as_slice()),
        (Some(_), false) => {
            return Ok(error_text(
                "target_id and target_ids are mutually exclusive",
            ));
        }
        (None, true) => {
            return Ok(error_text("one of target_id or target_ids is required"));
        }
    };
    if target_ids.len() > 25 {
        return Ok(error_text(&format!(
            "a comment accepts at most 25 attachments (got {})",
            target_ids.len()
        )));
    }
    match api::comment::attach_comment(&client, comment_id, &targets).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `comment` detach action: detach a single object from a comment (single only —
/// there is no batch detach).
async fn handle_comment_detach(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let target_id = match required_str(args, "target_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::detach_comment(&client, comment_id, target_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Event tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_event(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "search" => {
            // Strict bool: a present-but-malformed `acknowledged` errors rather
            // than silently widening the query (parity with the CLI's
            // typed `Option<bool>`).
            let acknowledged = match optional_bool_strict(args, "acknowledged") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // The SCOPE ids decide which events come back, so a mistyped one
            // must never read as "unscoped" — see `optional_id`.
            let (workspace_id, share_id, user_id, org_id) = match (
                optional_id(args, "workspace_id"),
                optional_id(args, "share_id"),
                optional_id(args, "user_id"),
                optional_id(args, "org_id"),
            ) {
                (Ok(w), Ok(s), Ok(u), Ok(o)) => (w, s, u, o),
                (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (_, _, _, Err(e)) => {
                    return Ok(e);
                }
            };
            match api::event::search_events(
                &client,
                &api::event::SearchEventsParams {
                    workspace_id: workspace_id.as_deref(),
                    share_id: share_id.as_deref(),
                    user_id: user_id.as_deref(),
                    org_id: org_id.as_deref(),
                    event: optional_str(args, "event"),
                    category: optional_str(args, "category"),
                    subcategory: optional_str(args, "subcategory"),
                    parent_event_id: optional_str(args, "parent_event_id"),
                    calling_user_id: optional_str(args, "calling_user_id"),
                    object_id: optional_str(args, "object_id"),
                    visibility: optional_str(args, "visibility"),
                    acknowledged,
                    created_min: optional_str(args, "created_min"),
                    created_max: optional_str(args, "created_max"),
                    limit: optional_u32(args, "limit"),
                    offset: optional_u32(args, "offset"),
                },
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "summarize" => {
            // summarize accepts every search filter plus `user_context`
            // (events.txt), so forward the same audit filters — including the
            // strict `acknowledged` — that `search` does.
            let acknowledged = match optional_bool_strict(args, "acknowledged") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::event::summarize_events(
                &client,
                &api::event::SummarizeEventsParams {
                    workspace_id: optional_str(args, "workspace_id"),
                    share_id: optional_str(args, "share_id"),
                    user_id: optional_str(args, "user_id"),
                    org_id: optional_str(args, "org_id"),
                    event: optional_str(args, "event"),
                    category: optional_str(args, "category"),
                    subcategory: optional_str(args, "subcategory"),
                    parent_event_id: optional_str(args, "parent_event_id"),
                    calling_user_id: optional_str(args, "calling_user_id"),
                    object_id: optional_str(args, "object_id"),
                    visibility: optional_str(args, "visibility"),
                    acknowledged,
                    created_min: optional_str(args, "created_min"),
                    created_max: optional_str(args, "created_max"),
                    user_context: optional_str(args, "user_context"),
                    limit: optional_u32(args, "limit"),
                    offset: optional_u32(args, "offset"),
                },
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "details" => {
            let event_id = match required_str(args, "event_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::event::get_event_details(&client, event_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "ack" => {
            let event_id = match required_str(args, "event_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::event::acknowledge_event(&client, event_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "activity-list" => {
            let profile_id = match required_str(args, "profile_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let cursor = optional_str(args, "cursor");
            let updated = cursor.is_some();
            match api::event::poll_activity(&client, profile_id, cursor, None, updated).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "activity-poll" => {
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::event::poll_activity(
                &client,
                entity_id,
                optional_str(args, "lastactivity"),
                optional_u32(args, "wait"),
                false,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown event action: {action}"))),
    }
}

/// Dashboard tool handler.
///
/// Reads the calling member's per-workspace actionable card feed (`get`) and
/// dismisses / snoozes / undismisses a card. All three are read or reversible
/// (undismiss reverses dismiss; dismiss/snooze only change the caller's own
/// view), so all are exposed over MCP.
async fn handle_dashboard(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "get" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::dashboard::get_dashboard(
                &client,
                workspace_id,
                optional_u32(args, "limit"),
                optional_u32(args, "offset"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "dismiss" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let card_key = match required_str(args, "card_key") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::dashboard::dismiss_card(
                &client,
                workspace_id,
                card_key,
                optional_str(args, "snooze_until"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "undismiss" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let card_key = match required_str(args, "card_key") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::dashboard::undismiss_card(&client, workspace_id, card_key).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown dashboard action: {action}"))),
    }
}

/// How-To tool handler.
///
/// A read-only, grounded product-guidance call — ideal for agents needing
/// runtime "how do I…" answers. When invoked over MCP, `surface` DEFAULTS to
/// `mcp` so the answer is phrased in terms of the consolidated MCP tools, unless
/// the caller overrides it (e.g. `code`, or any other value for REST phrasing).
async fn handle_howto(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "ask" => {
            let question = match required_str(args, "question") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Default the MCP surface to `mcp` so guidance is phrased for these
            // tools; an explicit `surface` arg (code / rest / …) overrides.
            let surface = optional_str(args, "surface").or(Some("mcp"));
            match api::howto::ask(
                &client,
                question,
                optional_str(args, "context"),
                surface,
                Some("fastio-cli-embedded"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown howto action: {action}"))),
    }
}

/// Invitation tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_invitation(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "list" => match api::invitation::list_user_invitations(&client, None, None).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "list-entity" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::invitation::list_invitations(
                &client,
                entity_type,
                entity_id,
                optional_str(args, "state"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "update" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let invitation_id = match required_str(args, "invitation_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::invitation::update_invitation(
                &client,
                entity_type,
                entity_id,
                invitation_id,
                &api::invitation::UpdateInvitationParams {
                    new_state: optional_str(args, "state"),
                    permissions: optional_str(args, "role"),
                    notify_options: optional_str(args, "notify_options"),
                    expires: optional_str(args, "expires"),
                },
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "accept" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let invitation_id = match required_str(args, "invitation_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::invitation::update_invitation(
                &client,
                entity_type,
                entity_id,
                invitation_id,
                &api::invitation::UpdateInvitationParams {
                    new_state: Some("accepted"),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "decline" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let invitation_id = match required_str(args, "invitation_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::invitation::update_invitation(
                &client,
                entity_type,
                entity_id,
                invitation_id,
                &api::invitation::UpdateInvitationParams {
                    new_state: Some("declined"),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "delete" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let invitation_id = match required_str(args, "invitation_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::invitation::delete_invitation(&client, entity_type, entity_id, invitation_id)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown invitation action: {action}"))),
    }
}

/// Strip the redundant standalone `downloadToken` from a preview response BEFORE
/// it is returned to the MCP caller, and attach a secret-bearing note.
///
/// The `get` / `thumbnail` preauthorize response carries
/// `{result, downloadToken, path, primaryFilename}` where the tokenized `path`
/// (`.../preview/.../read/<token>/file/...`) ALREADY embeds the token. Surfacing
/// the standalone `downloadToken` is therefore redundant secret exposure, so it is
/// removed; the tokenized `path` remains the deliverable the agent fetches. Mirrors
/// the `download.file-url` treatment of its redundant `download_token`.
fn sanitize_preview_response(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("downloadToken");
        obj.insert(
            "note".to_owned(),
            Value::String(
                "path is secret-bearing (carries a short-lived embedded read token). \
                 Do not log or share it."
                    .to_owned(),
            ),
        );
    }
}

/// Preview tool handler.
async fn handle_preview(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "get" | "thumbnail" => {
            let ctx_type = match required_str(args, "context_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let ctx_id = match required_str(args, "context_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // `thumbnail` hardcodes its (always-valid) type; `get` REQUIRES a
            // `preview_type` from the valid set — matching the CLI, which makes
            // `preview_type` mandatory for `get`. An absent or out-of-set value
            // is rejected here rather than 400ing at the server.
            let preview_type = if action == "thumbnail" {
                "thumbnail"
            } else {
                match optional_str(args, "preview_type") {
                    None => {
                        return Ok(error_text(
                            "Missing required parameter: preview_type \
                             (one of: bin, thumbnail, image, hlsstream, pdf, spreadsheet, audio, mp4)",
                        ));
                    }
                    Some(v) if !PREVIEW_TYPES.contains(&v) => {
                        return Ok(error_text(&format!(
                            "Invalid preview_type '{v}' \
                             (one of: bin, thumbnail, image, hlsstream, pdf, spreadsheet, audio, mp4)"
                        )));
                    }
                    Some(v) => v,
                }
            };
            match api::preview::get_preview_url(&client, ctx_type, ctx_id, node_id, preview_type)
                .await
            {
                Ok(mut v) => {
                    sanitize_preview_response(&mut v);
                    Ok(success_json(&v))
                }
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "transform" => handle_preview_transform(&client, args).await,
        _ => Ok(error_text(&format!("Unknown preview action: {action}"))),
    }
}

/// `preview` tool `transform` action handler.
///
/// Split out of [`handle_preview`] to keep that dispatcher under the line
/// limit and to give the strict integer parsing a focused home.
async fn handle_preview_transform(
    client: &fastio_cli::client::ApiClient,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let ctx_type = match required_str(args, "context_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // The transform name defaults to `image` (the only valid value);
    // the builder validates it.
    //
    // DELIBERATELY LENIENT — the one `optional_str(..).unwrap_or(..)` left after
    // the 2026-08-25 strict sweep. That pattern is normally a silent-wrong-request
    // bug (a discarded argument replaced by a DIFFERENT valid request), but here
    // the default is the ONLY valid value, so a mistype cannot select a request
    // the caller did not want. Do not "fix" this by mechanical grep.
    let transform =
        optional_str(args, "transform_name").unwrap_or(api::preview::IMAGE_TRANSFORM_NAME);
    // Parse the integer params strictly: a PRESENT-but-non-integer value
    // (e.g. `rotate: "90deg"`) must surface a clear error rather than be
    // silently dropped. Range/set validation still happens later in
    // `validate_transform_params`. The `?`-style early-return uses a
    // `match` because the handler returns `Result<_, McpError>`, not the
    // helper's `Result<_, CallToolResult>`.
    macro_rules! parse_u32_strict {
        ($key:literal) => {
            match optional_u32_strict(args, $key) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            }
        };
    }
    let width = parse_u32_strict!("width");
    let height = parse_u32_strict!("height");
    let crop_width = parse_u32_strict!("crop_width");
    let crop_height = parse_u32_strict!("crop_height");
    let crop_x = parse_u32_strict!("crop_x");
    let crop_y = parse_u32_strict!("crop_y");
    let rotate = parse_u32_strict!("rotate");
    match api::preview::get_transform_url(
        client,
        &api::preview::TransformUrlParams {
            context_type: ctx_type,
            context_id: ctx_id,
            node_id,
            transform_name: transform,
            width,
            height,
            output_format: optional_str(args, "output_format"),
            size: optional_str(args, "size"),
            crop_width,
            crop_height,
            crop_x,
            crop_y,
            rotate,
        },
    )
    .await
    {
        // The transform response is `{transform_name, token, read_url}`:
        // `read_url` is the `/read/` URL carrying the token AND the image
        // params (so they actually apply when fetched), and `token` is
        // the bare download token. Both are secret-bearing deliverables —
        // kept intact (redaction is handled by SECRET_LOG_KEYS on the
        // trace side), not stripped.
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Asset tool handler.
async fn handle_asset(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "list" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::asset::list_assets(&client, entity_type, entity_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "types" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::asset::list_asset_types(&client, entity_type).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "remove" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let asset_type = match required_str(args, "asset_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::asset::delete_asset(&client, entity_type, entity_id, asset_type).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown asset action: {action}"))),
    }
}

/// Read an optional JSON-object argument that may arrive either as a JSON object
/// directly (the common MCP-client shape) or as a serialized JSON-object string.
/// A missing/null key is `None`; any value that is not (or does not parse to) a
/// JSON object is rejected with a clear error rather than silently dropped.
fn json_object_arg(args: &Map<String, Value>, key: &str) -> Result<Option<Value>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(Some(Value::Object(map.clone()))),
        Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
            Ok(v) if v.is_object() => Ok(Some(v)),
            _ => Err(error_text(&format!(
                "{key} must be a JSON object (e.g. {{\"key\":\"value\"}})"
            ))),
        },
        Some(_) => Err(error_text(&format!(
            "{key} must be a JSON object (e.g. {{\"key\":\"value\"}})"
        ))),
    }
}

/// Shared poll-tick error classification for the KEPT MCP wait loops
/// (Ripley `ask` polling and metadata `extract` polling).
///
/// How an MCP poll loop should react to an error from one poll tick.
///
/// Replaces the old `Err(_) => {}` (which silently looped to timeout on a
/// 404/403/500). The 401 re-auth short-circuit is handled by the caller before
/// this is reached.
enum WfPollAction {
    /// Server asked us to wait this many seconds (0 = no explicit interval).
    RateLimited { retry_after_secs: u64 },
    /// A transient failure worth another poll on the regular cadence.
    RetryTransient,
    /// A persistent error rendered for return (loop must stop and surface it).
    Fatal(CallToolResult),
}

/// Classify a poll-tick [`CliError`] for an MCP wait/export loop.
///
/// Mirrors the CLI `classify_poll_error`: rate limits sleep their advertised
/// interval; all 5xx (`500..=599`) / 408 / transport / I/O are transient; other
/// 4xx, parse, and config are fatal and surfaced via [`cli_err_to_result`].
fn classify_wf_poll_error(err: &fastio_cli::error::CliError) -> WfPollAction {
    use fastio_cli::error::CliError;
    match err {
        CliError::RateLimit { retry_after_secs } => WfPollAction::RateLimited {
            retry_after_secs: *retry_after_secs,
        },
        CliError::Api(e) => match e.http_status {
            429 | 408 => WfPollAction::RateLimited {
                retry_after_secs: 0,
            },
            // All server errors are transient (matches the CLI classifier): a
            // 500 during a long poll is a momentary backend blip, not permanent.
            500..=599 => WfPollAction::RetryTransient,
            _ => WfPollAction::Fatal(cli_err_to_result(err)),
        },
        CliError::Http(_) | CliError::Io(_) => WfPollAction::RetryTransient,
        // Parse / config / auth(other) — and, conservatively, any future
        // non-exhaustive variant — are surfaced rather than looped.
        _ => WfPollAction::Fatal(cli_err_to_result(err)),
    }
}

/// Read a list-of-ids argument that may arrive either as a native JSON array of
/// strings or as a comma-separated string (the established CSV form for id-list
/// params). Returns the collected ids — the caller enforces non-empty and any cap.
///
/// The two forms are treated differently on purpose, because these feed mutating
/// / destructive callers (attach, bulk-delete): a native JSON array is machine
/// built, so a blank or non-string entry signals a generation bug and is
/// **rejected** (fail closed); a comma-separated string is loose human/CLI input,
/// so stray/trailing commas are tolerated by trimming and dropping blank tokens.
fn string_list_arg(args: &Map<String, Value>, key: &str) -> Result<Vec<String>, CallToolResult> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(s
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect()),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                match v.as_str() {
                    Some(t) if !t.trim().is_empty() => out.push(t.trim().to_owned()),
                    Some(_) => {
                        return Err(error_text(&format!(
                            "{key} array entries must be non-empty strings"
                        )));
                    }
                    None => {
                        return Err(error_text(&format!("{key} array entries must be strings")));
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(error_text(&format!(
            "{key} must be a JSON array of strings or a comma-separated string"
        ))),
    }
}

/// Apps tool handler.
///
/// Only `list` exists. `details` / `launch` / `get-tool-apps` were removed in
/// 2026.8 — they called a `/apps/` prefix that has never existed on the server,
/// so every call returned `9992`, and an exhaustive sweep of the deployed route
/// tree found no route for those operations to be pointed at.
async fn handle_apps(
    state: &McpState,
    action: &str,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    match action {
        // `/user/apps/` is the caller's OWN installed-app list and rejects an
        // anonymous request (10011), so this requires auth. The removed
        // `/apps/list/` was called unauthenticated — it never reached a handler
        // that could ask for a token.
        "list" => {
            if let Err(e) = require_auth(state).await {
                return Ok(e);
            }
            let client = state.client().read().await;
            match api::apps::list_installed_apps(&client).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown apps action: {action}"))),
    }
}

/// Import tool handler.
async fn handle_import(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "list-providers" => handle_import_list_providers(state, args).await,
        "list-identities" => handle_import_list_identities(state, args).await,
        "provision-identity" => handle_import_provision_identity(state, args).await,
        "identity-details" => handle_import_identity_details(state, args).await,
        "revoke-identity" => handle_import_revoke_identity(state, args).await,
        "list-sources" => handle_import_list_sources(state, args).await,
        "list-drives" => handle_import_list_drives(state, args).await,
        "refresh-drives" => handle_import_refresh_drives(state, args).await,
        "discover" => handle_import_discover(state, args).await,
        "create-source" => handle_import_create_source(state, args).await,
        "source-details" => handle_import_source_details(state, args).await,
        "update-source" => handle_import_update_source(state, args).await,
        "delete-source" => handle_import_delete_source(state, args).await,
        "disconnect" => handle_import_disconnect(state, args).await,
        "refresh" => handle_import_refresh(state, args).await,
        "list-jobs" => handle_import_list_jobs(state, args).await,
        "job-details" => handle_import_job_details(state, args).await,
        "cancel-job" => handle_import_cancel_job(state, args).await,
        "list-writebacks" => handle_import_list_writebacks(state, args).await,
        "writeback-details" => handle_import_writeback_details(state, args).await,
        "push-writeback" => handle_import_push_writeback(state, args).await,
        "retry-writeback" => handle_import_retry_writeback(state, args).await,
        "resolve-conflict" => handle_import_resolve_conflict(state, args).await,
        "cancel-writeback" => handle_import_cancel_writeback(state, args).await,
        _ => Ok(error_text(&format!("Unknown import action: {action}"))),
    }
}

async fn handle_import_list_providers(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::list_providers(&client, ws_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_list_identities(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::list_identities(&client, ws_id, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_provision_identity(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let provider = match required_str(args, "provider") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let account_type = optional_str(args, "account_type");
    match api::import::provision_identity(&client, ws_id, provider, account_type).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_identity_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let iid = match required_str(args, "identity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::identity_details(&client, ws_id, iid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_revoke_identity(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let iid = match required_str(args, "identity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::revoke_identity(&client, ws_id, iid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_list_sources(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::list_sources(&client, ws_id, optional_str(args, "status"), None, None).await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_discover(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let iid = match required_str(args, "identity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::discover(
        &client,
        ws_id,
        iid,
        optional_str(args, "drive_id"),
        optional_str(args, "remote_path"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_list_drives(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let iid = match required_str(args, "identity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::list_drives(
        &client,
        ws_id,
        iid,
        optional_str(args, "site_path"),
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_refresh_drives(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let iid = match required_str(args, "identity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::refresh_drives(&client, ws_id, iid, optional_str(args, "site_path")).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_create_source(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let iid = match required_str(args, "identity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let rpath = match required_str(args, "remote_path") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::create_source(
        &client,
        &api::import::CreateSourceParams {
            workspace_id: ws_id,
            identity_id: iid,
            remote_path: rpath,
            remote_name: optional_str(args, "remote_name"),
            sync_interval: optional_u32(args, "sync_interval"),
            access_mode: optional_str(args, "access_mode"),
            drive_id: optional_str(args, "drive_id"),
            destination_node_id: optional_str(args, "destination_node_id"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_source_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::source_details(&client, sid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_update_source(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Both parsed STRICTLY: a present-but-non-string `status` (or non-integer
    // `sync_interval`) read as absent would send a DIFFERENT request than the
    // caller wrote — no action inserted, siblings still applied — which is the
    // silent no-op this command's guard exists to close.
    let status = match optional_str_strict(args, "status") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let sync_interval = match optional_u32_strict(args, "sync_interval") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::update_source(
        &client,
        sid,
        sync_interval,
        status,
        optional_str(args, "remote_name"),
        optional_str(args, "access_mode"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_delete_source(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::delete_source(&client, sid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_disconnect(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let act = match required_str(args, "disconnect_action") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::disconnect_source(&client, sid, act).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_refresh(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::refresh_source(&client, sid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_list_jobs(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::list_jobs(&client, sid, optional_u32(args, "limit"), None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_job_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let jid = match required_str(args, "job_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::job_details(&client, sid, jid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_cancel_job(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let jid = match required_str(args, "job_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::cancel_job(&client, sid, jid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_list_writebacks(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::list_writebacks(
        &client,
        sid,
        optional_str(args, "status"),
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_writeback_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let wid = match required_str(args, "writeback_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::writeback_details(&client, sid, wid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_push_writeback(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let nid = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::push_writeback(&client, sid, nid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_retry_writeback(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let wid = match required_str(args, "writeback_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::retry_writeback(&client, sid, wid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_resolve_conflict(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let wid = match required_str(args, "writeback_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let res = match required_str(args, "conflict_resolution") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::resolve_conflict(&client, sid, wid, res).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_import_cancel_writeback(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let sid = match required_str(args, "source_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let wid = match required_str(args, "writeback_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::import::cancel_writeback(&client, sid, wid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Lock tool handler.
async fn handle_lock(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    let ctx_type = match optional_str_strict(args, "context_type") {
        Ok(v) => v.unwrap_or("workspace"),
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match action {
        "acquire" => match api::locking::lock_acquire(
            &client,
            ctx_type,
            ctx_id,
            node_id,
            optional_u32(args, "duration"),
            optional_str(args, "client_info"),
        )
        .await
        {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "status" => match api::locking::lock_status(&client, ctx_type, ctx_id, node_id).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "release" => {
            let lock_token = match required_str(args, "lock_token") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::locking::lock_release(&client, ctx_type, ctx_id, node_id, lock_token).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "heartbeat" => {
            let lock_token = match required_str(args, "lock_token") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::locking::lock_heartbeat(&client, ctx_type, ctx_id, node_id, lock_token).await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown lock action: {action}"))),
    }
}

/// Metadata tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_metadata(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "eligible" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Cursor-paginated, NOT offset-paginated. `limit`/`offset` used to
            // be forwarded here and the server accepted and ignored them, so a
            // caller asking for one row silently received a full page that
            // looked like a correct answer.
            // STRICT extraction, not the lax `optional_str`/`optional_u32`.
            // Those read any wrong-typed value as ABSENT, and every one of
            // these parameters selects WHICH ROWS come back: a JSON `cursor: 1`
            // would silently restart at page 1, and a non-string `mimetype`
            // would silently drop the filter — a plausible full page, 200, no
            // warning. That is the identical shape to the `limit`/`offset`
            // defect this action was just fixed for, so it must not be
            // reintroduced through argument extraction.
            let page_size = match optional_u32_strict(args, "page_size") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let cursor = match optional_str_strict(args, "cursor") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let mimetype = match optional_str_strict(args, "mimetype") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let extension = match optional_str_strict(args, "extension") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = api::metadata::EligibleParams::new()
                .page_size(page_size)
                .cursor(cursor)
                .mimetype(mimetype)
                .extension(extension);
            let ignored_offset_args = legacy_offset_args_supplied(args);
            match api::metadata::list_eligible(&client, workspace_id, &params).await {
                Ok(v) => {
                    let mut v = v;
                    if ignored_offset_args {
                        attach_warning(
                            &mut v,
                            Some(
                                "`limit`/`offset` do nothing on metadata eligible and were \
                                 IGNORED — this endpoint is cursor-paginated. This is a full \
                                 page, not the slice you asked for. Use `page_size` \
                                 (server-snapped to 25/100/250) and `cursor`.",
                            ),
                        );
                    }
                    Ok(success_json(&v))
                }
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "extract" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            match api::metadata::extract_node_metadata(&client, workspace_id, node_id, fields).await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "extract-and-wait" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            let poll_interval = optional_u64(args, "poll_interval");
            Ok(
                metadata_extract_and_wait(&client, workspace_id, node_id, fields, poll_interval)
                    .await,
            )
        }
        "search" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let query = match required_str(args, "query") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::metadata::search_metadata(
                &client,
                workspace_id,
                query,
                optional_u32(args, "limit"),
                optional_u32(args, "offset"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown metadata action: {action}"))),
    }
}

/// Default seconds between metadata-extract job-status polls (MCP).
const METADATA_EXTRACT_POLL_DEFAULT_SECS: u64 = 3;
/// Lower bound on the metadata-extract poll interval (MCP).
const METADATA_EXTRACT_POLL_MIN_SECS: u64 = 1;
/// Upper bound on the metadata-extract poll interval (MCP).
const METADATA_EXTRACT_POLL_MAX_SECS: u64 = 60;
/// Hard ceiling on the MCP `metadata extract-and-wait` poll loop. Sized
/// well under the ~1-hour JWT lifetime so a stuck job surfaces a clear
/// timeout rather than hanging the MCP session.
const METADATA_EXTRACT_WAIT_MAX_SECS: u64 = 600;

/// Compound: enqueue a single-file metadata extraction, then poll the
/// workspace jobs-status endpoint until the job reaches a terminal state.
///
/// This is the offload-friendly MCP expression of "extract and tell me the
/// result" — the agent makes one call instead of enqueue-then-poll. Spends
/// AI credits. The loop is bounded by [`METADATA_EXTRACT_WAIT_MAX_SECS`] so
/// it cannot hang past the JWT lifetime; a 401 short-circuits to a clear
/// re-auth error. When no job was enqueued (no `job_id`), the response body is
/// returned unchanged — there is nothing to wait for. Stated without naming a
/// cause: an `already_extracted` reply is a `200` with no job, so it is neither
/// necessarily an "empty effective scope" nor necessarily a `202`.
async fn metadata_extract_and_wait(
    client: &fastio_cli::client::ApiClient,
    workspace_id: &str,
    node_id: &str,
    fields: Option<&str>,
    poll_interval: Option<u64>,
) -> CallToolResult {
    use fastio_cli::api::metadata::ExtractJobState;

    let enqueue =
        match api::metadata::extract_node_metadata(client, workspace_id, node_id, fields).await {
            Ok(v) => v,
            Err(e) => return cli_err_to_result(&e),
        };

    // Pull the job_id out of the (possibly enveloped) extract response body.
    let payload = enqueue.get("response").unwrap_or(&enqueue);
    let job_id = payload
        .get("job_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let Some(job_id) = job_id else {
        // No job enqueued — return the response body unchanged.
        return success_json(&enqueue);
    };

    let interval = poll_interval
        .unwrap_or(METADATA_EXTRACT_POLL_DEFAULT_SECS)
        .clamp(
            METADATA_EXTRACT_POLL_MIN_SECS,
            METADATA_EXTRACT_POLL_MAX_SECS,
        );

    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(METADATA_EXTRACT_WAIT_MAX_SECS);

    loop {
        match api::workspace::jobs_status(client, workspace_id).await {
            Ok(status) => {
                match api::metadata::classify_single_extract_job(&status, node_id, Some(&job_id)) {
                    ExtractJobState::Completed => {
                        let result = serde_json::json!({
                            "result": "yes",
                            "job_id": job_id,
                            "node_id": node_id,
                            "status": "completed",
                            "message": "Extraction completed. Read values via the metadata-details action.",
                        });
                        return success_json(&result);
                    }
                    // `errored` IS NOT PROOF OF FINALITY — see the CLI twin in
                    // `commands/metadata.rs`. A retryable failure is published
                    // as `errored` for the whole backoff while the queue
                    // retries (reported by the platform 2026-08-27, live on
                    // prod today), so "failed" is a verdict the status does
                    // not support.
                    //
                    // This matters MORE on the agent surface than on the CLI:
                    // a human sees an error and re-checks; an agent told a job
                    // "failed" reports failure upstream or abandons the task,
                    // and nothing later corrects it.
                    ExtractJobState::Errored(msg) => {
                        let detail = msg.unwrap_or_else(|| "no error message provided".to_owned());
                        return error_text(&format!(
                            "extraction job {job_id} last reported `errored`: {detail}. This is the \
                             job's most recent status, not proof it will not complete — re-read the \
                             values via the metadata-details action before reporting it as failed."
                        ));
                    }
                    // `NotFound` is NOT treated as success. Terminal entries
                    // only age out after ~1h, well beyond this bounded
                    // `METADATA_EXTRACT_WAIT_MAX_SECS` window, so a missing
                    // entry within the window means the job is not yet visible
                    // rather than aged-out-after-completion. Keep polling until
                    // an EXPLICIT terminal state is observed or the deadline is
                    // reached (which returns an indeterminate timeout, not
                    // success). `Pending` and the `#[non_exhaustive]` catch-all
                    // also keep us polling.
                    _ => {}
                }
            }
            Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 401 => {
                return error_text(&format!(
                    "authentication expired while waiting for extraction job {job_id}. The job \
                     may still complete server-side; re-authenticate and read values via the \
                     metadata-details action."
                ));
            }
            // Classify rather than swallow: a transient blip retries on the next
            // tick; a persistent 4xx (403/404/402/parse) is surfaced instead of
            // looping silently to the deadline.
            Err(other) => match classify_wf_poll_error(&other) {
                WfPollAction::RateLimited { retry_after_secs } => {
                    if retry_after_secs > 0 {
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        tokio::time::sleep(
                            remaining.min(std::time::Duration::from_secs(retry_after_secs)),
                        )
                        .await;
                    }
                }
                WfPollAction::RetryTransient => {}
                WfPollAction::Fatal(result) => return result,
            },
        }

        if tokio::time::Instant::now() >= deadline {
            return error_text(&format!(
                "timed out after ~{METADATA_EXTRACT_WAIT_MAX_SECS}s waiting for extraction job \
                 {job_id}. The job may still complete server-side; poll the workspace \
                 jobs-status action or read values via metadata-details."
            ));
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let sleep = remaining.min(std::time::Duration::from_secs(interval));
        tokio::time::sleep(sleep).await;

        if tokio::time::Instant::now() >= deadline {
            return error_text(&format!(
                "timed out after ~{METADATA_EXTRACT_WAIT_MAX_SECS}s waiting for extraction job \
                 {job_id}. The job may still complete server-side; poll the workspace \
                 jobs-status action or read values via metadata-details."
            ));
        }
    }
}

// ─── Sign (E-Signature) ─────────────────────────────────────────────────────

/// Discriminates an async-artifact FETCH (signed PDF / audit certificate) from
/// every other signing call, so a `404` (live not-ready codes
/// `1609`/`128301`/`146422`) is only reframed as "not ready yet" where that is
/// genuinely correct. For [`SignOp::General`] —
/// CRUD / list / get / update / source-download — a `404` is a genuine
/// not-found. Mirrors the CLI's `SignOp`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SignOp {
    /// A CRUD / list / get / update call, or a source-document download.
    General,
    /// A signed-PDF fetch — available once the envelope **completes**, so the
    /// not-ready guidance steers to "poll until it completes".
    SignedFetch,
    /// An audit-certificate fetch — available once the envelope reaches **any
    /// terminal state** (completed/declined/voided/expired/failed), so the
    /// not-ready guidance steers to "poll until it reaches a terminal state".
    AuditFetch,
}

impl SignOp {
    /// `true` for an async signed/audit artifact fetch (where a
    /// `404`/`1609`/`128301`/`146422` means "not ready yet" rather than a
    /// genuine not-found).
    fn is_artifact_fetch(self) -> bool {
        matches!(self, SignOp::SignedFetch | SignOp::AuditFetch)
    }
}

/// Map a signing API error to an MCP result.
///
/// Mirrors the CLI's `map_signing_error` — signing-specific wording lives HERE
/// (and there), never in the global `error.rs` hints. Keyed on the actual
/// `error.code`, NOT a bare HTTP status:
///
/// - `404`/`1609`/`128301`/`146422` is reframed as "not ready yet" ONLY for a
///   signed/audit artifact fetch ([`SignOp::SignedFetch`] / [`SignOp::AuditFetch`]).
///   `146422` is the live signed-PDF not-ready code, `128301` the
///   audit-certificate one; both are also HTTP 404, but `146422` is matched by
///   its OWN predicate arm so a non-404 `146422` still reframes. The poll target
///   is artifact-appropriate: the signed PDF is available once the envelope
///   **completes**, the audit certificate once the envelope reaches **any
///   terminal state** (completed/declined/voided/expired/failed). For
///   [`SignOp::General`] a
///   `404`/`1609` is a genuine not-found and is NOT reframed.
/// - `10545` (workspace membership) / `115069` (envelope access) override the
///   generic 401 hint; `1680` (workspace permission, kept generic), `1670`
///   (plan signing capability), `9992` (director route-not-found), `1685`
///   (credits), `1660` (terminal) each get signing-scoped wording.
/// - Unmatched codes append the shared `err.suggestion()`.
fn sign_err_to_result(err: &fastio_cli::error::CliError, ctx: &str, op: SignOp) -> CallToolResult {
    if let fastio_cli::error::CliError::Api(api) = err {
        // Async-artifact "not ready yet" — only for a signed/audit fetch. Code
        // 9992 is EXCLUDED because "poll and retry" is the one framing that is
        // actively harmful here: 9992 is the API director's path walk ending
        // with no `index.php` matched, so no handler ever ran and no amount of
        // polling makes a route appear — an agent would loop forever. The
        // code-specific arm below names that mechanism without diagnosing
        // which side produced it.
        // An UNREADABLE error body is not evidence the artifact is pending.
        // Mirrors the CLI guard in `map_signing_error`: the two halves have to
        // move together, so a change to one belongs in the other.
        //
        // `download_file_stream` (which MCP `sign_download` also calls) bounds
        // its error-body read and marks an unreadable body with
        // `ERR_BODY_UNAVAILABLE` and `code: 0`. Zero sails past `!= 9992`, so a
        // slow-but-valid `9992` ("route not found" — polling can NEVER resolve
        // it) would be reported to an AGENT as "not ready yet, poll", which is
        // worse here than on the CLI: an agent loops without a human noticing.
        let body_unavailable =
            api.error_code.as_deref() == Some(fastio_cli::error::ERR_BODY_UNAVAILABLE);
        if op.is_artifact_fetch()
            && !body_unavailable
            && api.code != 9992
            && (api.http_status == 404
                || api.code == 1609
                || api.code == 128_301
                || api.code == 146_422)
        {
            // Artifact-appropriate poll target: the signed PDF is available once
            // the envelope COMPLETES; the audit certificate once the envelope
            // reaches any TERMINAL state (completed/declined/voided/expired/failed).
            let stage = if op == SignOp::AuditFetch {
                "the audit certificate is not generated until the envelope reaches a terminal \
                 state; poll envelope-get and retry once it reaches any terminal state (e.g. \
                 completed, declined, voided, expired, or failed)."
            } else {
                "the signed document is not generated until the envelope completes; poll \
                 envelope-get and retry once it completes."
            };
            return error_text(&format!("{ctx}: not ready yet — {stage} ({err})"));
        }
        let note = match api.code {
            10545 => Some(
                "you are not a member of this workspace (10545). Signing requires workspace \
                 membership — ask a workspace admin to add you, or check workspace_id.",
            ),
            115_069 => Some(
                "you do not have access to this envelope (115069). Confirm the envelope id and \
                 your permission on its workspace.",
            ),
            1680 => Some(
                "your workspace permission is insufficient for this signing action (1680). A \
                 higher workspace role may be required.",
            ),
            1670 => Some(
                "signing is not enabled for this workspace's organization (1670). Check the org \
                 plan capability: `fastio org info <org-id>` -> capabilities.signing.",
            ),
            9992 => Some(
                "the server's API director finished resolving this request path without matching \
                 an endpoint (9992). No handler ran, so polling will not help and this says \
                 nothing about the envelope's state. The response cannot say which side is at \
                 fault — a path this build calls that the deployment does not expose, and a \
                 deployment missing that endpoint, look identical from here. Report it with the \
                 exact command rather than re-checking ids on a guess.",
            ),
            1685 => Some("insufficient signing credits for this operation (1685)."),
            1660 => Some("the envelope is already terminal and cannot be changed (1660)."),
            _ => None,
        };
        if let Some(note) = note {
            return error_text(&format!("{ctx}: {note} ({err})"));
        }
        // Defer to the shared suggestion for everything else.
        if let Some(hint) = err.suggestion() {
            return error_text(&format!("{ctx}: {hint} ({err})"));
        }
    }
    error_text(&format!("{ctx}: {err}"))
}

/// Resolve an optional JSON-string array argument into typed builders via a
/// per-element mapper. Returns `Err` (an MCP result) on a malformed value.
fn sign_json_array(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<Value>>, CallToolResult> {
    match optional_str(args, key) {
        None => Ok(None),
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Array(items)) => Ok(Some(items)),
            Ok(_) => Err(error_text(&format!("{key} must be a JSON array"))),
            Err(e) => Err(error_text(&format!("{key} is not valid JSON: {e}"))),
        },
    }
}

/// Resolve an optional JSON-string OBJECT argument into a [`Value`].
///
/// `body_json` / `policy_json` are documented as OBJECTS;
/// a non-object (array / scalar / null) is rejected with a clear error rather
/// than passed through, mirroring the CLI's `resolve_opt_json_object`.
fn sign_json_object(args: &Map<String, Value>, key: &str) -> Result<Option<Value>, CallToolResult> {
    match optional_str(args, key) {
        None => Ok(None),
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(v @ Value::Object(_)) => Ok(Some(v)),
            Ok(_) => Err(error_text(&format!("{key} must be a JSON object"))),
            Err(e) => Err(error_text(&format!("{key} is not valid JSON: {e}"))),
        },
    }
}

/// Read an optional string field from a JSON object.
///
/// A missing key is `None`; a PRESENT key that is not a JSON string is an error
/// (an MCP result) rather than a silent drop, so a mistyped field is rejected
/// instead of vanishing from the request.
fn sign_str_field(v: &Value, key: &str) -> Result<Option<String>, CallToolResult> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(error_text(&format!("field '{key}' must be a JSON string"))),
    }
}

/// Read an optional `u64` field (number or string-encoded) from a JSON object.
///
/// A missing key is `None`; a PRESENT key that is neither a `u64` nor a string
/// that parses as one is an error rather than a silent drop.
fn sign_u64_field(v: &Value, key: &str) -> Result<Option<u64>, CallToolResult> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(f) => f
            .as_u64()
            .or_else(|| f.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or_else(|| error_text(&format!("field '{key}' must be a non-negative integer"))),
    }
}

/// Read an optional `f64` field (number or string-encoded) from a JSON object.
///
/// A missing key is `None`; a PRESENT key that is neither an `f64` nor a string
/// that parses as one is an error rather than a silent drop, so a malformed
/// coordinate (e.g. `"x_norm":"abc"`) is rejected instead of placing a field at
/// a bogus position.
fn sign_f64_field(v: &Value, key: &str) -> Result<Option<f64>, CallToolResult> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(f) => f
            .as_f64()
            .or_else(|| f.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or_else(|| error_text(&format!("field '{key}' must be a number"))),
    }
}

/// Read an optional boolean field from a JSON object.
///
/// A missing key is `None`; a PRESENT key that is not a JSON boolean is an error
/// rather than a silent drop.
fn sign_bool_field(v: &Value, key: &str) -> Result<Option<bool>, CallToolResult> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(error_text(&format!("field '{key}' must be a JSON boolean"))),
    }
}

/// Ensure a JSON array element is an OBJECT, naming the array and index on
/// failure (an MCP result). Mirrors the CLI's `ensure_object`.
///
/// The per-field readers (`sign_str_field` etc.) use [`Value::get`], which
/// returns `None` for a non-object element — so a malformed array like
/// `"[1]"` or `"[null]"` would otherwise yield an all-`None` (EMPTY) spec that
/// silently passes the recipients-required guard and ships garbage. Rejecting
/// the non-object up front turns that into a clear error (e.g.
/// `recipients[0] must be a JSON object`). Field-level requirements are left to
/// the server.
fn sign_ensure_object(v: &Value, label: &str, index: usize) -> Result<(), CallToolResult> {
    if v.is_object() {
        Ok(())
    } else {
        Err(error_text(&format!(
            "{label}[{index}] must be a JSON object"
        )))
    }
}

/// Map a JSON documents array into [`signing::DocumentSpec`] builders. Returns
/// `Err` (an MCP result) on a non-object element or a present-but-mistyped field.
fn sign_parse_documents(
    items: &[Value],
) -> Result<Vec<fastio_cli::api::signing::DocumentSpec>, CallToolResult> {
    use fastio_cli::api::signing::DocumentSpec;
    items
        .iter()
        .enumerate()
        .map(|(i, v)| {
            sign_ensure_object(v, "documents", i)?;
            Ok(DocumentSpec::new()
                .id(sign_str_field(v, "id")?)
                .source_node_id(sign_str_field(v, "source_node_id")?)
                .source_version_id(sign_str_field(v, "source_version_id")?)
                .display_order(sign_u64_field(v, "display_order")?))
        })
        .collect()
}

/// Map a JSON recipients array into [`signing::RecipientSpec`] builders. Returns
/// `Err` (an MCP result) on a non-object element or a present-but-mistyped field.
fn sign_parse_recipients(
    items: &[Value],
) -> Result<Vec<fastio_cli::api::signing::RecipientSpec>, CallToolResult> {
    use fastio_cli::api::signing::RecipientSpec;
    items
        .iter()
        .enumerate()
        .map(|(i, v)| {
            sign_ensure_object(v, "recipients", i)?;
            Ok(RecipientSpec::new()
                .email(sign_str_field(v, "email")?)
                .display_name(sign_str_field(v, "display_name")?)
                .phone_e164(sign_str_field(v, "phone_e164")?)
                .role(sign_str_field(v, "role")?)
                .routing_order(sign_u64_field(v, "routing_order")?)
                .auth_method(sign_str_field(v, "auth_method")?))
        })
        .collect()
}

/// Map a JSON fields array into [`signing::FieldSpec`] builders (the `type` key
/// carries the field type; `value_json` is preserved as a JSON string). Returns
/// `Err` (an MCP result) on a non-object element or a present-but-mistyped field
/// (e.g. a non-numeric coordinate).
fn sign_parse_fields(
    items: &[Value],
) -> Result<Vec<fastio_cli::api::signing::FieldSpec>, CallToolResult> {
    use fastio_cli::api::signing::FieldSpec;
    items
        .iter()
        .enumerate()
        .map(|(i, v)| {
            sign_ensure_object(v, "fields", i)?;
            let value_json = v.get("value_json").and_then(|vj| match vj {
                Value::Null => None,
                Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            });
            let validation = match v.get("validation") {
                None | Some(Value::Null) => None,
                Some(Value::Object(o)) => Some(Value::Object(o.clone())),
                Some(_) => {
                    return Err(error_text(&format!(
                        "fields[{i}].validation must be a JSON object \
                         (e.g. {{\"min_length\": 2, \"max_length\": 64}})"
                    )));
                }
            };
            Ok(FieldSpec::new()
                .recipient_email(sign_str_field(v, "recipient_email")?)
                .document_index(sign_u64_field(v, "document_index")?)
                .page(sign_u64_field(v, "page")?)
                .bounding_box(
                    sign_f64_field(v, "x_norm")?,
                    sign_f64_field(v, "y_norm")?,
                    sign_f64_field(v, "w_norm")?,
                    sign_f64_field(v, "h_norm")?,
                )
                .field_type(sign_str_field(v, "type")?)
                .required(sign_bool_field(v, "required")?)
                .value_json(value_json)
                .validation(validation))
        })
        .collect()
}

/// The authoritative per-action describe payload for the `sign` tool. Names the
/// CLI-only outward/destructive/terminal actions under `cli_only_actions`.
// The length is a flat action-spec table, not branching logic.
#[allow(clippy::too_many_lines)]
fn sign_describe() -> CallToolResult {
    let actions: &[(&str, &[&str], &[&str], &str)] = &[
        ("describe", &[], &[], ""),
        (
            "envelope-create",
            &["workspace_id"],
            &[
                "name",
                "expires_at",
                "body_json",
                "policy_json",
                "documents_json",
                "recipients_json",
                "fields_json",
                "source_node_id",
                "source_version_id",
                "recipient_email",
                "recipient_name",
                "auth_method",
            ],
            "creates a DRAFT (reversible). Supply documents/recipients via the *_json arrays \
             (or body_json), or the simple source_node_id + recipient_email path. The response is \
             the FLAT envelope (no inlined documents/recipients/fields; provider is null until \
             sent) — call envelope-get to read the server-generated sub-resource ids.",
        ),
        (
            "envelope-update",
            &["workspace_id", "envelope_id", "recipients_json"],
            &[
                "name",
                "expires_at",
                "policy_json",
                "documents_json",
                "fields_json",
            ],
            "DRAFT-only (a non-draft returns 403). recipients_json is REQUIRED — an update is a \
             full recipient replacement (>=1). fields_json is a full replacement; documents_json \
             is a declarative replace (omit to leave unchanged). expires_at and policy_json are \
             DECLARATIVE: omitting one CLEARS it (resets to null) — re-send the current value to \
             keep it (name/documents/fields are preserved when omitted).",
        ),
        (
            "envelope-list",
            &["workspace_id"],
            &[
                "envelope_status",
                "created_after",
                "created_before",
                "limit",
                "offset",
            ],
            "offset-paginated, newest first. envelope_status is a single status or a CSV of \
             draft,sent,in_progress,completed,declined,expired,voided,failed.",
        ),
        (
            "envelope-get",
            &["workspace_id", "envelope_id"],
            &[],
            "inlines documents/recipients/fields",
        ),
        (
            "envelope-retry",
            &["workspace_id", "envelope_id"],
            &[],
            "re-drives a STUCK envelope through self-healing recovery (admin). Idempotent + \
             no-op-success — re-driving a non-stuck or already-terminal envelope succeeds without \
             side effects, and it notifies no one (so it is exposed over MCP, unlike send/void). A \
             permanent signing-pipeline failure cascades the envelope to the terminal Failed state.",
        ),
        (
            "envelope-my-sign-link",
            &["workspace_id", "envelope_id"],
            &[],
            "mints YOUR (the caller's) own signing link — the dashboard signature-card primary \
             action (envelope_id = the card's target.id). Reversible/idempotent and notifies no \
             one. Requires a WRITE-scope token (a read-only token is rejected with 10754). The \
             structured result is NOT just a URL: sign_url non-null = you can sign now; \
             is_terminal = completed/void/declined; reauth_required = re-authenticate first; else \
             you are blocked by routing order (see blocked_signers).",
        ),
        (
            "template-list",
            &["workspace_id"],
            &["offset", "limit"],
            "offset-paginated (offset default 0; limit default 50, max 200). Lists reusable \
             signing-template blueprints (template id sa…).",
        ),
        (
            "template-details",
            &["workspace_id", "template_id"],
            &[],
            "the full template blueprint (snapshot: recipient_slots / document_slots / fields / \
             policy).",
        ),
        (
            "template-instantiate",
            &["workspace_id", "template_id", "recipient_bindings"],
            &["documents", "envelope_name"],
            "creates a reversible DRAFT envelope from the template. recipient_bindings is REQUIRED \
             and MUST be a JSON OBJECT/map keyed by slot_key → {email, display_name?, auth_method?} \
             (an array is rejected). documents is an optional JSON array of {document_slot_index, \
             source_node_id, source_version_id?}; envelope_name overrides the created envelope's \
             name. The result is the new sign_envelope plus geometry_mismatch / geometry_details. \
             template-create / -update / -delete are CLI-binary-only.",
        ),
        (
            "document-download",
            &["workspace_id", "envelope_id", "document_id"],
            &["output_path"],
            "streams the SOURCE PDF to the local fs; returns a path + byte count. These are the \
             source/preview bytes — no separate preview action is needed over MCP.",
        ),
        (
            "signed-download",
            &["workspace_id", "envelope_id", "document_id"],
            &["output_path"],
            "streams the SIGNED PDF; 404/1609/146422 => not ready until the envelope completes",
        ),
        (
            "audit-download",
            &["workspace_id", "envelope_id"],
            &["output_path"],
            "streams the audit certificate (JSON); 404/1609/128301 => not ready until terminal",
        ),
    ];

    let mut action_map = serde_json::Map::new();
    for (name, required, optional, note) in actions {
        let mut spec = serde_json::Map::new();
        spec.insert(
            "required".to_owned(),
            Value::Array(
                required
                    .iter()
                    .map(|s| Value::String((*s).to_owned()))
                    .collect(),
            ),
        );
        spec.insert(
            "optional".to_owned(),
            Value::Array(
                optional
                    .iter()
                    .map(|s| Value::String((*s).to_owned()))
                    .collect(),
            ),
        );
        if !note.is_empty() {
            spec.insert("note".to_owned(), Value::String((*note).to_owned()));
        }
        action_map.insert((*name).to_owned(), Value::Object(spec));
    }

    let cli_only: Vec<Value> = [
        "envelope send (EMAILS REAL RECIPIENTS)",
        "envelope void (terminal; credits not refunded)",
        "template create (`fastio sign template create …`)",
        "template update (optimistic-CAS; `fastio sign template update …`)",
        "template delete (soft-delete; `fastio sign template delete …`)",
    ]
    .iter()
    .map(|s| Value::String((*s).to_owned()))
    .collect();

    let payload = serde_json::json!({
        "tool": "sign",
        "summary": "E-signature SignEnvelopes (workspace-parented) — READ, reversible-DRAFT-drive, \
                    and idempotent-RECOVERY (envelope-retry). Drafting, recovery, and downloads are \
                    exposed over MCP; the outward-facing send/void are CLI-binary-only. Envelopes \
                    are voided, not deleted (no delete action).",
        "common_required": ["workspace_id"],
        "destructive_actions": [],
        "side_effects": "envelope-create makes a DRAFT (reversible; no recipient is notified and \
                         no credits are reserved until a CLI `send`). envelope-retry re-drives a \
                         stuck envelope's signing pipeline — idempotent + no-op-success and \
                         notifies no one (a permanent failure cascades to the terminal Failed \
                         state). Downloads write files to the agent's local filesystem. The \
                         outward-facing send / void actions are CLI-binary-only (see \
                         cli_only_actions) and are NOT exposed over MCP. There is no delete — \
                         envelopes are voided.",
        "guidance": {
            "workspace": "workspace_id is the 19-digit owner workspace; every envelope is \
                          workspace-parented (the former org surface was removed).",
            "send_void": "To send (emails real recipients) or void an envelope, run the CLI: \
                          `fastio sign envelope send|void …`. These are NOT available over MCP by \
                          design. Envelopes are voided, not deleted — there is no delete action.",
            "templates": "Reusable signing-template blueprints (template id sa…): template-list, \
                          template-details, and template-instantiate (creates a reversible DRAFT \
                          envelope) are exposed over MCP. template-create / template-update \
                          (optimistic-CAS) / template-delete (soft-delete) are CLI-binary-only — \
                          run `fastio sign template create|update|delete …`.",
            "cli_only_actions": cli_only,
        },
        "actions": Value::Object(action_map),
    });
    success_json(&payload)
}

/// E-signature tool handler — READ + reversible-DRAFT-drive actions only. The
/// outward-facing / terminal actions (send / void) are CLI-binary-only and are
/// routed to a guidance message BEFORE auth/workspace extraction. Envelopes are
/// voided, not deleted; a `delete` request is reported as unsupported.
#[allow(clippy::too_many_lines)] // a flat dispatch over the envelope lifecycle surface
async fn handle_sign(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    use fastio_cli::api::signing;

    // `describe` needs no auth.
    if action == "describe" {
        return Ok(sign_describe());
    }
    // `delete` is not a real action — the signing API has no delete (envelopes
    // are voided). Report that distinctly, BEFORE auth/workspace extraction.
    if matches!(action, "envelope-delete" | "delete") {
        return Ok(error_text(
            "envelope delete is not supported by the signing API: envelopes are voided, not \
             deleted. To void a non-terminal envelope, run the CLI — `fastio sign envelope void \
             …`. Call action='describe' for the MCP action list.",
        ));
    }
    // The outward-facing / terminal actions are CLI-binary-only. Route them to
    // the CLI-only guidance FIRST — before auth and workspace extraction — so
    // e.g. `action=send` with no workspace_id returns the intended "this is
    // CLI-only" message rather than "Missing required parameter: workspace_id".
    if matches!(action, "envelope-send" | "send" | "envelope-void" | "void") {
        return Ok(error_text(
            "send and void are CLI-binary-only for the sign tool: send EMAILS REAL RECIPIENTS and \
             void is terminal. Run them via the CLI — `fastio sign envelope send|void …`. Call \
             action='describe' for the MCP action list.",
        ));
    }
    // The signing-template SETTERS (create / update / delete) are CLI-binary-only
    // — mirroring the send/void boundary. Route them to guidance FIRST (before
    // auth/workspace extraction) so the message is "this is CLI-only" rather than
    // "Missing required parameter". Only template-list / -details / -instantiate
    // (reads + reversible draft creation) are exposed over MCP.
    if matches!(
        action,
        "template-create" | "template-update" | "template-delete"
    ) {
        return Ok(error_text(
            "sign-template create / update / delete are CLI-binary-only: run them via the CLI — \
             `fastio sign template create|update|delete …`. Over MCP only template-list, \
             template-details, and template-instantiate (reads + reversible DRAFT creation) are \
             exposed. Call action='describe' for the MCP action list.",
        ));
    }
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;

    let workspace_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };

    match action {
        "envelope-create" => {
            let params = match sign_build_create_params(args) {
                Ok(p) => p,
                Err(e) => return Ok(e),
            };
            if let Err(e) = params.validate() {
                return Ok(error_text(&format!("invalid create request: {e}")));
            }
            match signing::create_envelope(&client, workspace_id, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to create sign envelope",
                    SignOp::General,
                )),
            }
        }
        "envelope-update" => {
            let envelope_id = match required_str(args, "envelope_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = match sign_build_update_params(args) {
                Ok(p) => p,
                Err(e) => return Ok(e),
            };
            if params.is_empty() {
                return Ok(error_text(
                    "no fields to update were supplied: supply at least recipients_json \
                     (recipients are a full replacement). name / documents_json / fields_json \
                     are kept when omitted; expires_at / policy_json are DECLARATIVE — omitting \
                     them clears the envelope's expiry / policy",
                ));
            }
            // An update is a FULL recipient replacement — recipients_json (>=1)
            // is required (mirrors the CLI).
            if params.recipients.as_deref().is_none_or(<[_]>::is_empty) {
                return Ok(error_text(
                    "envelope-update is a full recipient replacement: supply recipients_json with \
                     at least one recipient (an update always replaces the recipient roster)",
                ));
            }
            if let Err(e) = params.validate() {
                return Ok(error_text(&format!("invalid update request: {e}")));
            }
            match signing::update_envelope(&client, workspace_id, envelope_id, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to update sign envelope",
                    SignOp::General,
                )),
            }
        }
        "envelope-list" => {
            let params = signing::ListEnvelopesParams::new()
                .envelope_status(optional_str(args, "envelope_status").map(str::to_owned))
                .created_after(optional_str(args, "created_after").map(str::to_owned))
                .created_before(optional_str(args, "created_before").map(str::to_owned))
                .limit(optional_u32(args, "limit"))
                .offset(optional_u32(args, "offset"));
            match signing::list_envelopes(&client, workspace_id, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to list sign envelopes",
                    SignOp::General,
                )),
            }
        }
        "envelope-get" => {
            let envelope_id = match required_str(args, "envelope_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match signing::get_envelope(&client, workspace_id, envelope_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to get sign envelope",
                    SignOp::General,
                )),
            }
        }
        "envelope-retry" => {
            // `retry` re-drives a stuck envelope through self-healing recovery.
            // It is idempotent + no-op-success and notifies no one, so — unlike
            // the CLI-only send/void — it IS exposed over MCP (its safety profile
            // matches the read/draft-drive surface, not the gated outward/terminal
            // actions). A `404`/`1609` is a genuine not-found → `SignOp::General`.
            let envelope_id = match required_str(args, "envelope_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match signing::retry_envelope(&client, workspace_id, envelope_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to retry sign envelope",
                    SignOp::General,
                )),
            }
        }
        "envelope-my-sign-link" => {
            // Mints the CALLER's own signing link (the dashboard signature-card
            // primary action). Reversible/idempotent and notifies no one, so —
            // like envelope-retry — its safety profile matches the read/draft
            // surface, not the gated outward/terminal send/void. A read-only
            // scoped token is rejected server-side (10754); a `404`/`1609` is a
            // genuine not-found → `SignOp::General`.
            let envelope_id = match required_str(args, "envelope_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match signing::my_sign_link(&client, workspace_id, envelope_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to mint sign link",
                    SignOp::General,
                )),
            }
        }
        "template-list" => {
            match signing::list_sign_templates(
                &client,
                workspace_id,
                optional_u32(args, "offset"),
                optional_u32(args, "limit"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to list sign templates",
                    SignOp::General,
                )),
            }
        }
        "template-details" => {
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match signing::get_sign_template(&client, workspace_id, template_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to get sign template",
                    SignOp::General,
                )),
            }
        }
        "template-instantiate" => {
            // Creates a reversible DRAFT envelope from the template. Its safety
            // profile matches the read/draft surface (like envelope-create), so it
            // IS exposed over MCP; the template SETTERS (create/update/delete) are
            // CLI-only and handled earlier.
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // recipient_bindings is REQUIRED and MUST be a JSON object/map keyed
            // by slot_key (an array is rejected by `sign_json_object`).
            let bindings = match sign_json_object(args, "recipient_bindings") {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return Ok(error_text(
                        "template-instantiate requires recipient_bindings: a JSON object/map keyed \
                         by slot_key → {email, display_name?, auth_method?} (an array is rejected)",
                    ));
                }
                Err(e) => return Ok(e),
            };
            // documents is an optional JSON array of slot bindings.
            let documents = match sign_json_array(args, "documents") {
                Ok(opt) => opt.map(Value::Array),
                Err(e) => return Ok(e),
            };
            match signing::instantiate_sign_template(
                &client,
                workspace_id,
                template_id,
                &bindings,
                documents.as_ref(),
                optional_str(args, "envelope_name"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(sign_err_to_result(
                    &e,
                    "failed to instantiate sign template",
                    SignOp::General,
                )),
            }
        }
        "document-download" | "signed-download" | "audit-download" => {
            Ok(sign_download(&client, action, workspace_id, args).await)
        }
        // The outward-facing / terminal actions (send / void) and the
        // unsupported delete are handled earlier — before auth/workspace
        // extraction — so they are not matched here.
        _ => Ok(error_text(&format!(
            "Unknown or CLI-only sign action: {action}. The outward-facing send / void are \
             CLI-binary-only (`fastio sign envelope …`) and envelopes are voided, not deleted. \
             Call action='describe' for the MCP action list."
        ))),
    }
}

/// Build [`signing::CreateEnvelopeParams`] from the MCP create args (prefer
/// `body_json`, then the `*_json` arrays, then the simple single-signer path).
fn sign_build_create_params(
    args: &Map<String, Value>,
) -> Result<fastio_cli::api::signing::CreateEnvelopeParams, CallToolResult> {
    use fastio_cli::api::signing::{CreateEnvelopeParams, DocumentSpec, RecipientSpec};

    // body_json is the whole request when present. It is documented as an
    // OBJECT; `sign_json_object` rejects a non-object.
    if let Some(body) = sign_json_object(args, "body_json")? {
        let documents = match body.get("documents") {
            Some(Value::Array(items)) => sign_parse_documents(items)?,
            Some(_) => return Err(error_text("body_json 'documents' must be an array")),
            None => Vec::new(),
        };
        let recipients = match body.get("recipients") {
            Some(Value::Array(items)) => sign_parse_recipients(items)?,
            Some(_) => return Err(error_text("body_json 'recipients' must be an array")),
            None => Vec::new(),
        };
        let fields = match body.get("fields") {
            Some(Value::Array(items)) => sign_parse_fields(items)?,
            Some(_) => return Err(error_text("body_json 'fields' must be an array")),
            None => Vec::new(),
        };
        let policy_json = match body.get("policy_json") {
            None | Some(Value::Null) => None,
            Some(p @ Value::Object(_)) => Some(p.clone()),
            Some(_) => return Err(error_text("body_json 'policy_json' must be a JSON object")),
        };
        return Ok(CreateEnvelopeParams::new()
            .name(sign_str_field(&body, "name")?)
            .expires_at(sign_str_field(&body, "expires_at")?)
            .policy_json(policy_json)
            .documents(documents)
            .recipients(recipients)
            .fields(fields));
    }

    let documents = match sign_json_array(args, "documents_json")? {
        Some(items) => sign_parse_documents(&items)?,
        None => match optional_str(args, "source_node_id") {
            // STRICT: absence means "current version" of the source node, so a
            // present non-string read as absent would silently build the
            // envelope from the CURRENT document instead of the pinned one —
            // and an e-signature envelope is exactly where sending a different
            // document than the caller specified matters most.
            Some(node) => vec![
                DocumentSpec::new()
                    .source_node_id(Some(node.to_owned()))
                    .source_version_id(
                        optional_str_strict(args, "source_version_id")?.map(str::to_owned),
                    )
                    .display_order(Some(0)),
            ],
            None => {
                return Err(error_text(
                    "envelope-create needs documents: pass documents_json (or body_json), or the \
                     simple source_node_id",
                ));
            }
        },
    };

    let recipients = match sign_json_array(args, "recipients_json")? {
        Some(items) => sign_parse_recipients(&items)?,
        None => match optional_str(args, "recipient_email") {
            Some(email) => vec![
                RecipientSpec::new()
                    .email(Some(email.to_owned()))
                    .display_name(optional_str(args, "recipient_name").map(str::to_owned))
                    .role(Some("signer".to_owned()))
                    .routing_order(Some(1))
                    .auth_method(optional_str(args, "auth_method").map(str::to_owned)),
            ],
            None => {
                return Err(error_text(
                    "envelope-create needs recipients: pass recipients_json (or body_json), or the \
                     simple recipient_email",
                ));
            }
        },
    };

    let fields = match sign_json_array(args, "fields_json")? {
        Some(items) => sign_parse_fields(&items)?,
        None => Vec::new(),
    };

    Ok(CreateEnvelopeParams::new()
        .name(optional_str(args, "name").map(str::to_owned))
        .expires_at(optional_str(args, "expires_at").map(str::to_owned))
        .policy_json(sign_json_object(args, "policy_json")?)
        .documents(documents)
        .recipients(recipients)
        .fields(fields))
}

/// Build [`signing::UpdateEnvelopeParams`] from the MCP update args.
fn sign_build_update_params(
    args: &Map<String, Value>,
) -> Result<fastio_cli::api::signing::UpdateEnvelopeParams, CallToolResult> {
    use fastio_cli::api::signing::UpdateEnvelopeParams;
    let documents = match sign_json_array(args, "documents_json")? {
        Some(i) => Some(sign_parse_documents(&i)?),
        None => None,
    };
    let recipients = match sign_json_array(args, "recipients_json")? {
        Some(i) => Some(sign_parse_recipients(&i)?),
        None => None,
    };
    let fields = match sign_json_array(args, "fields_json")? {
        Some(i) => Some(sign_parse_fields(&i)?),
        None => None,
    };
    Ok(UpdateEnvelopeParams::new()
        .name(optional_str(args, "name").map(str::to_owned))
        .expires_at(optional_str(args, "expires_at").map(str::to_owned))
        .policy_json(sign_json_object(args, "policy_json")?)
        .documents(documents)
        .recipients(recipients)
        .fields(fields))
}

/// Recursively create a directory tree, restricting any directory this call
/// creates to `0700` (owner rwx only) on Unix.
///
/// Used for the implicitly-created default download directory so the agent's
/// downloaded signed PDFs / audit certs are not parked under a world/group
/// listable directory. On non-Unix this is a plain `create_dir_all`. (Only the
/// DIRECTORY is locked down here; the downloaded FILES respect the user's umask
/// per the shared `download_file_stream` — see the call site comment.)
fn create_dir_all_private(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Stream a signing download (source / signed PDF, or audit cert) to the local
/// filesystem and return a path + byte count (NEVER base64). Builds the download
/// path, resolves the output FILE path (default under `.fastio/downloads/`), and
/// streams via [`ApiClient::download_file_stream`] — never routing a signing
/// node id through `/storage/{node}/read/`.
async fn sign_download(
    client: &fastio_cli::client::ApiClient,
    action: &str,
    workspace_id: &str,
    args: &Map<String, Value>,
) -> CallToolResult {
    use fastio_cli::api::signing;
    let envelope_id = match required_str(args, "envelope_id") {
        Ok(v) => v,
        Err(e) => return e,
    };

    // Resolve the api path + a sensible default filename per artifact, plus the
    // SignOp discriminator: only the SIGNED PDF and the AUDIT certificate are
    // generated asynchronously (a 404 — live not-ready codes 1609/128301/146422
    // — there means "not ready yet"); a SOURCE-document download is a plain fetch
    // where a 404 is a genuine not-found.
    // document-download serves the same source/preview bytes, so there is no
    // separate MCP preview action.
    let (api_path_res, default_name, what, op) = match action {
        "document-download" => {
            let doc = match required_str(args, "document_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            (
                signing::document_download_path(workspace_id, envelope_id, doc),
                format!("{envelope_id}-{doc}-source.pdf"),
                "source document",
                SignOp::General,
            )
        }
        "signed-download" => {
            let doc = match required_str(args, "document_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            (
                signing::signed_document_download_path(workspace_id, envelope_id, doc),
                format!("{envelope_id}-{doc}-signed.pdf"),
                "signed document",
                SignOp::SignedFetch,
            )
        }
        // audit-download
        _ => (
            signing::audit_download_path(workspace_id, envelope_id),
            format!("{envelope_id}-audit.json"),
            "audit certificate",
            SignOp::AuditFetch,
        ),
    };

    let api_path = match api_path_res {
        Ok(p) => p,
        Err(e) => return error_text(&format!("invalid download request: {e}")),
    };

    // Resolve the output file path: an explicit output_path is used verbatim;
    // otherwise a default filename under .fastio/downloads/.
    //
    // Perms split: the DEFAULT download DIRECTORY is created 0700 (owner-only)
    // on Unix — it holds the agent's signed PDFs / audit certs and is created
    // implicitly, so it should not be world/group-listable. The downloaded
    // FILES themselves are left to the user's umask (the shared
    // `download_file_stream` does not force 0600): a downloaded document written
    // to a user-chosen path is correct CLI behavior, like `curl`/`cp`.
    // Target-selecting, same as the File Share download paths: a present
    // non-string `output_path` would be silently dropped and the signed document
    // written to the DEFAULT directory instead of where the caller asked.
    if let Err(e) = validate_string_arg(
        args,
        "output_path",
        "a destination path; a non-string would silently write to the default download path",
    ) {
        return e;
    }
    let out_path = if let Some(p) = optional_str(args, "output_path") {
        std::path::PathBuf::from(p)
    } else {
        let dir = std::path::Path::new(".fastio/downloads");
        if let Err(e) = create_dir_all_private(dir) {
            return error_text(&format!(
                "failed to create output directory '{}': {e}",
                dir.display()
            ));
        }
        dir.join(default_name)
    };

    match client.download_file_stream(&api_path, &out_path).await {
        Ok(bytes) => {
            let result = serde_json::json!({
                "result": "yes",
                "downloaded": {
                    "artifact": what,
                    "path": out_path.display().to_string(),
                    "byte_count": bytes,
                },
            });
            success_json(&result)
        }
        Err(e) => sign_err_to_result(&e, &format!("failed to download {what}"), op),
    }
}

// ─── File Shares ────────────────────────────────────────────────────────────

/// Discriminates a File Share call by its AUTH CLASS and `create`-ness, mirroring
/// the CLI's `FsOp` so [`fileshare_err_to_result`] can reframe a `1650`/`401`
/// correctly: a management call authenticates with the caller's ACCOUNT token (a
/// `1650` there is account auth, not a link password), whereas a link-access call
/// (consumption) authenticates against the share's LINK gate (`x-ve-password`, so
/// a `1650` is a link-password failure). A `create` additionally hints
/// node-must-be-a-file on a `1605`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FsMcpOp {
    /// A `create` call — a `1605` invalid-input hints node-must-be-a-file.
    Create,
    /// A management call other than `create` (list / update / delete / grants /
    /// activity). Authenticates with the ACCOUNT token, so a `1650`/`401` is an
    /// account-auth failure, not a link password.
    ManagementOther,
    /// A link-access call (consumption: info / download / versions).
    /// `x-ve-password` applies, so a `1650`/`401` is a link-password failure.
    LinkAccess,
    /// The `preview` consumption call. Like [`Self::LinkAccess`] for auth
    /// purposes (`x-ve-password` applies, so a `1650`/`401` is a link-password
    /// failure), but a `404` WITHOUT a share-gone code (`1609`) — or the
    /// preview-specific `143705` — is a PREVIEW miss (the share exists; the
    /// requested preview asset does not), NOT a share-gone.
    Preview,
}

impl FsMcpOp {
    /// Whether this op authenticates against the share's LINK gate
    /// (`x-ve-password`), so a `1650`/`401` means a link-password failure rather
    /// than an account-auth failure. Both consumption classes (`LinkAccess` and
    /// `Preview`) gate on the link.
    fn is_link_access(self) -> bool {
        matches!(self, Self::LinkAccess | Self::Preview)
    }

    /// Whether this is the `preview` op, so a bare `404` is a preview miss rather
    /// than a share-gone (the SHARE exists; the requested preview asset does not).
    fn is_preview(self) -> bool {
        matches!(self, Self::Preview)
    }
}

/// Map a File Share API error to an MCP result.
///
/// Mirrors the CLI's `map_fileshare_error` — File-Share-specific wording lives
/// HERE (and there), never in the global `error.rs` hints. Keyed on the actual
/// `error.code` (HTTP status only as a secondary signal), with the same
/// auth-class awareness as the CLI:
///
/// - `1650`/`401` → a link-password failure (steer to the `password` arg) ONLY on
///   a [`FsMcpOp::LinkAccess`] op; on a management op it is account auth, so it
///   defers to the generic suggestion ("run `fastio auth login`").
/// - `1700`/`403` → capability insufficient (view < download < edit; a write-back
///   `edit` grant is CLI-only).
/// - `1609`/`404` → UNIFORM "unavailable" (never distinguish not-found / expired /
///   revoked — no enumeration oracle).
/// - `1680`/`403` → the bound file is not serveable (locked / DMCA / infected).
/// - `1605`/`400` → surface the server message; on create, hint node-must-be-a-file.
/// - A [`CliError::VersionConflict`] passes through with its current-version hint.
/// - Everything else defers to the shared `err.suggestion()`.
fn fileshare_err_to_result(
    err: &fastio_cli::error::CliError,
    ctx: &str,
    op: FsMcpOp,
) -> CallToolResult {
    use fastio_cli::error::CliError;

    // A CAS conflict is already fully framed by its Display + suggestion. Carry
    // `suggestion()` into the MCP result for parity with the CLI render.
    //
    // Deliberately FORWARDS the hint rather than restating it: when the advice
    // was corrected (check-before-re-apply, because a retried session can report
    // a conflict naming the caller's own write), this surface inherited the fix
    // with no edit. A restated copy here would have kept the old advice and
    // nothing would have failed.
    if let CliError::VersionConflict { current_version } = err {
        let base = format!(
            "{ctx}: the bound file changed since the version you supplied; current version is \
             {current_version} ({err})"
        );
        return match err.suggestion() {
            Some(hint) => error_text(&format!("{base} {hint}")),
            None => error_text(&base),
        };
    }

    let CliError::Api(api) = err else {
        // A non-Api error (auth / io / parse): defer to the shared suggestion.
        if let Some(hint) = err.suggestion() {
            return error_text(&format!("{ctx}: {hint} ({err})"));
        }
        return error_text(&format!("{ctx}: {err}"));
    };

    // Code-keyed reframing (HTTP status only as a secondary fallback below).
    let note: Option<String> = match api.code {
        1650 if op.is_link_access() => Some(
            "this File Share requires a link password (or the one supplied is wrong) (1650). Pass \
             the `password` arg (it travels only in the x-ve-password header and is never logged)."
                .to_owned(),
        ),
        1700 => Some(
            "your capability on this File Share is insufficient for this action (1700). \
             Capabilities are ordered view < download < edit; writing a new version is the \
             CLI-only `fastio fileshare upload` and needs an explicit `edit` grant."
                .to_owned(),
        ),
        // Preview-specific miss (143705 / "Unable to retrieve preview"). Emitted
        // ONLY by the storage preview-read path's default arm (server
        // `storage/Io.php`), so keying on the code alone is op-independent and
        // safe — the SHARE exists; only the requested preview asset does not.
        // Distinct from the uniform-unavailable 1609 below (the share itself).
        143_705 => Some(
            "no preview of this type is available for the bound file (143705) — it may still be \
             generating, or this file type may not support it. Retry shortly, or try another \
             preview_type."
                .to_owned(),
        ),
        1609 => Some(
            "this File Share is unavailable (1609) — it may not exist, may have expired, or may \
             have been revoked."
                .to_owned(),
        ),
        1680 => Some(
            "the bound file cannot be served (1680) — it may be locked, taken down (DMCA), or \
             flagged as infected."
                .to_owned(),
        ),
        1605 => Some(if op == FsMcpOp::Create {
            format!(
                "invalid request (1605): {}. The node_id must be a FILE node (not a folder or \
                 note).",
                api.message
            )
        } else {
            format!("invalid request (1605): {}", api.message)
        }),
        _ => None,
    };

    // Bare-status fallback for password / unavailable when the server returns the
    // status without the specific code (only after the code match misses).
    let note = note.or_else(|| {
        match api.http_status {
        401 if op.is_link_access() => Some(
            "this File Share requires a link password (or the one supplied is wrong). Pass the \
             `password` arg (it travels only in the x-ve-password header and is never logged)."
                .to_owned(),
        ),
        // A bare 404 on the PREVIEW op (no 1609, no 143705) is a preview miss, not
        // a share-gone — the call reached the share but the requested preview
        // asset does not exist. A bare 404 on any NON-preview op keeps the uniform
        // "unavailable" below (that genuinely means the share is gone).
        404 if op.is_preview() => Some(
            "no preview of this type is available for the bound file — it may still be generating, \
             or this file type may not support it. Retry shortly, or try another preview_type."
                .to_owned(),
        ),
        404 => Some(
            "this File Share is unavailable — it may not exist, may have expired, or may have \
             been revoked."
                .to_owned(),
        ),
        _ => None,
    }
    });

    if let Some(note) = note {
        return error_text(&format!("{ctx}: {note} ({err})"));
    }
    // No File-Share framing (e.g. a management 1650/401): defer to the shared
    // suggestion so the generic account-login guidance still fires.
    if let Some(hint) = err.suggestion() {
        return error_text(&format!("{ctx}: {hint} ({err})"));
    }
    error_text(&format!("{ctx}: {err}"))
}

/// The authoritative per-action describe payload for the `fileshare` tool. Names
/// the CLI-binary-only actions (`upload`, `ws-token`) under `cli_only_actions`
/// and the confirm-gated ones under `destructive_actions`.
// The length is a flat action-spec table, not branching logic (mirrors
// `sign_describe`).
#[allow(clippy::too_many_lines)]
fn fileshare_describe() -> CallToolResult {
    let actions: &[(&str, &[&str], &[&str], &str)] = &[
        ("describe", &[], &[], ""),
        (
            "create",
            &["workspace_id", "node_id"],
            &[
                "title",
                "access_option",
                "password",
                "expires",
                "expires_at",
            ],
            "binds a SINGLE FILE node and returns the durable link. node_id must be a FILE node \
             (not a folder or note). expires and expires_at are mutually exclusive. password \
             protects the link (x-ve-password; never logged).",
        ),
        (
            "list",
            &["workspace_id"],
            &["offset", "limit"],
            "offset-paginated; each row carries the grant_count / grants_preview.",
        ),
        (
            "info",
            &["fileshare_id"],
            &["password"],
            "details + effective_capability + the bound file metadata. ANONYMOUS-capable on a \
             public (anyone_with_link) share; supply password for a protected link.",
        ),
        (
            "update",
            &["fileshare_id"],
            &[
                "title",
                "access_option",
                "password",
                "clear_password",
                "expires",
                "expires_at",
                "clear_expires",
            ],
            "supply at least one mutable field. clear_password REMOVES the password (do not also \
             pass password); clear_expires CLEARS the expiry. expires and expires_at are mutually \
             exclusive.",
        ),
        (
            "grants-list",
            &["fileshare_id"],
            &[],
            "live named-people grants (no pagination; first 1000).",
        ),
        (
            "grants-add",
            &["fileshare_id", "capability"],
            &["user", "email"],
            "grant or raise a user's capability. Supply EXACTLY ONE of user / email. An \
             unregistered email sends a real invite.",
        ),
        (
            "grants-remove",
            &["fileshare_id", "confirm_revoke"],
            &["user", "email"],
            "REQUIRES confirm_revoke=true. Supply EXACTLY ONE of user / email. Idempotent.",
        ),
        (
            "delete",
            &["fileshare_id", "confirm_delete"],
            &[],
            "REQUIRES confirm_delete=true. Revokes the link and cascades its grants; the bound \
             file is NOT touched.",
        ),
        (
            "versions",
            &["fileshare_id"],
            &["password"],
            "lists the bound file's versions. ANONYMOUS-capable; supply password for a protected \
             link.",
        ),
        (
            "download",
            &["fileshare_id"],
            &["version", "password", "output_path"],
            "streams the bound file (or a historical version) to the local fs; returns a path + \
             byte count. ANONYMOUS-capable; supply password for a protected link.",
        ),
        (
            "preview",
            &["fileshare_id", "preview_type"],
            &["password", "output_path"],
            "streams a DERIVED preview asset (PRIMARY asset only; sub-assets of a multi-file \
             preview are not fetched) to the local fs; returns a path + byte count. \
             ANONYMOUS-capable.",
        ),
        (
            "activity",
            &["fileshare_id"],
            &["lastactivity", "wait", "updated"],
            "a SINGLE activity poll (members-only; always authed). Pass wait / lastactivity / \
             updated through — this does NOT loop.",
        ),
    ];

    let mut action_map = serde_json::Map::new();
    for (name, required, optional, note) in actions {
        let mut spec = serde_json::Map::new();
        spec.insert(
            "required".to_owned(),
            Value::Array(
                required
                    .iter()
                    .map(|s| Value::String((*s).to_owned()))
                    .collect(),
            ),
        );
        spec.insert(
            "optional".to_owned(),
            Value::Array(
                optional
                    .iter()
                    .map(|s| Value::String((*s).to_owned()))
                    .collect(),
            ),
        );
        if !note.is_empty() {
            spec.insert("note".to_owned(), Value::String((*note).to_owned()));
        }
        action_map.insert((*name).to_owned(), Value::Object(spec));
    }

    let cli_only: Vec<Value> = [
        "upload (write-back of a NEW VERSION of the bound file — needs local file bytes; \
         destructive). Run `fastio fileshare upload <id> <file>`.",
        "ws-token (realtime WebSocket-token mint — the token is a long-lived secret and must not \
         be parked in an MCP transcript; the CLI redacts it from stdout and writes it 0600). Run \
         `fastio fileshare ws-token <id> --token-file <path>`.",
    ]
    .iter()
    .map(|s| Value::String((*s).to_owned()))
    .collect();

    let payload = serde_json::json!({
        "tool": "fileshare",
        "summary": "File Shares — durable, link-shareable views of a SINGLE workspace file \
                    (replacing the retired QuickShare). READ + DRIVE over MCP; the write-back \
                    'upload' and the realtime 'ws-token' mint are CLI-binary-only.",
        "destructive_actions": ["delete (confirm_delete=true)", "grants-remove (confirm_revoke=true)"],
        "side_effects": "create makes a durable shareable link. delete revokes the link + \
                         cascades grants (bound file untouched) and REQUIRES confirm_delete=true; \
                         grants-remove REQUIRES confirm_revoke=true. download / preview write \
                         files to the agent's local filesystem. grants-add with an unregistered \
                         email sends a real invite. The write-back 'upload' (new bound-file \
                         version) and the 'ws-token' mint are CLI-binary-only (see \
                         cli_only_actions).",
        "guidance": {
            "password": "A protected link requires the `password` arg on info / download / \
                         versions / preview (and to set it on create / update). It travels ONLY \
                         in the x-ve-password header and is NEVER logged or echoed in results.",
            "anonymous": "info / download / versions / preview may be ANONYMOUS on a public \
                          (anyone_with_link) share — no auth needed; a protected link needs the \
                          password arg.",
            "write_back": "To replace the bound file's bytes with a new version, run the CLI: \
                           `fastio fileshare upload <id> <file> [--if-version <vid>]`. \
                           --if-version is a server-enforced CAS precondition: when the server \
                           detects a version conflict it reports CONFLICT_VERSION_MISMATCH and \
                           the CLI surfaces it as a version-conflict error with the current \
                           version id — re-download, re-apply, retry with that id. This is NOT \
                           available over MCP (it needs local file bytes and is destructive).",
            "ws_token": "To mint a realtime WebSocket token, run the CLI: `fastio fileshare \
                         ws-token <id> --token-file <path>` (0600). NOT exposed over MCP — the \
                         token is a long-lived secret that must not be parked in an MCP \
                         transcript.",
            "cli_only_actions": cli_only,
        },
        "actions": Value::Object(action_map),
    });
    success_json(&payload)
}

/// File Share tool handler — READ + DRIVE actions. The write-back `upload` and
/// the realtime `ws-token` mint are CLI-binary-only and are routed to a guidance
/// message BEFORE auth/arg extraction (mirrors how `sign` keeps send/void
/// CLI-only). `delete` and `grants-remove` are confirm-gated (`confirm_delete` /
/// `confirm_revoke`) and rejected BEFORE the network call when unconfirmed.
#[allow(clippy::too_many_lines)] // a flat dispatch over the File Share surface
async fn handle_fileshare(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    use fastio_cli::api::{event, fileshare};

    // `describe` needs no auth.
    if action == "describe" {
        return Ok(fileshare_describe());
    }
    // The write-back `upload` is CLI-binary-only: it pushes a NEW VERSION of the
    // bound file, needs the local file bytes, and is destructive. Route it to the
    // CLI-only guidance FIRST — before auth/arg extraction — so e.g.
    // `action=upload` with no fileshare_id returns the intended "this is
    // CLI-only" message rather than a missing-parameter error.
    if matches!(action, "upload" | "writeback" | "write-back") {
        return Ok(error_text(
            "fileshare upload (write-back) is CLI-binary-only: it pushes a NEW VERSION of the \
             bound file and needs the local file bytes. Run it via the CLI — `fastio fileshare \
             upload <id> <file> [--if-version <vid>] [--password …]`. --if-version is a \
             server-enforced CAS precondition: when the server detects a version conflict it \
             reports CONFLICT_VERSION_MISMATCH and the CLI surfaces it as a version-conflict \
             error with the current version id. Call action='describe' for the MCP action list.",
        ));
    }
    // `ws-token` (realtime WebSocket-token mint) is CLI-binary-only: the token is
    // a long-lived secret that must not be parked in an MCP transcript. The CLI
    // redacts it from stdout and writes it 0600 to --token-file.
    if matches!(action, "ws-token" | "websocket" | "realtime-token") {
        return Ok(error_text(
            "fileshare ws-token (realtime WebSocket-token mint) is CLI-binary-only: the token is \
             a long-lived secret that must not be parked in an MCP transcript. Run it via the CLI \
             — `fastio fileshare ws-token <id> --token-file <path>` (written 0600). Call \
             action='describe' for the MCP action list.",
        ));
    }
    // Confirm gates PREFLIGHT — before auth and before arg extraction — so an
    // unauthenticated or arg-less probe of a destructive action gets the gate
    // message (not "Not authenticated" or a missing-parameter error), mirroring
    // the CLI's `--yes` gate which fires regardless of session state. The
    // post-auth XOR validation for grants-remove still runs (only once confirmed).
    if action == "delete" && optional_bool(args, "confirm_delete") != Some(true) {
        return Ok(error_text(FILESHARE_DELETE_CONFIRM));
    }
    if action == "grants-remove" && optional_bool(args, "confirm_revoke") != Some(true) {
        return Ok(error_text(FILESHARE_REVOKE_CONFIRM));
    }
    // Auth split: the four LINK-ACCESS consumption actions (info / download /
    // versions / preview) may run ANONYMOUSLY per the share's access tier (spec
    // §5; CLI parity via `build_client_allow_anonymous`). When the MCP server
    // holds no token, `state.client()` is a token-less `ApiClient` (see
    // `McpState::new` / `FastioMcpServer::new`): the Wave-1 `get_with_password` /
    // `download_*_with_password` helpers attach `Authorization: Bearer` ONLY when
    // the client holds a token, so calling them as-is IS the anonymous path. A
    // `named_people` / `any_registered` share then returns 401/403, which
    // `fileshare_err_to_result` renders correctly. Management actions
    // (create / list / update / delete / grants-* / activity) still require auth.
    if !matches!(action, "info" | "download" | "versions" | "preview")
        && let Err(e) = require_auth(state).await
    {
        return Ok(e);
    }
    let client = state.client().read().await;

    match action {
        "create" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // A non-string `password` would be silently dropped → an UNPROTECTED
            // share; reject it before building params.
            if let Err(e) = fileshare_validate_password_arg(args) {
                return Ok(e);
            }
            // Strictly parse the expiry inputs: a present-but-invalid `expires`
            // (e.g. `"abc"`, `-1`, `1.5`, `null`) or a present non-string
            // `expires_at` must be rejected here, NOT silently dropped — a
            // silently-dropped `expires` on create would make a DURABLE share.
            let (expires, expires_at) = match fileshare_strict_expiry(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = fileshare::CreateFileShareParams::new()
                .node(Some(node_id.to_owned()))
                .title(optional_str(args, "title").map(str::to_owned))
                .access_option(optional_str(args, "access_option").map(str::to_owned))
                .password(fileshare_mcp_password(args))
                .expires(expires)
                .expires_at(expires_at);
            if let Err(e) = params.validate() {
                return Ok(error_text(&format!("invalid create request: {e}")));
            }
            match fileshare::create_fileshare(&client, workspace_id, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to create File Share",
                    FsMcpOp::Create,
                )),
            }
        }
        "list" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match fileshare::list_fileshares(
                &client,
                workspace_id,
                optional_u32(args, "offset"),
                optional_u32(args, "limit"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to list File Shares",
                    FsMcpOp::ManagementOther,
                )),
            }
        }
        "info" => {
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            if let Err(e) = fileshare_validate_consumption_password_arg(args) {
                return Ok(e);
            }
            let password = fileshare_mcp_password(args);
            match fileshare::get_details(&client, fileshare_id, password.as_ref()).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to get File Share details",
                    FsMcpOp::LinkAccess,
                )),
            }
        }
        "update" => {
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // A non-string `password` is a hard error (a JSON number would be
            // silently dropped → an unintended state); validate presence/type
            // BEFORE resolving it.
            if let Err(e) = fileshare_validate_password_arg(args) {
                return Ok(e);
            }
            let clear_password = optional_bool(args, "clear_password") == Some(true);
            // Reject `password` + `clear_password=true` (the CLI rejects the
            // analogous `--password` + `--clear-password` via a clap conflict).
            // Setting and clearing in one call is contradictory; surface it
            // explicitly rather than silently clearing.
            if clear_password && args.get("password").is_some() {
                return Ok(error_text(
                    "conflicting update: `password` and `clear_password=true` cannot be combined — \
                     pass `password` to SET a password or `clear_password=true` to REMOVE it, not \
                     both.",
                ));
            }
            // Honor clear_password: when clearing, do NOT also resolve a password
            // (the library rejects password + clear together).
            let password = if clear_password {
                None
            } else {
                fileshare_mcp_password(args)
            };
            // Same strict expiry parse as create: reject a present-but-invalid
            // `expires` / non-string `expires_at` rather than silently dropping
            // it (a silent drop would leave the expiry unchanged unexpectedly).
            let (expires, expires_at) = match fileshare_strict_expiry(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = fileshare::UpdateFileShareParams::new()
                .title(optional_str(args, "title").map(str::to_owned))
                .access_option(optional_str(args, "access_option").map(str::to_owned))
                .password(password)
                .clear_password(clear_password)
                .expires(expires)
                .expires_at(expires_at)
                .clear_expires(optional_bool(args, "clear_expires") == Some(true));
            if params.is_empty() {
                return Ok(error_text(
                    "nothing to update: supply at least one of title, access_option, password, \
                     clear_password, expires, expires_at, or clear_expires",
                ));
            }
            if let Err(e) = params.validate() {
                return Ok(error_text(&format!("invalid update request: {e}")));
            }
            match fileshare::update_fileshare(&client, fileshare_id, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to update File Share",
                    FsMcpOp::ManagementOther,
                )),
            }
        }
        "grants-list" => {
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match fileshare::list_grants(&client, fileshare_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to list File Share grants",
                    FsMcpOp::ManagementOther,
                )),
            }
        }
        "grants-add" => {
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let capability = match required_str(args, "capability") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = fileshare::GrantParams::new()
                .user(optional_str(args, "user").map(str::to_owned))
                .email(optional_str(args, "email").map(str::to_owned))
                .capability(Some(capability.to_owned()));
            if let Err(e) = params.validate_add() {
                return Ok(error_text(&format!("invalid grant request: {e}")));
            }
            match fileshare::add_grant(&client, fileshare_id, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to add File Share grant",
                    FsMcpOp::ManagementOther,
                )),
            }
        }
        "grants-remove" => {
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = fileshare::GrantParams::new()
                .user(optional_str(args, "user").map(str::to_owned))
                .email(optional_str(args, "email").map(str::to_owned));
            // The confirm gate already fired pre-auth at the top of the handler
            // (so an unauthed / arg-less probe sees the gate message). Here we
            // run only the post-confirm user/email XOR validation.
            if let Err(e) = params.validate_remove() {
                return Ok(error_text(&format!("invalid grant request: {e}")));
            }
            match fileshare::remove_grant(&client, fileshare_id, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to remove File Share grant",
                    FsMcpOp::ManagementOther,
                )),
            }
        }
        "delete" => {
            // The confirm gate already fired pre-auth at the top of the handler.
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match fileshare::delete_fileshare(&client, fileshare_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to delete File Share",
                    FsMcpOp::ManagementOther,
                )),
            }
        }
        "versions" => {
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            if let Err(e) = fileshare_validate_consumption_password_arg(args) {
                return Ok(e);
            }
            let password = fileshare_mcp_password(args);
            match fileshare::list_versions(&client, fileshare_id, password.as_ref()).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to list File Share versions",
                    FsMcpOp::LinkAccess,
                )),
            }
        }
        "download" => Ok(fileshare_download(&client, args).await),
        "preview" => Ok(fileshare_preview(&client, args).await),
        "activity" => {
            let fileshare_id = match required_str(args, "fileshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // `required_str` accepts a present-but-empty `""`; `poll_activity`
            // does NOT guard the entity id, so an empty id would build
            // `/activity/poll//`. Reject it explicitly here.
            if fileshare_id.is_empty() {
                return Ok(error_text(
                    "invalid request: `fileshare_id` must not be empty for the activity action.",
                ));
            }
            // A SINGLE poll — pass wait / lastactivity / updated through; no loop.
            match event::poll_activity(
                &client,
                fileshare_id,
                optional_str(args, "lastactivity"),
                optional_u32(args, "wait"),
                optional_bool(args, "updated") == Some(true),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(fileshare_err_to_result(
                    &e,
                    "failed to poll File Share activity",
                    FsMcpOp::ManagementOther,
                )),
            }
        }
        // upload / ws-token are CLI-only and handled earlier (before auth/arg
        // extraction), so they are not matched here.
        _ => Ok(error_text(&format!(
            "Unknown or CLI-only fileshare action: {action}. The write-back `upload` and the \
             realtime `ws-token` mint are CLI-binary-only (`fastio fileshare …`). Call \
             action='describe' for the MCP action list."
        ))),
    }
}

/// Rejection message for `delete` invoked without `confirm_delete=true`.
const FILESHARE_DELETE_CONFIRM: &str = "fileshare delete permanently revokes the link and cascades \
     its grants (the bound file is not touched); pass confirm_delete=true to proceed (mirrors the \
     CLI --yes gate).";

/// Rejection message for `grants-remove` invoked without `confirm_revoke=true`.
const FILESHARE_REVOKE_CONFIRM: &str = "fileshare grants-remove revokes this user's access to the \
     File Share; pass confirm_revoke=true to proceed (mirrors the CLI --yes gate).";

/// Strictly parse the File-Share expiry inputs, distinguishing ABSENT (→ `None`)
/// from PRESENT-but-invalid (→ a clear error).
///
/// `expires` is read elsewhere via [`optional_u64`], which silently returns
/// `None` for a present-but-invalid value (`"abc"`, `-1`, `1.5`, `true`,
/// `null`, an object/array, or a string that does not parse to a `u64`). On
/// `create` that silent drop is a security footgun: a bad `expires` would make
/// a DURABLE (never-expiring) share instead of being rejected. Likewise a
/// present non-string `expires_at` (a number/bool/null) is silently dropped by
/// [`optional_str`].
///
/// This fileshare-scoped parser rejects a present-but-invalid `expires` or a
/// present non-string `expires_at` with an explicit message, while preserving
/// the [`optional_u64`] convenience for VALID values — a string-encoded integer
/// such as `"60"` is still accepted. Absent keys resolve to `None`. The mutual
/// exclusion of `expires` / `expires_at` is enforced later by the library
/// validator (`CreateFileShareParams::validate` / `UpdateFileShareParams`).
fn fileshare_strict_expiry(
    args: &Map<String, Value>,
) -> Result<(Option<u64>, Option<String>), CallToolResult> {
    let expires = match args.get("expires") {
        // A truly-absent key resolves to `None`.
        None => None,
        // A present key must resolve to a valid `u64`: a JSON number that is a
        // non-negative integer, or a string that parses to a `u64` (preserving
        // `optional_u64`'s convenience for valid string-encoded integers).
        // Everything else (`"abc"`, `-1`, `1.5`, `true`, `null`, an
        // object/array) is rejected rather than silently dropped.
        Some(v) => match v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        {
            Some(n) => Some(n),
            None => {
                return Err(error_text(
                    "invalid request: `expires` must be a positive integer (seconds)",
                ));
            }
        },
    };
    let expires_at = match args.get("expires_at") {
        None => None,
        Some(v) if v.is_string() => v.as_str().map(str::to_owned),
        Some(_) => {
            return Err(error_text(
                "invalid request: `expires_at` must be a string (an ISO-8601 / \
                 `YYYY-MM-DD HH:MM:SS` timestamp); pass `expires` for a relative \
                 expiry in seconds",
            ));
        }
    };
    Ok((expires, expires_at))
}

/// Resolve the optional link `password` arg, wrapping it in a [`SecretString`]
/// immediately so the plaintext lifetime is minimal and it is never echoed.
///
/// Flag PRESENCE is preserved: a PRESENT-but-empty `password` (`""`) flows
/// through as `Some("")` so the Wave-1 library validator rejects it with its
/// clear message, rather than being silently downgraded to "absent" (which on
/// create would produce an UNPROTECTED share). The password travels ONLY in the
/// `x-ve-password` header (threaded by the Wave-1 client helpers).
fn fileshare_mcp_password(args: &Map<String, Value>) -> Option<SecretString> {
    // `Value::as_str` returns `Some("")` for a present empty string, preserving
    // PRESENCE — exactly the semantics the validator depends on.
    args.get("password")
        .and_then(Value::as_str)
        .map(|s| SecretString::from(s.to_owned()))
}

/// Reject a PRESENT-but-non-string `password` arg with a clear type error.
///
/// `fileshare_mcp_password` resolves via `Value::as_str`, which returns `None`
/// for a non-string JSON value (e.g. a number or boolean). Without this guard a
/// JSON-number password on `create` would be SILENTLY DROPPED, producing an
/// UNPROTECTED File Share; on a link-access action it would silently fall back
/// to the unauthenticated/no-password path. Callers MUST run this before
/// resolving the password. An ABSENT password (`None`) is fine; a present
/// string (including `""`) is fine — the empty-string case is rejected later by
/// the library validator. The value is NEVER echoed in the error.
fn fileshare_validate_password_arg(args: &Map<String, Value>) -> Result<(), CallToolResult> {
    match args.get("password") {
        Some(v) if !v.is_string() => Err(error_text(
            "invalid request: `password` must be a string (the link password travels in the \
             x-ve-password header).",
        )),
        _ => Ok(()),
    }
}

/// Validate the `password` arg on a CONSUMPTION / WRITE-BACK path (info /
/// download / versions / preview / upload): reject a non-string type AND a
/// present-but-EMPTY string.
///
/// On these paths the resolved password is applied DIRECTLY as the
/// `x-ve-password` header — the library `validate()` (which rejects an empty
/// password on management create/update) never runs. A link password is
/// contractually 1-255 chars, so a present `""` is invalid: sending an empty
/// header is meaningless and only masks an unprotected-share mistake. An ABSENT
/// password (`None`) is the correct way to consume an UNPROTECTED share, so only
/// a PRESENT empty string is rejected. The value is NEVER echoed in the error.
fn fileshare_validate_consumption_password_arg(
    args: &Map<String, Value>,
) -> Result<(), CallToolResult> {
    fileshare_validate_password_arg(args)?;
    if args.get("password").and_then(Value::as_str) == Some("") {
        return Err(error_text(
            "invalid request: link password cannot be empty — omit `password` for an \
             unprotected share.",
        ));
    }
    Ok(())
}

/// Reject a PRESENT-but-non-string TARGET-SELECTING arg with a clear type error.
///
/// `optional_str` resolves via `Value::as_str`, which returns `None` for a
/// non-string JSON value (e.g. a number or boolean) — so a present-but-non-string
/// `version` or `output_path` would be SILENTLY DROPPED and the action would
/// proceed against the WRONG target: a non-string `version` falls back to the
/// CURRENT file (wrong bytes, no error), and a non-string `output_path` falls
/// back to the DEFAULT path (the file is written somewhere the caller did not
/// ask for). Both are target-selecting, so the silent drop is a correctness bug,
/// not a benign convenience. An ABSENT arg is fine; a present string is fine.
/// `note` is appended to clarify the consequence of the wrong type.
fn validate_string_arg(
    args: &Map<String, Value>,
    key: &str,
    note: &str,
) -> Result<(), CallToolResult> {
    match args.get(key) {
        Some(v) if !v.is_string() => Err(error_text(&format!(
            "invalid request: `{key}` must be a string ({note})."
        ))),
        _ => Ok(()),
    }
}

/// Stream a File Share's bound file (or a historical version) to the local
/// filesystem and return a path + byte count. ANONYMOUS-capable; the optional
/// `password` authorizes a protected link (x-ve-password). The default output
/// directory (`.fastio/downloads/`) is created `0700`.
async fn fileshare_download(
    client: &fastio_cli::client::ApiClient,
    args: &Map<String, Value>,
) -> CallToolResult {
    use fastio_cli::api::fileshare;
    let fileshare_id = match required_str(args, "fileshare_id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = fileshare_validate_consumption_password_arg(args) {
        return e;
    }
    // `version` and `output_path` are TARGET-SELECTING: a present-but-non-string
    // `version` would be silently dropped → the CURRENT file downloads instead of
    // the requested one (wrong bytes, no error); a present-but-non-string
    // `output_path` would be dropped → the file is written to the DEFAULT path.
    // Reject either before resolving them.
    if let Err(e) = validate_string_arg(
        args,
        "version",
        "a version id; a non-string would silently download the current file instead",
    ) {
        return e;
    }
    if let Err(e) = validate_string_arg(
        args,
        "output_path",
        "a destination path; a non-string would silently write to the default download path",
    ) {
        return e;
    }
    let password = fileshare_mcp_password(args);

    let api_path = match optional_str(args, "version") {
        Some(v) => fileshare::storage_version_read_path(fileshare_id, v),
        None => fileshare::storage_read_path(fileshare_id),
    };
    let api_path = match api_path {
        Ok(p) => p,
        Err(e) => return error_text(&format!("invalid download request: {e}")),
    };

    // Resolve the output path. An explicit `output_path` is used VERBATIM, and
    // — mirroring the CLI — we then SKIP the best-effort details fetch entirely
    // (it is only ever needed to derive a default filename). Only when no
    // `output_path` was supplied do we fetch details to name the file.
    let out_path = if let Some(p) = optional_str(args, "output_path") {
        std::path::PathBuf::from(p)
    } else {
        // Default filename: the bound file's name (best-effort details fetch,
        // sanitized) else "<id>-download". The best-effort error is swallowed
        // (the real download error, if any, surfaces on the stream call); the
        // CliError/ApiError Display carries only server diagnostics, never the
        // password (it travels only in the request header).
        let default_name =
            match fileshare::get_details(client, fileshare_id, password.as_ref()).await {
                Ok(details) => fileshare::fileshare_file_name(&details).map_or_else(
                    // FALLBACK: the bound-file name is unavailable, so the
                    // default is derived from the caller-influenced
                    // `fileshare_id`. Run it through `sanitize_filename` (strips
                    // `/`, `\\`, `..`, leading dots; empty → "download") so a
                    // crafted id with `../` or an absolute path cannot escape
                    // `.fastio/downloads/`.
                    || {
                        fastio_cli::api::download::sanitize_filename(&format!(
                            "{fileshare_id}-download"
                        ))
                    },
                    |n| fastio_cli::api::download::sanitize_filename(&n),
                ),
                // FALLBACK: the best-effort details fetch failed; same sanitize
                // as above so a crafted `fileshare_id` cannot traverse out of
                // the download dir.
                Err(_) => fastio_cli::api::download::sanitize_filename(&format!(
                    "{fileshare_id}-download"
                )),
            };
        match fileshare_resolve_output(args, &default_name) {
            Ok(p) => p,
            Err(e) => return e,
        }
    };

    match client
        .download_file_stream_with_password(&api_path, &out_path, password.as_ref())
        .await
    {
        Ok(bytes) => fileshare_download_result("bound file", &out_path, bytes),
        Err(e) => fileshare_err_to_result(&e, "failed to download File Share", FsMcpOp::LinkAccess),
    }
}

/// Stream a File Share's PRIMARY preview asset (after at most one manual,
/// leak-safe redirect) to the local filesystem and return a path + byte count.
/// Multi-file previews yield the primary asset only. ANONYMOUS-capable.
async fn fileshare_preview(
    client: &fastio_cli::client::ApiClient,
    args: &Map<String, Value>,
) -> CallToolResult {
    use fastio_cli::api::fileshare;
    let fileshare_id = match required_str(args, "fileshare_id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let preview_type = match required_str(args, "preview_type") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = fileshare_validate_consumption_password_arg(args) {
        return e;
    }
    // `output_path` is target-selecting: a present-but-non-string value would be
    // silently dropped → the preview is written to the DEFAULT path. Reject it.
    if let Err(e) = validate_string_arg(
        args,
        "output_path",
        "a destination path; a non-string would silently write to the default download path",
    ) {
        return e;
    }
    let password = fileshare_mcp_password(args);

    let api_path = match fileshare::storage_preview_path(fileshare_id, preview_type) {
        Ok(p) => p,
        Err(e) => return error_text(&format!("invalid preview request: {e}")),
    };

    // A preview is a derived asset, so the bound file name is not the right
    // default — use "<id>.<preview_type>".
    let default_name =
        fastio_cli::api::download::sanitize_filename(&format!("{fileshare_id}.{preview_type}"));
    let out_path = match fileshare_resolve_output(args, &default_name) {
        Ok(p) => p,
        Err(e) => return e,
    };

    match client
        .download_preview_following_redirect(&api_path, &out_path, password.as_ref())
        .await
    {
        Ok(bytes) => fileshare_download_result("preview", &out_path, bytes),
        Err(e) => fileshare_err_to_result(
            &e,
            "failed to download File Share preview",
            FsMcpOp::Preview,
        ),
    }
}

/// Resolve the output FILE path for a download / preview: an explicit
/// `output_path` is used verbatim; otherwise `default_name` under the
/// `.fastio/downloads/` directory (created `0700` on Unix). Returns an `Err`
/// (an MCP result) if the default directory cannot be created.
fn fileshare_resolve_output(
    args: &Map<String, Value>,
    default_name: &str,
) -> Result<std::path::PathBuf, CallToolResult> {
    if let Some(p) = optional_str(args, "output_path") {
        return Ok(std::path::PathBuf::from(p));
    }
    let dir = std::path::Path::new(".fastio/downloads");
    if let Err(e) = create_dir_all_private(dir) {
        return Err(error_text(&format!(
            "failed to create output directory '{}': {e}",
            dir.display()
        )));
    }
    Ok(dir.join(default_name))
}

/// Build the success result for a File Share download / preview (path + byte
/// count; NEVER base64).
fn fileshare_download_result(
    artifact: &str,
    out_path: &std::path::Path,
    bytes: u64,
) -> CallToolResult {
    success_json(&serde_json::json!({
        "result": "yes",
        "downloaded": {
            "artifact": artifact,
            "path": out_path.display().to_string(),
            "byte_count": bytes,
        },
    }))
}

/// System health tool handler.
///
/// System tools do not require authentication.
async fn handle_system(
    state: &McpState,
    action: &str,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    // System endpoints do not require authentication, but we still need a client.
    let client = state.client().read().await;
    match action {
        "ping" => match api::system::ping(&client).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "status" => match api::system::system_status(&client).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        _ => Ok(error_text(&format!("Unknown system action: {action}"))),
    }
}

/// Offline `id` tool handler — classify Fast.io identifiers with no auth and no
/// network. `describe` returns the structured action reference; `info`
/// classifies the `id` / `ids` parameters via [`fastio_cli::opaque_id`].
fn handle_id(action: &str, args: &Map<String, Value>) -> CallToolResult {
    match action {
        "describe" => id_describe(),
        "info" => {
            let ids = collect_id_args(args);
            if ids.is_empty() {
                return error_text(
                    "provide `id` (a single id) or `ids` (a JSON array of strings or a \
                     comma-separated string) to classify",
                );
            }
            let rows: Vec<Value> = ids
                .iter()
                .map(|id| fastio_cli::opaque_id::to_json(&fastio_cli::opaque_id::classify(id)))
                .collect();
            success_json(&Value::Array(rows))
        }
        _ => error_text(&format!("Unknown id action: {action}")),
    }
}

/// Gather ids from the `id` (single string) and `ids` (a JSON array of strings,
/// or a comma-separated / JSON-array-encoded string) parameters. Blank entries
/// are dropped. The MCP parameter schema advertises both as strings, so the
/// string forms of `ids` must be parsed here.
fn collect_id_args(args: &Map<String, Value>) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(s) = optional_str(args, "id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        ids.push(s.to_owned());
    }
    match args.get("ids") {
        Some(Value::Array(arr)) => {
            for v in arr {
                if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    ids.push(s.to_owned());
                }
            }
        }
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            // Accept a JSON-array-encoded string, else fall back to comma-split.
            let parsed = trimmed
                .starts_with('[')
                .then(|| serde_json::from_str::<Vec<String>>(trimmed).ok())
                .flatten();
            match parsed {
                Some(list) => ids.extend(
                    list.into_iter()
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty()),
                ),
                None => ids.extend(
                    trimmed
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                ),
            }
        }
        _ => {}
    }
    ids
}

/// The structured `describe` payload for the `id` tool — the authoritative
/// per-action reference (mirrors the shape used by the other `*_describe`
/// helpers). Needs no auth.
fn id_describe() -> CallToolResult {
    let actions: &[(&str, &[&str], &[&str], &str)] = &[
        ("describe", &[], &[], ""),
        (
            "info",
            &[],
            &["id", "ids"],
            "classify one (`id`) or many (`ids`) Fast.io identifiers offline; supply at least one",
        ),
    ];

    let mut action_map = serde_json::Map::new();
    for (name, required, optional, note) in actions {
        let mut spec = serde_json::Map::new();
        spec.insert(
            "required".to_owned(),
            Value::Array(
                required
                    .iter()
                    .map(|s| Value::String((*s).to_owned()))
                    .collect(),
            ),
        );
        spec.insert(
            "optional".to_owned(),
            Value::Array(
                optional
                    .iter()
                    .map(|s| Value::String((*s).to_owned()))
                    .collect(),
            ),
        );
        if !note.is_empty() {
            spec.insert("note".to_owned(), Value::String((*note).to_owned()));
        }
        action_map.insert((*name).to_owned(), Value::Object(spec));
    }

    let payload = serde_json::json!({
        "tool": "id",
        "summary": "Offline OpaqueId classifier — maps a Fast.io id to its entity type by its \
                    self-describing length + type prefix (29-char carries a 1-char type; 30-char \
                    carries a 2-char type — the workflow family under 'w' plus the non-workflow \
                    Task and Comment types). No auth, no network.",
        "destructive_actions": [],
        "side_effects": "none — pure local classification; no network calls and no credentials.",
        "guidance": {
            "one_of_required_body": ["id", "ids"],
            "classification": "A 30-char id is classified by its 2-char prefix against the combined \
                               workflow ('w*') + non-workflow 30-char (Task 'ta'/'tb'/'tc', Comment \
                               'ca'/'cb') map; an unmapped 2-char prefix is family='unknown'. A \
                               29-char id whose 1-char code is unmapped is reported \
                               family='unknown' (it may be a transitional workflow code pending \
                               reassignment), NEVER guessed.",
        },
        "actions": Value::Object(action_map),
    });
    success_json(&payload)
}

#[cfg(test)]
mod ripley_tool_tests {
    use super::{
        McpState, TOOL_DEFS, ToolRouter, inject_onboarding_url, sanitize_preview_response,
        sanitize_subscribe_response,
    };
    use serde_json::{Map, Value, json};
    use std::sync::Arc;

    /// Serialize a `CallToolResult` to JSON text so tests can assert on the
    /// rendered content without depending on rmcp's internal field layout.
    fn result_to_string(result: &super::CallToolResult) -> String {
        serde_json::to_string(result).unwrap_or_default()
    }

    fn unauthed_router() -> ToolRouter {
        // Inject `esign_enabled = true` so the existing `sign` dispatch tests
        // exercise the real handler without mutating the process environment
        // (unsafe under Rust 2024). The kill-switch's own gate is covered by the
        // dedicated `sign_*_disabled` tests below.
        ToolRouter::new_with_esign(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            true,
        )
    }

    /// An AUTHENTICATED router whose client points at a DEAD loopback port.
    ///
    /// Callers exercise what runs AFTER `require_auth`: argument validation,
    /// dispatch reachability, kill-switch gates. MOST arms reject their input
    /// before building a request and never open a socket — but some do proceed
    /// to a connection attempt (the arms that assert on the resulting transport
    /// error say so at their call sites). `127.0.0.1:1` is what makes that
    /// attempt fail instantly and LOCALLY, so no test on this router can reach
    /// the real API — whether or not its arm short-circuits today.
    async fn authed_router() -> ToolRouter {
        let state = Arc::new(McpState::new_unauthenticated_for_test("http://127.0.0.1:1"));
        state.set_token("test-token".to_owned()).await;
        ToolRouter::new_with_esign(state, true)
    }

    #[test]
    fn list_tools_advertises_ripley_not_ai() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"ripley"), "ripley tool must be advertised");
        assert!(
            !names.contains(&"ai"),
            "hidden `ai` alias must NOT be advertised in list_tools"
        );
    }

    #[tokio::test]
    async fn ai_alias_routes_to_ripley_handler() {
        // An unauthenticated call short-circuits at require_auth inside
        // handle_ai; reaching that (vs the unknown-tool arm) proves the
        // `ai` alias routed to the ripley handler.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("chat-list".to_owned()));
        let res = router.call_tool("ai", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Not authenticated"),
            "ai alias should reach handle_ai (auth gate), got: {text}"
        );
        assert!(
            !text.contains("Unknown tool"),
            "ai alias must not fall through to the unknown-tool arm"
        );
    }

    fn user_tool_actions() -> Vec<&'static str> {
        super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "user")
            .expect("user tool registered")
            .actions
            .to_vec()
    }

    #[test]
    fn user_tool_omits_irreversible_account_close() {
        // Account `close` permanently closes the user's account (irreversible),
        // so it is CLI-binary-only and MUST NOT be advertised over MCP — like the
        // sign `send` carve-out.
        let actions = user_tool_actions();
        assert!(
            !actions.contains(&"close"),
            "user MCP tool must NOT advertise the irreversible account-close action 'close'"
        );
    }

    #[tokio::test]
    async fn user_close_is_cli_only_over_mcp() {
        // `action=close` is gated BEFORE the auth check, so even an
        // unauthenticated router returns the CLI-only guidance (not the auth
        // error and not the unknown-action arm) — proving close is never routed
        // to the real handler over MCP.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("close".to_owned()));
        let res = router.call_tool("user", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("CLI-binary-only") && text.contains("PERMANENTLY"),
            "user close must return the CLI-only guidance, got: {text}"
        );
        assert!(
            !text.contains("Not authenticated"),
            "user close must be gated before the auth check, got: {text}"
        );
        assert!(
            !text.contains("Unknown user action"),
            "user close must hit the CLI-only guard, not the unknown-action arm, got: {text}"
        );
    }

    #[tokio::test]
    async fn workspace_search_routes_through_unified_search() {
        // The CLI `workspace search` and the MCP `workspace search` action now
        // share `api::search::unified_search_workspace`, which validates the
        // query BEFORE any network call. A blank query therefore returns the
        // unified-search validation error pre-network — proving MCP no longer
        // uses the un-validated `search_files` path (CLI/MCP parity).
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("search".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("query".to_owned(), Value::String("   ".to_owned()));
        let res = router.call_tool("workspace", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("search query must not be empty"),
            "workspace search must route through unified_search_workspace \
             (pre-network query validation), got: {text}"
        );
    }

    #[tokio::test]
    async fn comment_list_rejects_invalid_sort() {
        // The CLI restricts comment-list `sort` to asc/desc via a clap
        // value_parser; MCP enforces the same set pre-network, so a bogus sort
        // returns a clear error instead of reaching the server (CLI/MCP parity).
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        args.insert(
            "entity_type".to_owned(),
            Value::String("workspace".to_owned()),
        );
        args.insert("entity_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("n1".to_owned()));
        args.insert("sort".to_owned(), Value::String("bogus".to_owned()));
        let res = router.call_tool("comment", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Invalid sort 'bogus'") && text.contains("asc, desc"),
            "comment list must reject a non-asc/desc sort pre-network, got: {text}"
        );
    }

    #[tokio::test]
    async fn comment_list_all_rejects_invalid_sort() {
        // Same asc/desc enforcement on the `list-all` action.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list-all".to_owned()));
        args.insert(
            "entity_type".to_owned(),
            Value::String("workspace".to_owned()),
        );
        args.insert("entity_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("sort".to_owned(), Value::String("sideways".to_owned()));
        let res = router.call_tool("comment", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Invalid sort 'sideways'") && text.contains("asc, desc"),
            "comment list-all must reject a non-asc/desc sort pre-network, got: {text}"
        );
    }

    #[tokio::test]
    async fn ripley_name_routes_to_ripley_handler() {
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("chat-list".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
    }

    #[tokio::test]
    async fn unknown_tool_still_reports_unknown() {
        let router = unauthed_router();
        let res = router
            .call_tool("definitely-not-a-tool", Map::new())
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Unknown tool"), "got: {text}");
    }

    #[test]
    fn dashboard_and_howto_tools_are_advertised() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(
            names.contains(&"dashboard"),
            "dashboard tool must be advertised"
        );
        assert!(names.contains(&"howto"), "howto tool must be advertised");
    }

    #[tokio::test]
    async fn dashboard_get_reaches_auth_gate() {
        // An unauthenticated dashboard call short-circuits at require_auth inside
        // handle_dashboard; reaching that (vs the unknown-tool arm) proves the
        // `dashboard` name routed to its handler.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("get".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        let res = router
            .call_tool("dashboard", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
        assert!(!text.contains("Unknown tool"), "got: {text}");
    }

    #[tokio::test]
    async fn howto_name_and_hyphen_alias_route_to_handler() {
        // Both `howto` and the `how-to` alias must route to handle_howto and hit
        // the auth gate, not the unknown-tool arm.
        for name in ["howto", "how-to"] {
            let router = unauthed_router();
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String("ask".to_owned()));
            args.insert(
                "question".to_owned(),
                Value::String("How do I create a share?".to_owned()),
            );
            let res = router.call_tool(name, args).await.expect("call_tool ok");
            let text = result_to_string(&res);
            assert!(
                text.contains("Not authenticated"),
                "tool '{name}' should reach handle_howto auth gate, got: {text}"
            );
            assert!(
                !text.contains("Unknown tool"),
                "tool '{name}' must not fall through to the unknown-tool arm, got: {text}"
            );
        }
    }

    #[test]
    fn sign_tool_advertises_my_sign_link_action() {
        let actions = sign_tool_actions();
        assert!(
            actions.contains(&"envelope-my-sign-link"),
            "sign tool must advertise envelope-my-sign-link, got: {actions:?}"
        );
    }

    #[test]
    fn apps_tool_advertises_only_list_and_never_the_dead_actions() {
        // `details` / `launch` / `get-tool-apps` called a `/apps/` prefix that
        // has never existed on the server — every call returned 9992 — and an
        // exhaustive sweep of the deployed tree found NO route for any of them.
        // They are unimplementable, not merely unimplemented: re-advertising one
        // would put back a permanently broken action.
        let actions = super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "apps")
            .expect("apps tool registered")
            .actions
            .to_vec();
        // `list` plus the universal `describe` (declared on every tool as of
        // 0.2.72) — and nothing else. The point of this lock is the dead-action
        // guard below, which is unchanged.
        assert_eq!(actions, vec!["list", "describe"], "got: {actions:?}");
        for dead in ["details", "launch", "get-tool-apps"] {
            assert!(
                !actions.contains(&dead),
                "apps must not re-advertise {dead:?}: no deployed route exists"
            );
        }
    }

    #[tokio::test]
    async fn apps_list_reaches_auth_gate_not_unknown_action() {
        // Two things at once: `list` still routes, AND it now requires auth.
        // `/user/apps/` is the caller's own installed-app list and rejects an
        // anonymous request; the old (non-existent) `/apps/list/` was called
        // unauthenticated, so hitting the auth gate is itself the proof the
        // re-point landed.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        let res = router.call_tool("apps", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
        assert!(!text.contains("Unknown tool"), "got: {text}");
        assert!(!text.contains("Unknown apps action"), "got: {text}");
    }

    #[tokio::test]
    async fn id_describe_needs_no_auth_and_lists_actions() {
        // The `id` tool is fully offline — describe works unauthenticated.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let res = router.call_tool("id", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("info"),
            "describe should list the info action: {text}"
        );
        assert!(
            text.contains("Offline"),
            "describe should note it is offline: {text}"
        );
    }

    #[tokio::test]
    async fn id_info_classifies_single_and_multiple_ids_offline() {
        // No auth, no network: a workflow id, a node id, and a comma-separated
        // `ids` string all classify locally.
        let router = unauthed_router();

        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("info".to_owned()));
        args.insert(
            "id".to_owned(),
            Value::String("wa3jm5zqzfxpxdr2dx8z5bvnb3rpjf".to_owned()),
        );
        let text = result_to_string(&router.call_tool("id", args).await.expect("ok"));
        assert!(text.contains("WorkflowStepOccurrence"), "got: {text}");

        // `ids` as a comma-separated string (the schema advertises strings).
        let mut multi = Map::new();
        multi.insert("action".to_owned(), Value::String("info".to_owned()));
        multi.insert(
            "ids".to_owned(),
            Value::String("2yxh5ojakxr3mwzty6tvk66cjnqsw, 3867689418901071163".to_owned()),
        );
        let text = result_to_string(&router.call_tool("id", multi).await.expect("ok"));
        assert!(text.contains("StorageNode"), "got: {text}");
        assert!(text.contains("19-digit numeric profile id"), "got: {text}");
    }

    #[tokio::test]
    async fn id_info_without_id_or_ids_errors() {
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("info".to_owned()));
        let text = result_to_string(&router.call_tool("id", args).await.expect("ok"));
        assert!(text.contains("provide `id`"), "got: {text}");
    }

    #[test]
    fn mcp_search_success_normalizes_files_map_to_array_rows() {
        // The MCP flat-search handler (`handle_files_search`) emits
        // `success_json(&normalize_search_response(v))`. This reproduces that
        // exact composition on a node-id-keyed `files` MAP and asserts the
        // rendered markdown is a one-row-per-file table (CLI/MCP parity), not a
        // node-id-keyed object dump.
        let raw = json!({
            "result": true,
            "files": {
                "f1": {"name": "File 1", "type": "file"},
                "f2": {"name": "File 2", "type": "file"}
            }
        });
        let normalized = super::api::storage::normalize_search_response(raw);
        let result = super::success_json(&normalized);
        let text = result_to_string(&result);
        // A record-shaped array renders as a GFM pipe table with a `node_id`
        // column header and one cell per file id — the array shape, not the
        // map shape.
        assert!(
            text.contains("node_id"),
            "normalized search output should render a node_id table column, got: {text}"
        );
        assert!(
            text.contains("f1") && text.contains("f2"),
            "both file ids should appear as row values, got: {text}"
        );
        assert!(
            text.contains("File 1") && text.contains("File 2"),
            "both file names should appear in the table, got: {text}"
        );
    }

    #[test]
    fn ripley_tool_description_leads_with_offload_framing() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let ripley = tools
            .iter()
            .find(|t| t.name.as_ref() == "ripley")
            .expect("ripley tool present");
        let desc = ripley.description.as_deref().unwrap_or_default();
        assert!(
            desc.starts_with("Offload"),
            "ripley description should lead with offload framing, got: {desc}"
        );
    }

    #[test]
    fn share_form_helper_builds_files_json_array() {
        // The MCP share-generate handler delegates to this shared builder;
        // confirm the body shape it produces is the JSON `files` array.
        let form = super::api::ai::build_share_form(&["x".to_owned(), "y".to_owned()]);
        assert_eq!(form.get("files").map(String::as_str), Some(r#"["x","y"]"#));
        assert!(!form.contains_key("nodes"));
        let _ = json!({}); // silence unused import if other asserts change
    }

    #[tokio::test]
    async fn ai_chat_create_without_question_errors() {
        // The API docs make `question` required for create; the handler must
        // reject a create that omits `query_text` rather than send a
        // question-less body.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("chat-create".to_owned()));
        args.insert("context_id".to_owned(), Value::String("ws1".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("query_text"),
            "create without question must surface a missing query_text error, got: {text}"
        );
    }

    #[tokio::test]
    async fn ai_share_generate_rejects_more_than_25_files() {
        // The API docs cap `files` at 1-25; the handler must reject >25
        // client-side before the network round-trip.
        let router = authed_router().await;
        let ids: Vec<String> = (0..26).map(|i| format!("id{i}")).collect();
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("share-generate".to_owned()),
        );
        args.insert("context_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("files".to_owned(), Value::String(ids.join(",")));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("too many files") && text.contains("25"),
            "share-generate with 26 files must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn ai_share_generate_accepts_json_array_files() {
        // `files` may be a JSON-array string; confirm it parses (a single id
        // is well under the cap, so this passes validation and proceeds to the
        // network attempt — which then fails, but NOT with a parse/validation
        // error).
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("share-generate".to_owned()),
        );
        args.insert("context_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert(
            "files".to_owned(),
            Value::String(r#"["abc","def"]"#.to_owned()),
        );
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("failed to parse") && !text.contains("at least one file ID"),
            "JSON-array files should parse cleanly, got: {text}"
        );
    }

    #[tokio::test]
    async fn auth_api_key_update_rejects_empty_update() {
        // Parity with the CLI `api-key update` guard: an update with no mutable
        // field (name/scopes/agent_name/expires) is rejected client-side before
        // the wire call, rather than forwarding an empty form to the server.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("api-key-update".to_owned()),
        );
        args.insert("key_id".to_owned(), Value::String("key-123".to_owned()));
        let res = router.call_tool("auth", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("at least one update field is required"),
            "empty api-key-update must be rejected client-side, got: {text}"
        );
    }

    #[test]
    fn ripley_tool_advertises_phase2_actions() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let ripley = tools
            .iter()
            .find(|t| t.name.as_ref() == "ripley")
            .expect("ripley tool present");
        let schema = serde_json::to_string(&ripley.input_schema).unwrap_or_default();
        for action in ["ask", "share-generate"] {
            assert!(
                schema.contains(action),
                "ripley schema must advertise the `{action}` action, got: {schema}"
            );
        }
        // The retired self-only AI-memory actions must NOT be advertised —
        // agent memory was removed from public API access.
        for retired in ["memory-get", "memory-set", "memory-delete"] {
            assert!(
                !schema.contains(retired),
                "ripley schema must NOT advertise retired memory action `{retired}`"
            );
        }
        // `search` was retired from `ripley` with the rest of the `/ai/search/`
        // surface. Assert against the ADVERTISED ACTION LIST rather than the
        // serialized schema: the word "search" may legitimately reappear in a
        // future parameter description, and a substring check would then fail
        // for the wrong reason (or, worse, pass while the action came back
        // under a reworded description).
        let actions = super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "ripley")
            .expect("ripley tool registered")
            .actions;
        assert!(
            !actions.contains(&"search"),
            "ripley must NOT advertise `search` — searching moved to the \
             dedicated `search` tool, got: {actions:?}"
        );
    }

    #[test]
    fn metadata_tool_advertises_extract_and_wait_action() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let metadata = tools
            .iter()
            .find(|t| t.name.as_ref() == "metadata")
            .expect("metadata tool present");
        let schema = serde_json::to_string(&metadata.input_schema).unwrap_or_default();
        for action in ["extract", "extract-and-wait"] {
            assert!(
                schema.contains(action),
                "metadata schema must advertise `{action}`, got: {schema}"
            );
        }
        // NEGATIVE arm, added 2026-08-27. This test previously asserted
        // `auto-match` / `extract-all` were advertised and KEPT PASSING after
        // both were removed — `schema.contains` is a SUBSTRING check, and a
        // stale sentence in the `confirm_ai_spend` description still named
        // them. A green suite was asserting the presence of actions that no
        // longer existed. Pinning their ABSENCE is what actually catches that.
        for gone_param in ["batch_size", "force"] {
            assert!(
                !schema.contains(gone_param),
                "param `{gone_param}` belonged to a removed action and must not be advertised, got: {schema}"
            );
        }
        for gone in ["auto-match", "extract-all"] {
            assert!(
                !schema.contains(gone),
                "removed action `{gone}` must not be named anywhere in the metadata schema, got: {schema}"
            );
        }
        // New params surfaced. `batch_size` (auto-match) and `force`
        // (extract-all) left with their actions on 2026-08-27 and are asserted
        // ABSENT below — they were still advertised after the actions were
        // removed, which is dead surface an agent can pass and have silently
        // ignored.
        assert!(
            schema.contains("poll_interval"),
            "metadata schema must advertise param `poll_interval`, got: {schema}"
        );
        // Credit-spend side-effects flagged in the description.
        assert!(
            metadata
                .description
                .as_deref()
                .unwrap_or_default()
                .contains("SPEND AI CREDITS"),
            "metadata description must flag credit-spending actions"
        );
    }

    #[test]
    fn workspace_tool_advertises_metadata_extract_and_wait_action() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let workspace = tools
            .iter()
            .find(|t| t.name.as_ref() == "workspace")
            .expect("workspace tool present");
        let schema = serde_json::to_string(&workspace.input_schema).unwrap_or_default();
        for action in ["metadata-extract", "metadata-extract-and-wait"] {
            assert!(
                schema.contains(action),
                "workspace schema must advertise `{action}`, got: {schema}"
            );
        }
        assert!(
            workspace
                .description
                .as_deref()
                .unwrap_or_default()
                .contains("SPEND AI CREDITS"),
            "workspace description must flag credit-spending metadata actions"
        );
    }

    /// The metadata TEMPLATE and SAVED-VIEW surface was removed, but the tool
    /// DESCRIPTIONS and dead PARAMS kept advertising it — a caller reading the
    /// schema could still be told to pass `sort_field`, or that
    /// `metadata-template-suggest-fields` spends credits. Nothing failed,
    /// because every surviving assertion was a `contains` on something else.
    ///
    /// This pins the ABSENCE of each retired name across BOTH surfaces a
    /// client actually reads: the `input_schema` (where param names and their
    /// descriptions land) and the tool `description` string (which is NOT part
    /// of the schema — the two retired credit-spend actions were named there,
    /// so a schema-only check would have missed them).
    ///
    /// Scoped to the `metadata` and `workspace` tools deliberately:
    /// `sort_field` / `sort_dir` are live, legitimate params on the `files`
    /// and `storage` tools, so a repo-wide assertion would be false.
    #[test]
    fn retired_template_and_view_names_are_not_advertised() {
        /// Names that belonged to the removed template/saved-view surface.
        /// Adding a name here must accompany its removal, never precede it.
        const RETIRED: &[&str] = &[
            "add-nodes",
            "remove-nodes",
            "list-nodes",
            "metadata-template-preview-match",
            "metadata-template-suggest-fields",
            "sort_field",
            "sort_dir",
        ];

        let tools = ToolRouter::list_tools_with(true).tools;
        for tool_name in ["metadata", "workspace"] {
            let tool = tools
                .iter()
                .find(|t| t.name.as_ref() == tool_name)
                .expect("tool present");
            let schema = serde_json::to_string(&tool.input_schema).unwrap_or_default();
            let description = tool.description.as_deref().unwrap_or_default().to_owned();

            // Positive control: without this, a schema that failed to
            // serialize (or a tool whose params vanished entirely) would
            // satisfy every negative assertion below and pass vacuously.
            assert!(
                schema.contains("workspace_id"),
                "{tool_name} schema did not serialize as expected, so the \
                 negative assertions below would pass vacuously: {schema}"
            );

            for retired in RETIRED {
                assert!(
                    !schema.contains(retired),
                    "`{retired}` belongs to the removed metadata template/view \
                     surface and must not appear in the {tool_name} schema: {schema}"
                );
                assert!(
                    !description.contains(retired),
                    "`{retired}` belongs to the removed metadata template/view \
                     surface and must not appear in the {tool_name} description: {description}"
                );
            }
        }
    }

    #[tokio::test]
    async fn metadata_extract_and_wait_routes_to_handler() {
        // Unauthenticated → short-circuits at the auth gate inside
        // handle_metadata, proving `extract-and-wait` routed (vs the
        // unknown-action arm).
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("extract-and-wait".to_owned()),
        );
        let res = router
            .call_tool("metadata", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
        assert!(!text.contains("Unknown metadata action"), "got: {text}");
    }

    // ─── Metadata surface lock ──────────────────────────────────────────────
    //
    // These five tests (four here, one in `cli.rs`) pin the metadata surface
    // as it exists TODAY, so any later addition or removal has to be made
    // deliberately and shows up in review instead of drifting in silently.

    /// Every action the `metadata` tool advertises today.
    // `describe` is the UNIVERSAL action every tool declares as of 0.2.72 — it is
    // part of the locked surface, not a drift, and its absence would now be the
    // regression.
    const METADATA_TOOL_SURFACE: &[&str] = &[
        "eligible",
        "extract",
        "extract-and-wait",
        "search",
        "describe",
    ];

    /// Every `metadata-*` action the `workspace` tool advertises today. The
    /// tool carries many more actions; only this prefix is part of the lock.
    const WORKSPACE_METADATA_SURFACE: &[&str] = &[
        "metadata-delete",
        "metadata-details",
        "metadata-extract",
        "metadata-extract-and-wait",
    ];

    /// Metadata actions that have been RETIRED from the `metadata` tool.
    ///
    /// When an action is removed from `TOOL_DEFS` it MUST be added here in the
    /// same change. `dead_actions_are_really_gone` then proves the handler arm
    /// was deleted too, rather than merely unadvertised — the one drift
    /// direction `metadata_surface_actions_all_dispatch` cannot see.
    ///
    /// Populated 2026-08-26 by the saved-view removal: saved
    /// views were removed rather than reworded, because they described a
    /// workspace-shared layout as "the caller's" and have no successor
    /// concept. Do not populate these with invented action names to make the
    /// test look busy — every entry must be an action that really shipped.
    const DEAD_METADATA_ACTIONS: &[&str] = &[
        // Saved-view export — removed 2026-08-26.
        "export-view",
        // Template-scoped actions — removed 2026-08-27 by product decision.
        "add-nodes",
        "remove-nodes",
        "list-nodes",
        "auto-match",
        "extract-all",
    ];

    /// Retired `metadata-*` actions of the `workspace` tool. Same contract as
    /// [`DEAD_METADATA_ACTIONS`].
    const DEAD_WORKSPACE_ACTIONS: &[&str] = &[
        // Saved views — removed 2026-08-26.
        "metadata-view-save",
        "metadata-view-get",
        "metadata-view-delete",
        "metadata-views-list",
        // Templates — removed 2026-08-27 by product decision: the interfaces
        // retire now and the dataset is cleaned up afterwards.
        "metadata-template-create",
        "metadata-template-preview-match",
        "metadata-template-suggest-fields",
        "metadata-template-delete",
        "metadata-template-list",
        "metadata-template-details",
        "metadata-template-settings",
        "metadata-template-update",
        "metadata-templates-in-use",
        // Retired 2026-08-27. BOTH routed to
        // a `{template_id}` path segment, and templates are gone — so after the
        // removal above there was no longer any way for a caller to obtain a
        // valid id to pass. `metadata-update` additionally POSTed
        // `storage/{node}/metadata/update/{template_id}/`, which the platform
        // has RETIRED (410 / 191138, reported by the platform — relayed, not
        // measured here). Its successor is
        // `POST storage/{node}/metadata/facts/` with a JSON `facts` object;
        // that is a NEW write surface, deliberately NOT added here.
        "metadata-list",
        "metadata-update",
    ];

    /// Retired actions of the `ripley` tool. Same contract as
    /// [`DEAD_METADATA_ACTIONS`].
    const DEAD_RIPLEY_ACTIONS: &[&str] = &[
        // `search` — removed 2026-08-29 with the rest of the `/ai/search/`
        // surface. The backend deleted `GET /{workspace,share}/{id}/ai/search/`
        // on 2026-08-28 (sunset 2026-06-01), and search is not a Ripley verb:
        // the successor is the dedicated `search` tool (unified `/search/`),
        // which covers BOTH the workspace and share contexts this action used
        // to serve.
        "search",
    ];

    fn metadata_tool_actions() -> Vec<&'static str> {
        super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "metadata")
            .expect("metadata tool registered")
            .actions
            .to_vec()
    }

    fn workspace_metadata_actions() -> Vec<&'static str> {
        super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "workspace")
            .expect("workspace tool registered")
            .actions
            .iter()
            .copied()
            .filter(|action| action.starts_with("metadata-"))
            .collect()
    }

    /// Assert an advertised action list matches `expected` EXACTLY — nothing
    /// missing, nothing extra, no duplicates — naming the difference so a
    /// surface change fails with an actionable diff, not a bare count.
    fn assert_actions_exact(label: &str, actual: &[&str], expected: &[&str]) {
        use std::collections::BTreeSet;

        let actual_set: BTreeSet<&str> = actual.iter().copied().collect();
        let expected_set: BTreeSet<&str> = expected.iter().copied().collect();
        let missing: Vec<&str> = expected_set.difference(&actual_set).copied().collect();
        let extra: Vec<&str> = actual_set.difference(&expected_set).copied().collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{label} surface changed — missing: {missing:?}, unexpected: {extra:?}"
        );
        assert_eq!(
            actual.len(),
            actual_set.len(),
            "{label} advertises a duplicate action: {actual:?}"
        );
        assert_eq!(
            expected.len(),
            expected_set.len(),
            "{label} expectation list contains a duplicate"
        );
    }

    #[test]
    fn metadata_tool_surface_is_locked() {
        assert_actions_exact(
            "`metadata` tool",
            &metadata_tool_actions(),
            METADATA_TOOL_SURFACE,
        );
    }

    #[test]
    fn workspace_metadata_surface_is_locked() {
        assert_actions_exact(
            "`workspace` tool `metadata-*`",
            &workspace_metadata_actions(),
            WORKSPACE_METADATA_SURFACE,
        );
    }

    /// Dispatch `action` on `tool` through the real `call_tool` path with no
    /// other arguments, returning the rendered response text.
    async fn dispatch_action(router: &ToolRouter, tool: &str, action: &str) -> String {
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String(action.to_owned()));
        let res = router
            .call_tool(tool, args)
            .await
            .expect("call_tool should not error");
        result_to_string(&res)
    }

    /// The `files` tool advertises BOTH extracted-text actions and both really
    /// dispatch.
    ///
    /// Same drift hazard `metadata_surface_actions_all_dispatch` documents:
    /// `call_tool` never checks `action` against `TOOL_DEFS`, so an advertised
    /// action with no handler arm answers "Unknown files action" at runtime
    /// while looking perfectly healthy in the schema. The negative control at
    /// the end keeps the assertion from passing vacuously.
    #[tokio::test]
    async fn files_content_actions_are_advertised_and_dispatch() {
        let actions = super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "files")
            .expect("files tool registered")
            .actions;
        for action in ["content", "content-many"] {
            assert!(
                actions.contains(&action),
                "`files` must advertise `{action}`, got: {actions:?}"
            );
        }

        let router = authed_router().await;
        for action in ["content", "content-many"] {
            let text = dispatch_action(&router, "files", action).await;
            assert!(
                !text.contains("Unknown files action"),
                "`files` advertises `{action}` but it does not dispatch: {text}"
            );
        }
        let text = dispatch_action(&router, "files", "no-such-action").await;
        assert!(
            text.contains("Unknown files action"),
            "reachability lock is vacuous — an unknown `files` action must reach \
             the fallback arm, got: {text}"
        );
    }

    /// `content-many` refuses a `share_id` with an EXPLANATION rather than
    /// quietly answering from a workspace.
    ///
    /// `share_id` is also this tool's `add-link` parameter, so an agent reusing
    /// an argument map from an earlier step is the realistic way this gets sent
    /// — and there is no share form of the route to fall back on.
    #[tokio::test]
    async fn files_content_many_rejects_a_share_id() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("content-many".to_owned()),
        );
        args.insert("share_id".to_owned(), Value::String("s1".to_owned()));
        args.insert("nodes".to_owned(), Value::String("a,b".to_owned()));
        args.insert("query".to_owned(), Value::String("q".to_owned()));
        let text = result_to_string(&router.call_tool("files", args).await.expect("call_tool ok"));
        assert!(
            text.contains("WORKSPACE ONLY"),
            "a share_id on content-many must be refused by name, got: {text}"
        );
    }

    /// A bad VALUE is rejected by the shared `ContentReadParams::validate`, not
    /// by a second MCP-only rule, and the wire parameter is named in the
    /// message so an agent knows which argument to change.
    #[tokio::test]
    async fn files_content_surfaces_the_shared_window_validator() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("content".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("123".to_owned()));
        args.insert("node_id".to_owned(), Value::String("n1".to_owned()));
        args.insert("query".to_owned(), Value::String("q".to_owned()));
        args.insert("page".to_owned(), Value::from(2));
        let text = result_to_string(&router.call_tool("files", args).await.expect("call_tool ok"));
        assert!(
            text.contains("at most one content window"),
            "two window selectors must be refused by the shared validator, got: {text}"
        );
    }

    /// Every tool that gained a parameter here declares each NAME exactly once.
    ///
    /// `action_schema` inserts params into a map keyed by name, so a second
    /// entry for an existing name silently OVERWRITES the first and the loser
    /// never ships — a failure with no compiler error and no runtime symptom
    /// beyond an agent reading the wrong description. Asserted over the whole
    /// registry, not just the four tools touched, because the hazard is
    /// structural rather than specific to this change.
    #[test]
    fn no_tool_declares_a_parameter_name_twice() {
        use std::collections::BTreeSet;
        for def in super::TOOL_DEFS {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for (name, _, _) in def.params {
                assert!(
                    seen.insert(name),
                    "tool `{}` declares parameter `{name}` more than once — the \
                     later entry silently overwrites the earlier one",
                    def.name
                );
            }
        }
    }

    /// The new parameters are actually advertised on the tools that accept
    /// them — the half `no_tool_declares_a_parameter_name_twice` cannot see.
    #[test]
    fn new_content_parameters_are_advertised() {
        let params_of = |tool: &str| -> Vec<&str> {
            super::TOOL_DEFS
                .iter()
                .find(|d| d.name == tool)
                .unwrap_or_else(|| panic!("{tool} tool registered"))
                .params
                .iter()
                .map(|(name, _, _)| *name)
                .collect()
        };
        let files = params_of("files");
        for name in ["nodes", "page", "chunk_from", "chunk_to", "max_bytes"] {
            assert!(files.contains(&name), "`files` must advertise `{name}`");
        }
        assert!(
            params_of("search").contains(&"details"),
            "the unified `search` tool must advertise `details`"
        );
        for tool in ["workspace", "org"] {
            assert!(
                params_of(tool).contains(&"metadata_extraction"),
                "`{tool}` must advertise `metadata_extraction`"
            );
        }
    }

    /// The MCP workspace update forwards `metadata_extraction` as the string
    /// `"true"`/`"false"`, and OMITS it when unset — the opt-out contract: an
    /// absent field must leave the server's default of on in place, never send
    /// `false` on the caller's behalf.
    #[test]
    fn mcp_workspace_update_forwards_metadata_extraction() {
        let mut args = Map::new();
        args.insert("metadata_extraction".to_owned(), Value::Bool(false));
        let fields = super::build_workspace_update_fields(&args);
        assert_eq!(
            fields.get("metadata_extraction").map(String::as_str),
            Some("false")
        );
        assert!(
            !super::build_workspace_update_fields(&Map::new()).contains_key("metadata_extraction"),
            "unset `metadata_extraction` must be omitted from the update form"
        );
    }

    #[tokio::test]
    async fn metadata_surface_actions_all_dispatch() {
        // WHY THIS TEST EXISTS: `call_tool` dispatches on the tool NAME and
        // hands `action` straight to the handler — it never validates the
        // action against `TOOL_DEFS`. The advertised list and the dispatchable
        // match arms are two independently maintained lists with no compiler
        // or runtime link between them, so they can drift in EITHER direction:
        //
        //   advertised ⊆ dispatchable — an advertised action with no handler
        //     arm, i.e. a `TOOL_DEFS` entry that answers "Unknown ... action".
        //     THIS TEST COVERS THAT DIRECTION, by walking every advertised
        //     action through the real dispatcher.
        //
        //   dispatchable ⊆ advertised — a handler arm left behind after its
        //     `TOOL_DEFS` entry was removed, i.e. an action that is hidden but
        //     still fully callable. THIS TEST DOES NOT COVER THAT: it only
        //     ever reads actions out of `TOOL_DEFS`, so a retired action
        //     simply stops being visited and every assertion here still
        //     passes. `dead_actions_are_really_gone` is the lock for that
        //     direction, via the `DEAD_*_ACTIONS` denylists.
        //
        // The router must be AUTHENTICATED. `handle_metadata` and
        // `handle_workspace` both run `require_auth` BEFORE `match action`, so
        // on an unauthenticated router even a nonsense action never reaches
        // the `_ =>` arm — the assertions below would hold vacuously. The
        // negative controls at the end prove they do not.
        let router = authed_router().await;

        for action in metadata_tool_actions() {
            let text = dispatch_action(&router, "metadata", action).await;
            assert!(
                !text.contains("Unknown metadata action"),
                "`metadata` advertises `{action}` but it does not dispatch: {text}"
            );
        }

        for action in workspace_metadata_actions() {
            let text = dispatch_action(&router, "workspace", action).await;
            assert!(
                !text.contains("Unknown workspace action"),
                "`workspace` advertises `{action}` but it does not dispatch: {text}"
            );
        }

        // Negative controls. Do NOT drop these: without them the loops above
        // still pass if the unknown-action arms ever become unreachable.
        let text = dispatch_action(&router, "metadata", "no-such-action").await;
        assert!(
            text.contains("Unknown metadata action"),
            "reachability lock is vacuous — an unknown `metadata` action must \
             reach the fallback arm, got: {text}"
        );
        let text = dispatch_action(&router, "workspace", "metadata-no-such-action").await;
        assert!(
            text.contains("Unknown workspace action"),
            "reachability lock is vacuous — an unknown `workspace` action must \
             reach the fallback arm, got: {text}"
        );
    }

    #[tokio::test]
    async fn dead_actions_are_really_gone() {
        // The inverse of `metadata_surface_actions_all_dispatch`: that test
        // reads its actions out of `TOOL_DEFS`, so removing an entry there
        // makes the action invisible to it, handler arm and all. Every retired
        // action is therefore listed in `DEAD_*_ACTIONS` and dispatched here,
        // which fails unless the arm is really gone.
        //
        // Both lists are now POPULATED (the retired template/saved-view
        // actions, plus `metadata-update` / `metadata-list`), so this test has
        // real assertions to make. The negative controls in
        // `metadata_surface_actions_all_dispatch` cover the harness.
        let router = authed_router().await;

        for action in DEAD_METADATA_ACTIONS.iter().copied() {
            let text = dispatch_action(&router, "metadata", action).await;
            assert!(
                text.contains("Unknown metadata action"),
                "`{action}` was retired from the `metadata` tool but its handler \
                 arm still dispatches — hidden but callable: {text}"
            );
        }

        for action in DEAD_WORKSPACE_ACTIONS.iter().copied() {
            let text = dispatch_action(&router, "workspace", action).await;
            assert!(
                text.contains("Unknown workspace action"),
                "`{action}` was retired from the `workspace` tool but its handler \
                 arm still dispatches — hidden but callable: {text}"
            );
        }

        // The `ai` alias routes to the same handler, so check BOTH spellings —
        // a retired action that is unreachable as `ripley` but still callable
        // as `ai` is exactly as alive as one that never moved.
        for action in DEAD_RIPLEY_ACTIONS.iter().copied() {
            for tool in ["ripley", "ai"] {
                let text = dispatch_action(&router, tool, action).await;
                assert!(
                    text.contains("Unknown ripley action"),
                    "`{action}` was retired from the `ripley` tool but its handler \
                     arm still dispatches via `{tool}` — hidden but callable: {text}"
                );
            }
        }
    }

    // ─── MCP metadata-details render lock ───────────────────────────────────
    //
    // WHY THESE EXIST: the declared-type strip is called at two places in the
    // `metadata-details` arm (single and bulk). Deleting either left the whole
    // suite green — the helper is well covered, the WIRING was not. These two
    // drive the real `call_tool` path against a loopback stub of the documented
    // details envelope and assert the RENDERED response.
    //
    // The stub binds `127.0.0.1:0`. Nothing here talks to a live API.

    /// The template-DECLARED type in the stub payloads — must not be rendered.
    const STUB_DECLARED_TYPE: &str = "float";
    /// The STORED value, which disagrees with the declared type. Asserting it
    /// IS rendered is the positive control that keeps these tests honest.
    const STUB_STORED_VALUE: &str = "FIRST-NOT-A-NUMBER";
    /// The STORED value of the SECOND object in the bulk fixture. It must not
    /// be a substring of (or contain) [`STUB_STORED_VALUE`]: if one contained
    /// the other, the bulk positive control would be satisfied by a single
    /// object and would pass with the other one dropped entirely.
    const STUB_STORED_VALUE_2: &str = "SECOND-NOPE";

    /// One metadata entry in the documented shape
    /// `{key, description, type, value, is_auto, updated}`.
    fn stub_entry(value: &str) -> Value {
        serde_json::json!({
            "key": "invoice_total",
            "description": "Invoice total",
            "type": STUB_DECLARED_TYPE,
            "value": value,
            "is_auto": false,
            "updated": "2026-01-28 12:30:00 UTC"
        })
    }

    /// The single-node `GET …/metadata/details/` envelope.
    fn stub_single_details_body() -> String {
        serde_json::json!({
            "result": "yes",
            "response": {
                "object_id": "abc123",
                "node_id": {"id": "abc123", "name": "invoice.pdf", "type": "file"},
                "template_metadata": [stub_entry(STUB_STORED_VALUE)],
                "custom_metadata": []
            }
        })
        .to_string()
    }

    /// The bulk (`format: "multi"`) envelope for two node ids.
    fn stub_bulk_details_body() -> String {
        serde_json::json!({
            "result": "yes",
            "response": {
                "format": "multi",
                "objects": [
                    {
                        "object_id": "abc123",
                        "node_id": {"id": "abc123", "name": "invoice.pdf", "type": "file"},
                        "template_metadata": [stub_entry(STUB_STORED_VALUE)],
                        "custom_metadata": []
                    },
                    {
                        "object_id": "def456",
                        "node_id": {"id": "def456", "name": "invoice2.pdf", "type": "file"},
                        "template_metadata": [stub_entry(STUB_STORED_VALUE_2)],
                        "custom_metadata": []
                    }
                ],
                "templates": {},
                "errors": []
            }
        })
        .to_string()
    }

    /// Serve `body` as a single `200 OK` JSON response on a fresh loopback
    /// port, then close. Same one-shot idiom as the client unit tests
    /// (`src/client.rs::spawn_one_shot_server`). Returns `127.0.0.1:<port>`.
    async fn spawn_details_stub(body: String) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Drain the request headers (read once; enough for a GET).
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        addr
    }

    /// An authenticated router pointed at `addr`, so a metadata read really
    /// completes and its rendered response can be asserted. The sibling
    /// `authed_router` points at a dead port instead, so the arms that do
    /// attempt a connection there fail locally rather than answering.
    async fn stubbed_authed_router(addr: &str) -> ToolRouter {
        let state = Arc::new(McpState::new_unauthenticated_for_test(&format!(
            "http://{addr}"
        )));
        state.set_token("test-token".to_owned()).await;
        ToolRouter::new_with_esign(state, true)
    }

    /// Dispatch `workspace/metadata-details` with `id_key = id_value`.
    async fn dispatch_details(router: &ToolRouter, id_key: &str, id_value: &str) -> String {
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("metadata-details".to_owned()),
        );
        args.insert(
            "workspace_id".to_owned(),
            Value::String("4687730903718774523".to_owned()),
        );
        args.insert(id_key.to_owned(), Value::String(id_value.to_owned()));
        let res = router
            .call_tool("workspace", args)
            .await
            .expect("call_tool should not error");
        result_to_string(&res)
    }

    #[tokio::test]
    async fn mcp_metadata_details_single_drops_the_declared_type() {
        let addr = spawn_details_stub(stub_single_details_body()).await;
        let router = stubbed_authed_router(&addr).await;
        let text = dispatch_details(&router, "node_id", "abc123").await;

        assert!(
            text.contains(STUB_STORED_VALUE),
            "the stored value must be rendered, got: {text}"
        );
        assert!(
            !text.contains(STUB_DECLARED_TYPE),
            "MCP renders markdown to an agent, so the declared type \
             {STUB_DECLARED_TYPE:?} must be stripped, got: {text}"
        );
    }

    #[tokio::test]
    async fn mcp_metadata_details_bulk_drops_the_declared_type() {
        let addr = spawn_details_stub(stub_bulk_details_body()).await;
        let router = stubbed_authed_router(&addr).await;
        let text = dispatch_details(&router, "node_ids", "abc123,def456").await;

        for value in [STUB_STORED_VALUE, STUB_STORED_VALUE_2] {
            assert!(
                text.contains(value),
                "every stored value must be rendered, got: {text}"
            );
        }
        assert!(
            !text.contains(STUB_DECLARED_TYPE),
            "the bulk arm must strip the declared type {STUB_DECLARED_TYPE:?} \
             from every object, got: {text}"
        );
    }

    #[test]
    fn workspace_tool_no_longer_advertises_saved_views() {
        // Saved views were removed (they described a workspace-shared layout as
        // "the caller's" and have no successor), so this test locks the ABSENCE
        // of the four saved-view actions and the `config` param. An agent
        // reading a stale schema would call an action that no longer dispatches.
        let tools = ToolRouter::list_tools_with(true).tools;
        let workspace = tools
            .iter()
            .find(|t| t.name.as_ref() == "workspace")
            .expect("workspace tool present");
        let schema = serde_json::to_string(&workspace.input_schema).unwrap_or_default();
        for action in [
            "metadata-view-save",
            "metadata-view-get",
            "metadata-view-delete",
            "metadata-views-list",
        ] {
            assert!(
                !schema.contains(action),
                "workspace schema must NOT advertise removed `{action}`, got: {schema}"
            );
        }
        assert!(
            !schema.contains("view_id"),
            "retired node-scoped `view_id` param must be gone from the workspace schema"
        );
    }

    /// The import tool must vanish from the advertised surface when the
    /// cloud-import kill-switch is off, and reappear when it is on — mirroring
    /// the E-Sign rule so advertised and callable stay in lockstep.
    #[test]
    fn import_tool_hidden_when_cloud_import_disabled() {
        let state = Arc::new(McpState::new_unauthenticated_for_test(
            "https://api.fast.io/current",
        ));
        let off = ToolRouter::new_with_cloud_import(Arc::clone(&state), false);
        assert!(
            !off.list_tools()
                .tools
                .iter()
                .any(|t| t.name.as_ref() == "import"),
            "import must not be advertised when cloud import is disabled"
        );
        let on = ToolRouter::new_with_cloud_import(state, true);
        assert!(
            on.list_tools()
                .tools
                .iter()
                .any(|t| t.name.as_ref() == "import"),
            "import must be advertised when cloud import is enabled"
        );
    }

    #[tokio::test]
    async fn import_call_is_refused_when_cloud_import_disabled() {
        // Callable surface must match the advertised one: dispatch is refused
        // BEFORE any auth/client work, so the message is the gate's, not "Not
        // authenticated".
        let router = ToolRouter::new_with_cloud_import(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            false,
        );
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list-drives".to_owned()));
        let res = router
            .call_tool("import", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Cloud import is not yet available"),
            "got: {text}"
        );
        assert!(text.contains("FASTIO_ENABLE_CLOUD_IMPORT=1"), "got: {text}");
        assert!(
            !text.contains("Not authenticated"),
            "gate must precede auth: {text}"
        );
    }

    #[test]
    fn import_tool_advertises_list_drives_and_drive_params() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let import = tools
            .iter()
            .find(|t| t.name.as_ref() == "import")
            .expect("import tool present");
        let schema = serde_json::to_string(&import.input_schema).unwrap_or_default();
        for advertised in ["list-drives", "refresh-drives", "drive_id", "site_path"] {
            assert!(
                schema.contains(advertised),
                "import schema must advertise `{advertised}`, got: {schema}"
            );
        }
    }

    #[tokio::test]
    async fn import_list_drives_routes_to_handler() {
        // The action must reach its handler rather than the unknown-action arm;
        // an unauthenticated router stops it at the auth check.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list-drives".to_owned()));
        let res = router
            .call_tool("import", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
        assert!(!text.contains("Unknown import action"), "got: {text}");
    }

    #[tokio::test]
    async fn import_refresh_drives_routes_to_handler() {
        // refresh-drives is the only call that talks to the provider; it must
        // reach its handler rather than the unknown-action arm.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("refresh-drives".to_owned()),
        );
        let res = router
            .call_tool("import", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
        assert!(!text.contains("Unknown import action"), "got: {text}");
    }

    #[tokio::test]
    async fn workspace_metadata_extract_and_wait_routes_to_handler() {
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("metadata-extract-and-wait".to_owned()),
        );
        let res = router
            .call_tool("workspace", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
        assert!(!text.contains("Unknown workspace action"), "got: {text}");
    }

    #[tokio::test]
    async fn memory_actions_are_retired() {
        // Agent memory was removed from public API access: the memory-* actions
        // must no longer route to a handler — they hit the unknown-action arm
        // even when authenticated.
        let router = authed_router().await;
        for action in ["memory-get", "memory-set", "memory-delete"] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String((*action).to_owned()));
            args.insert("context_type".to_owned(), Value::String("org".to_owned()));
            args.insert("context_id".to_owned(), Value::String("o1".to_owned()));
            let res = router
                .call_tool("ripley", args)
                .await
                .expect("call_tool ok");
            let text = result_to_string(&res);
            assert!(
                text.contains("Unknown ripley action"),
                "retired `{action}` must hit the unknown-action arm, got: {text}"
            );
        }
    }

    #[tokio::test]
    async fn ai_autotitle_rejects_workspace_context() {
        // autotitle is SHARE-ONLY. A workspace context must
        // be rejected pre-network rather than mis-routed.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("autotitle".to_owned()));
        args.insert(
            "context_type".to_owned(),
            Value::String("workspace".to_owned()),
        );
        args.insert("context_id".to_owned(), Value::String("ws1".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("share-only"),
            "autotitle with workspace context must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn ai_autotitle_share_delegates_not_rejected() {
        // A share context is valid for autotitle; it must NOT hit the share-only
        // rejection. The fake-token network attempt then fails, but the failure
        // is a network/auth error — not the pre-network context rejection.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("autotitle".to_owned()));
        args.insert("context_type".to_owned(), Value::String("share".to_owned()));
        args.insert("context_id".to_owned(), Value::String("sh1".to_owned()));
        args.insert("context".to_owned(), Value::String("focus hint".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("share-only"),
            "autotitle with a share context must reach delegation, got: {text}"
        );
    }

    #[tokio::test]
    async fn ai_transactions_rejects_share_context() {
        // transactions is WORKSPACE-ONLY. A share context must
        // be rejected pre-network rather than mis-routed.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("transactions".to_owned()),
        );
        args.insert("context_type".to_owned(), Value::String("share".to_owned()));
        args.insert("context_id".to_owned(), Value::String("sh1".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("workspace-only"),
            "transactions with share context must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn ai_chat_create_share_kind_emits_warning() {
        // `kind` is workspace-only; for a share it is dropped, and the handler
        // surfaces a one-line warning rather than swallowing it silently. The
        // network call fails on the fake token, but the chat-create warning is
        // only attached on a successful create — so instead assert the request
        // is NOT rejected for `kind` (lenient: no hard error mentioning kind).
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("chat-create".to_owned()));
        args.insert("context_type".to_owned(), Value::String("share".to_owned()));
        args.insert("context_id".to_owned(), Value::String("sh1".to_owned()));
        args.insert("query_text".to_owned(), Value::String("hi".to_owned()));
        args.insert("kind".to_owned(), Value::String("agent".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        // Lenient: a share+kind create must not hard-error about `kind`.
        assert!(
            !text.contains("kind is not allowed") && !text.contains("invalid kind"),
            "share+kind must be lenient (no hard kind error), got: {text}"
        );
    }

    #[test]
    fn ask_auth_expired_text_carries_recovery_ids() {
        // FIX 3: the MCP `ask` 401 path must surface chat_id + message_id and a
        // re-check hint so an agent can recover after re-authenticating.
        let text = super::ask_auth_expired_text("chat-42", "msg-7");
        assert!(text.contains("chat-42"), "must carry chat_id, got: {text}");
        assert!(text.contains("msg-7"), "must carry message_id, got: {text}");
        assert!(
            text.contains("message-details"),
            "must include a re-check hint, got: {text}"
        );
    }

    #[test]
    fn turn_status_normalisation_handles_string_and_numeric() {
        // Status normalisation must accept a string OR a numeric value. This
        // used to test the MCP-local `json_value_field_to_string`; that helper
        // was folded into `api::ai::turn_status` on 2026-08-31 so the CLI and
        // MCP wait loops share one implementation instead of two that have to be
        // kept matching. The PROPERTY under test is unchanged — only its owner.
        assert_eq!(
            fastio_cli::api::ai::turn_status(&json!({"status": "complete"})),
            "complete"
        );
        assert_eq!(fastio_cli::api::ai::turn_status(&json!({"status": 3})), "3");
        // …and via the legacy `state` key, which the MCP loop still tolerates.
        assert_eq!(fastio_cli::api::ai::turn_status(&json!({"state": 3})), "3");
        assert_eq!(fastio_cli::api::ai::turn_status(&json!({})), "");
    }

    #[test]
    fn attach_warning_appends_to_warnings_array() {
        // FIX 5: the lenient kind-drop note is surfaced under a `warnings` array.
        let mut payload = json!({"chat_id": "c1"});
        super::attach_warning(&mut payload, Some(super::KIND_SHARE_WARNING));
        let warnings = payload
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings array present");
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].as_str(),
            Some(super::KIND_SHARE_WARNING),
            "warning text must match the kind-share note"
        );
        // None is a no-op.
        let mut clean = json!({"chat_id": "c2"});
        super::attach_warning(&mut clean, None);
        assert!(clean.get("warnings").is_none());
    }

    /// Collect a payload's `warnings` array as plain strings.
    fn warnings_of(payload: &Value) -> Vec<String> {
        payload
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn search_filters_arg_absent_null_and_blank_are_none() {
        let mut args = Map::new();
        assert!(super::search_filters_arg(&args).expect("ok").is_none());
        args.insert("filters".to_owned(), Value::Null);
        assert!(super::search_filters_arg(&args).expect("ok").is_none());
        args.insert("filters".to_owned(), json!("   "));
        assert!(super::search_filters_arg(&args).expect("ok").is_none());
    }

    #[test]
    fn search_filters_arg_string_forwarded_verbatim() {
        // An already-encoded string must not be re-serialized.
        //
        // 🔑 THE WHITESPACE IS THE CONTROL, and the big integer alone is NOT.
        // `9007199254740993` fits in `i64`, so `serde_json` stores it exactly
        // and `Value::to_string()` reproduces it byte-for-byte — meaning an
        // implementation that DID parse-and-re-serialize would still pass a
        // fixture that only varied the number. The comment would have claimed
        // a property the body never executed.
        //
        // The significant whitespace below cannot survive a round trip:
        // `to_string()` always emits the compact form. So this fixture fails
        // for a re-serializing implementation and passes only for a verbatim
        // one — which is the property being asserted.
        let raw = r#"[ { "field": "n", "operator": "=", "value": 9007199254740993 } ]"#;
        let mut args = Map::new();
        args.insert("filters".to_owned(), json!(raw));
        assert_eq!(
            super::search_filters_arg(&args).expect("ok").as_deref(),
            Some(raw)
        );
    }

    #[test]
    fn search_filters_arg_accepts_a_real_array() {
        // The natural MCP spelling. `optional_str` would read this as ABSENT
        // and silently drop the predicate, which is the failure this exists to
        // prevent — so a real array must be accepted, not ignored.
        let mut args = Map::new();
        args.insert(
            "filters".to_owned(),
            json!([{"field": "category", "operator": "=", "value": "Legal"}]),
        );
        let out = super::search_filters_arg(&args).expect("ok").expect("some");
        let parsed: Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed[0]["field"], json!("category"));
    }

    #[test]
    fn search_filters_arg_rejects_wrong_types_loudly() {
        // A number, a bool, or an object is an error — NEVER a silent omission
        // that returns unfiltered results looking like filtered ones.
        for bad in [json!(7), json!(true), json!({"field": "a"})] {
            let mut args = Map::new();
            args.insert("filters".to_owned(), bad.clone());
            assert!(
                super::search_filters_arg(&args).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn search_filters_arg_empty_array_is_no_filter_not_an_error() {
        // The `filters` bullet list in the API docs defines `[]` as "no
        // filter at all". Matches the CLI twin
        // `json_array_arg_empty_array_is_no_filter_not_an_error`.
        //
        // This test covers the NATIVE ARRAY spelling ONLY. Its comment used to
        // claim it proved "all three input paths (CLI text, MCP array, MCP
        // encoded string) agree" while the body inserted a single `json!([])` —
        // the encoded-string path was NAMED as covered and never executed, and
        // that path was in fact broken. The encoded-string spelling now has its
        // own test below. A COMMENT IS NOT A CONTROL: an assertion about
        // coverage belongs in a test body, not in prose above one.
        let mut args = Map::new();
        args.insert("filters".to_owned(), json!([]));
        assert!(super::search_filters_arg(&args).expect("legal").is_none());
    }

    #[test]
    fn search_filters_arg_encoded_string_drops_empty_rejects_bad_and_forwards_verbatim() {
        // The third input spelling, previously unexercised. An encoded `"[]"`
        // must drop to None exactly like the native `[]` and the CLI's text
        // form; forwarding it produced a live `filters=[]`, no
        // `metadata_filter` block, and a FALSE "results are UNFILTERED"
        // warning aimed at a caller who had asked for precisely no filter.
        for encoded in ["[]", "  [ ]  ", "[\n]"] {
            let mut args = Map::new();
            args.insert("filters".to_owned(), json!(encoded));
            assert!(
                super::search_filters_arg(&args)
                    .expect("empty array is legal")
                    .is_none(),
                "encoded {encoded:?} must drop to None"
            );
        }

        // Shape errors must be caught on this path too, not forwarded to the
        // wire, matching both the CLI and the native-array arm.
        for bad in ["{}", "[1]", "not json", "\"x\"", "7"] {
            let mut args = Map::new();
            args.insert("filters".to_owned(), json!(bad));
            assert!(
                super::search_filters_arg(&args).is_err(),
                "encoded {bad:?} must be rejected"
            );
        }

        // A well-formed predicate still travels VERBATIM. Significant
        // whitespace is what makes this detectable: `Value::to_string()` emits
        // the compact form, so a re-serializing implementation cannot return
        // this byte-for-byte. A large-integer-only fixture would NOT have
        // caught it (see `search_filters_arg_string_forwarded_verbatim`).
        let raw = r#"[ { "field": "n", "operator": "=", "value": 9007199254740993 } ]"#;
        let mut args = Map::new();
        args.insert("filters".to_owned(), json!(raw));
        assert_eq!(
            super::search_filters_arg(&args).expect("ok").as_deref(),
            Some(raw)
        );
    }

    #[test]
    fn search_filters_arg_does_not_resolve_the_cli_at_path_file_form() {
        // The CLI's `@path` form reads a FILE. It is a convenience for a human
        // typing a shell command and must never be reachable from an MCP
        // parameter, or a tool caller gains an arbitrary local-file read
        // through `filters`. `search_filters_arg` therefore calls
        // `validate_json_object_array` (shape only) and NOT
        // `parse_json_object_array_arg` (which resolves `@path`).
        //
        // 🔑 THE FILE MUST CONTAIN VALID JSON, and that is the whole design of
        // this test. It first pointed at `/etc/passwd` — which is NOT valid
        // JSON, so the wrong resolver would have read the file and then failed
        // to parse it, and the assertion below would have PASSED ANYWAY. That
        // version could not distinguish "the resolver was never called" from
        // "it was called and the file happened not to parse", i.e. it asserted
        // nothing about the property it was named for.
        //
        // With VALID JSON on disk the two outcomes finally diverge:
        //   wrong resolver -> reads the file, parses `[{}]`, returns Ok(Some) -> FAILS
        //   correct one    -> "@/tmp/…" is not JSON                          -> Err, passes
        let dir = std::env::temp_dir();
        let path = dir.join("fastio_cli_at_path_probe.json");
        let fixture = r#"[{"field":"a","operator":"exists"}]"#;
        std::fs::write(&path, fixture).expect("write probe fixture");

        // POSITIVE CONTROL — proves this test can actually FAIL.
        //
        // The assertion below is an `is_err()`, and an `is_err()` passes for
        // ANY reason, including reasons that have nothing to do with the
        // property under test. So first establish that the fixture's CONTENT
        // would have been accepted: if the wrong resolver read this file, it
        // would hand back exactly this text and the check below would see an
        // `Ok`. Without this line the test is a tautology dressed as a
        // security guard — which is precisely what its first version was.
        assert_eq!(
            crate::commands::validate_json_object_array(fixture, "`filters`")
                .expect("fixture content must be VALID")
                .as_deref(),
            Some(fixture),
            "control: the file's contents must be accepted, so an Err below \
             can only mean the file was never read"
        );

        let mut args = Map::new();
        args.insert("filters".to_owned(), json!(format!("@{}", path.display())));
        let out = super::search_filters_arg(&args);
        let _ = std::fs::remove_file(&path);

        assert!(
            out.is_err(),
            "@path must be REJECTED as non-JSON, never resolved and read — \
             an Ok here means the file was opened"
        );
    }

    #[test]
    fn search_filters_arg_rejects_non_object_elements() {
        let mut args = Map::new();
        args.insert("filters".to_owned(), json!(["status=open"]));
        assert!(super::search_filters_arg(&args).is_err());
    }

    #[test]
    fn filters_schema_admits_a_real_array_not_only_a_string() {
        // The handler accepts a JSON array; a flat "type":"string" schema would
        // make that form unreachable for a validating client — the tool would
        // advertise one contract and accept another. Also assert the string
        // branch survives, since dropping it would break encoded callers.
        let schema = super::property_schema("filters", "desc");
        let variants = schema
            .get("oneOf")
            .and_then(Value::as_array)
            .expect("filters must publish a oneOf");
        let types: Vec<&str> = variants
            .iter()
            .filter_map(|v| v.get("type").and_then(Value::as_str))
            .collect();
        assert!(
            types.contains(&"array"),
            "array form must validate: {types:?}"
        );
        assert!(
            types.contains(&"string"),
            "encoded-string form must keep validating: {types:?}"
        );
        assert!(schema.get("type").is_none(), "oneOf must not be shadowed");
        // Every other parameter stays a plain string — this is a targeted
        // widening, not a change to the whole surface.
        let other = super::property_schema("node_ids", "desc");
        assert_eq!(other.get("type").and_then(Value::as_str), Some("string"));
    }

    #[test]
    fn workspace_tool_no_longer_advertises_metadata_template_id() {
        // `template_id` was declared on the WORKSPACE tool only for the
        // metadata template actions. Those are gone and the two handlers that
        // still read it (`metadata-update`/`metadata-list`) were retired with
        // them, so no workspace action reads it any more. A parameter a tool
        // advertises but no action consumes is a standing invitation to send
        // one and have it silently ignored.
        //
        // NOTE the E-Sign `sign` tool separately reads a `template_id` (`sa…`
        // signing-template ids) — a different feature entirely. This assertion
        // is scoped to the `workspace` tool on purpose and must not be widened.
        let def = super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "workspace")
            .expect("workspace tool present");
        assert!(
            !def.params.iter().any(|(name, _, _)| *name == "template_id"),
            "workspace tool must not advertise `template_id` — no surviving action reads it"
        );
    }

    #[test]
    fn retired_workspace_metadata_actions_are_not_advertised() {
        // Both routed to a `{template_id}` segment that no caller can obtain
        // any more, and `metadata-update` POSTed a route the platform retired.
        // `dead_actions_are_really_gone` proves the handler arms are gone; this
        // pins that the SCHEMA stopped naming them, which is the drift the
        // dispatch test cannot see.
        let def = super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "workspace")
            .expect("workspace tool present");
        for retired in ["metadata-update", "metadata-list"] {
            assert!(
                !def.actions.contains(&retired),
                "`{retired}` is retired but still advertised on the workspace tool"
            );
        }
    }

    #[test]
    fn metadata_filter_absent_warns_results_are_unfiltered() {
        // The dangerous case: a plain 200 with a plausible file list that was
        // never actually filtered.
        let mut payload = json!({"files": [{"id": "n1"}]});
        super::attach_metadata_filter_warnings(&mut payload);
        let warnings = warnings_of(&payload);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("UNFILTERED"),
            "must say the results are unfiltered, got: {}",
            warnings[0]
        );
    }

    #[test]
    fn metadata_filter_present_and_clean_warns_nothing() {
        // The control: a healthy filtered response must stay silent, or the
        // warning above is noise nobody reads.
        let mut payload = json!({
            "files": [{"id": "n1"}],
            "metadata_filter": {
                "applied": true, "matched": 34,
                "truncated": false, "scope_incomplete": false,
            },
        });
        super::attach_metadata_filter_warnings(&mut payload);
        assert!(warnings_of(&payload).is_empty());
    }

    #[test]
    fn metadata_filter_truncated_and_scope_incomplete_stay_distinct() {
        // They are independent booleans with OPPOSITE advice — truncated is
        // deterministic (narrow it), scope_incomplete is transient (retry).
        // Collapsing them into one "may be incomplete" line would hand the
        // caller the wrong action half the time.
        let mut truncated = json!({"metadata_filter": {"applied": true, "truncated": true}});
        super::attach_metadata_filter_warnings(&mut truncated);
        let w = warnings_of(&truncated);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("Narrow the filter"), "got: {}", w[0]);
        assert!(
            !w[0].contains("Re-run"),
            "must not advise a retry: {}",
            w[0]
        );

        let mut transient = json!({"metadata_filter": {"applied": true, "scope_incomplete": true}});
        super::attach_metadata_filter_warnings(&mut transient);
        let w = warnings_of(&transient);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("Re-run the same request"), "got: {}", w[0]);

        // Both at once yields both, not one merged line.
        let mut both = json!({
            "metadata_filter": {"applied": true, "truncated": true, "scope_incomplete": true},
        });
        super::attach_metadata_filter_warnings(&mut both);
        assert_eq!(warnings_of(&both).len(), 2);
    }

    #[test]
    fn filters_is_declared_on_every_tool_whose_search_reads_it() {
        // An undeclared param and an ignored one are indistinguishable on the
        // wire, so the declaration is pinned rather than assumed. `files
        // search` is now the ONLY handler that reads `filters` — the `ripley`
        // search action that also read it was retired on 2026-08-29 — so the
        // lock runs in both directions: `files` must declare it (a handler
        // reading a param its tool does not declare is unreachable by
        // discovery), and `ripley` must not (a declared param no handler reads
        // is a promise the tool cannot keep).
        let def_for = |tool: &str| {
            super::TOOL_DEFS
                .iter()
                .find(|d| d.name == tool)
                .unwrap_or_else(|| panic!("{tool} tool present"))
        };
        let declares_filters =
            |tool: &str| def_for(tool).params.iter().any(|(n, _, _)| *n == "filters");
        assert!(
            declares_filters("files"),
            "files tool must declare `filters` — its search handler reads it"
        );
        assert!(
            !declares_filters("ripley"),
            "ripley tool must NOT declare `filters` — the search action that \
             read it was retired"
        );
    }

    #[tokio::test]
    async fn search_tool_routes_both_contexts_through_unified_search() {
        // The `search` tool is the successor to the retired `ripley search`
        // action, and it must reach the UNIFIED `/search/` endpoints, not
        // `/storage/search/`. `api::search` validates the query BEFORE any
        // network call and `api::storage` does not, so a blank query returning
        // the unified validation error proves which builder was used — on both
        // arms, including the SHARE arm, which is the leg `ripley search` used
        // to be the only route to.
        let router = authed_router().await;
        for (action, id_key, id) in [
            ("workspace", "workspace_id", "ws1"),
            ("share", "share_id", "sh1"),
        ] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String(action.to_owned()));
            args.insert(id_key.to_owned(), Value::String(id.to_owned()));
            args.insert("query".to_owned(), Value::String("   ".to_owned()));
            let res = router.call_tool("search", args).await.expect("ok");
            let text = result_to_string(&res);
            assert!(
                text.contains("search query must not be empty"),
                "`search {action}` must route through the unified builder \
                 (pre-network query validation), got: {text}"
            );
        }
    }

    #[tokio::test]
    async fn search_rejects_malformed_pagination_instead_of_defaulting() {
        // A present-but-unparseable pagination value must be a loud error, not
        // a silent fall-back to the server default — otherwise the caller gets
        // a 200 carrying a DIFFERENT page than they asked for, with nothing in
        // the response to distinguish the two.
        // Every one of the six pagination keys, not just the first — this also
        // pins that all six are actually WIRED UP under the name the tool
        // advertises. A key that was never read would sail past unnoticed.
        let router = authed_router().await;
        for key in [
            "files_offset",
            "files_limit",
            "metadata_offset",
            "metadata_limit",
            "comments_offset",
            "comments_limit",
        ] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String("workspace".to_owned()));
            args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
            args.insert("query".to_owned(), Value::String("q".to_owned()));
            args.insert(key.to_owned(), Value::String("ten".to_owned()));
            let res = router.call_tool("search", args).await.expect("ok");
            let text = result_to_string(&res);
            assert!(
                text.contains(key),
                "a malformed `{key}` must be rejected by name, got: {text}"
            );
        }
    }

    #[test]
    fn only_buckets_arg_accepts_string_and_array_and_rejects_other() {
        let with = |v: Value| {
            let mut args = Map::new();
            args.insert("only".to_owned(), v);
            args
        };
        // Absent / null → no filter.
        assert!(super::only_buckets_arg(&Map::new()).expect("ok").is_none());
        assert!(
            super::only_buckets_arg(&with(Value::Null))
                .expect("ok")
                .is_none()
        );
        // String passes through; array is joined into the same comma form.
        assert_eq!(
            super::only_buckets_arg(&with(Value::String("files,comments".to_owned())))
                .expect("ok")
                .as_deref(),
            Some("files,comments")
        );
        assert_eq!(
            super::only_buckets_arg(&with(json!(["files", "comments"])))
                .expect("ok")
                .as_deref(),
            Some("files,comments")
        );
        // A non-string/array, and an array with a non-string entry, are LOUD —
        // never silently "show every bucket".
        assert!(super::only_buckets_arg(&with(json!(7))).is_err());
        assert!(super::only_buckets_arg(&with(json!(["files", 7]))).is_err());
        // PRESENT but naming no bucket is also loud. Supplying `only` at all
        // states an intent to narrow, so answering with every bucket would be
        // the same silent default the strict parsing exists to prevent.
        for empty in [json!([]), json!([""]), json!(""), json!(" , ")] {
            assert!(
                super::only_buckets_arg(&with(empty.clone())).is_err(),
                "present-but-empty `only` ({empty}) must be rejected, not treated as no filter"
            );
        }
        // A blank entry ALONGSIDE a real one is still a valid filter.
        assert_eq!(
            super::only_buckets_arg(&with(json!(["files", ""])))
                .expect("ok")
                .as_deref(),
            Some("files,")
        );
    }

    #[tokio::test]
    async fn files_search_accepts_a_share_target() {
        // `files search` is the FLAT leg and now serves shares too. Proven by
        // routing, not by a 200: with `share_id` supplied, `workspace_id` must
        // NOT be demanded — under the old workspace-only handler this call
        // returned the missing-parameter error instead of reaching the wire.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("search".to_owned()));
        args.insert("share_id".to_owned(), Value::String("sh1".to_owned()));
        args.insert("query".to_owned(), Value::String("q".to_owned()));
        let res = router.call_tool("files", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("Missing required parameter: workspace_id"),
            "a share-targeted files search must not demand workspace_id, got: {text}"
        );
    }

    #[test]
    fn content_search_unavailable_is_warned_on_both_placements() {
        // Matters most on the SHARE leg: per the search contract
        // `content_search_available` is reachable as `false` ONLY on share
        // routes, so before `files search` accepted a `share_id` this state
        // could not occur in MCP at all. An agent that reads the empty `files`
        // list without this warning concludes "nothing matched", when the truth
        // is content search cannot run here.
        let warnings_of = |v: &Value| {
            v.get("warnings")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|w| w.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        // Flat `/storage/search/` shape: top-level `search_metadata`.
        let mut flat = json!({
            "files": [],
            "search_metadata": {
                "content_search_available": false,
                "reason": "summary_permission_denied"
            }
        });
        super::attach_content_search_warning(&mut flat);
        let w = warnings_of(&flat);
        assert_eq!(w.len(), 1, "expected exactly one warning, got: {w:?}");
        assert!(w[0].contains("NOT available"), "got: {}", w[0]);
        assert!(
            w[0].contains("summaries"),
            "the share-only reason must be spelled out, got: {}",
            w[0]
        );
        assert!(
            w[0].contains("filename"),
            "must offer the recovery path, got: {}",
            w[0]
        );

        // Unified shape: the block rides on the files bucket instead.
        let mut nested = json!({
            "buckets": {"files": {"items": [], "search_metadata": {"content_search_available": false}}}
        });
        super::attach_content_search_warning(&mut nested);
        assert_eq!(warnings_of(&nested).len(), 1, "nested placement must warn");

        // Available, and absent, are both silent — this must not cry wolf.
        let mut ok = json!({"files": [], "search_metadata": {"content_search_available": true}});
        super::attach_content_search_warning(&mut ok);
        assert!(warnings_of(&ok).is_empty(), "available must not warn");
        let mut none = json!({"files": []});
        super::attach_content_search_warning(&mut none);
        assert!(warnings_of(&none).is_empty(), "absent report must not warn");
    }

    #[tokio::test]
    async fn files_search_target_is_exclusive_and_rejects_blanks() {
        // The MCP leg must enforce the same XOR the CLI gets from clap.
        // Preferring `share_id` when both are present is reachable in a way it
        // is not on the CLI, because `share_id` is ALSO this tool's `add-link`
        // parameter — an agent reusing an argument map would have had its
        // workspace search silently redirected to a share.
        let router = authed_router().await;
        let call = |args: Vec<(&'static str, Value)>| {
            let router = &router;
            async move {
                let mut m = Map::new();
                m.insert("action".to_owned(), Value::String("search".to_owned()));
                m.insert("query".to_owned(), Value::String("q".to_owned()));
                for (k, v) in args {
                    m.insert(k.to_owned(), v);
                }
                result_to_string(&router.call_tool("files", m).await.expect("ok"))
            }
        };

        // Neither → names BOTH accepted parameters, not just workspace_id.
        let text = call(vec![]).await;
        assert!(
            text.contains("workspace_id") && text.contains("share_id"),
            "with no target the error must name both options, got: {text}"
        );

        // Both → refused outright, never silently one of them.
        let text = call(vec![
            ("workspace_id", Value::String("ws1".to_owned())),
            ("share_id", Value::String("sh1".to_owned())),
        ])
        .await;
        assert!(
            text.contains("not both"),
            "both targets must be refused, got: {text}"
        );

        // A present-but-blank id is not a target — it must not reach the wire
        // as one, and must not let the other id through either.
        let text = call(vec![
            ("share_id", Value::String(String::new())),
            ("workspace_id", Value::String("ws1".to_owned())),
        ])
        .await;
        assert!(
            text.contains("not both") || text.contains("must not be empty"),
            "a blank share_id alongside a workspace must not silently search \
             share \"\", got: {text}"
        );
        let text = call(vec![("share_id", Value::String("   ".to_owned()))]).await;
        assert!(
            text.contains("must not be empty"),
            "a whitespace-only share_id must be rejected, got: {text}"
        );
    }

    #[test]
    fn files_tool_advertises_share_id_for_its_search_leg() {
        // WHY THIS IS NOT `params.iter().any(|n| n == "share_id")`: that
        // assertion was ALREADY TRUE before share search existed, because
        // `share_id` is also the `add-link` parameter — it passed without
        // testing anything. Worse, the first version of this change added a
        // SECOND `share_id` entry, and since `action_schema` inserts params
        // into a map by name, the later one silently overwrote the earlier in
        // the ADVERTISED schema. The name-only check could not see that.
        //
        // So assert against the schema clients actually receive, and pin
        // uniqueness directly.
        let def = super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "files")
            .expect("files tool registered");
        let dupes = def
            .params
            .iter()
            .filter(|(n, _, _)| *n == "share_id")
            .count();
        assert_eq!(
            dupes, 1,
            "`share_id` must be declared exactly once — a duplicate is silently \
             dropped from the advertised schema, got {dupes}"
        );
        let mut names: Vec<&str> = def.params.iter().map(|(n, _, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(
            before,
            names.len(),
            "files tool has duplicate param names; the later entry wins and the \
             earlier description never ships"
        );

        let tools = ToolRouter::list_tools_with(true).tools;
        let files = tools
            .iter()
            .find(|t| t.name.as_ref() == "files")
            .expect("files tool present");
        let desc = files.input_schema["properties"]["share_id"]["description"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            desc.contains("search"),
            "the ADVERTISED share_id description must document the search leg, got: {desc}"
        );
        assert!(
            desc.contains("add-link"),
            "...without dropping its original add-link meaning, got: {desc}"
        );
    }

    #[tokio::test]
    async fn search_rejects_malformed_search_mode_flags() {
        // The mode flags get the same strict treatment as pagination on this
        // tool: `case_sensitive: "tru"` is schema-valid (the schema types it as
        // a string) but is not a bool, and silently defaulting it to false runs
        // a different search than asked for. `search_in`/`name_match` as
        // non-strings are the same class.
        let router = authed_router().await;
        for (key, value) in [
            ("case_sensitive", json!("tru")),
            ("search_in", json!(["filename"])),
            ("name_match", json!(7)),
        ] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String("workspace".to_owned()));
            args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
            args.insert("query".to_owned(), Value::String("q".to_owned()));
            args.insert(key.to_owned(), value);
            let res = router.call_tool("search", args).await.expect("ok");
            let text = result_to_string(&res);
            assert!(
                text.contains(key),
                "a malformed `{key}` must be rejected by name, got: {text}"
            );
        }
    }

    #[test]
    fn only_schema_advertises_the_array_form_it_accepts() {
        // `only_buckets_arg` accepts an array, so the PUBLISHED schema must say
        // so — a schema-validating client rejects the array against a bare
        // `"type": "string"`, making the accepted spelling unreachable for
        // exactly the callers that validate.
        let tools = ToolRouter::list_tools_with(true).tools;
        let search = tools
            .iter()
            .find(|t| t.name.as_ref() == "search")
            .expect("search tool present");
        let only = search.input_schema["properties"]["only"].clone();
        assert!(
            only.get("oneOf").is_some(),
            "`only` must publish both accepted forms, got: {only}"
        );
        let forms = serde_json::to_string(&only).unwrap_or_default();
        assert!(
            forms.contains("array") && forms.contains("string"),
            "`only` schema must name array AND string, got: {forms}"
        );
    }

    #[tokio::test]
    async fn unknown_search_action_reports_unknown() {
        // Negative control for the test above: without this, the loop there
        // still passes if the arms ever stop being reachable.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("files".to_owned()));
        args.insert("query".to_owned(), Value::String("q".to_owned()));
        let res = router
            .call_tool("search", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Unknown search action"), "got: {text}");
    }

    #[tokio::test]
    async fn unknown_ripley_action_reports_unknown() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("not-an-action".to_owned()),
        );
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Unknown ripley action"), "got: {text}");
    }

    // ── FIX 1: AI-credit-spend confirmation gate (MCP) ──────────────────────

    #[test]
    fn ai_spend_confirmed_only_on_explicit_true() {
        let mut absent = Map::new();
        absent.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        assert!(
            !super::ai_spend_confirmed(&absent),
            "absent must not confirm"
        );

        let mut native_true = Map::new();
        native_true.insert("confirm_ai_spend".to_owned(), Value::Bool(true));
        assert!(
            super::ai_spend_confirmed(&native_true),
            "native true confirms"
        );

        let mut native_false = Map::new();
        native_false.insert("confirm_ai_spend".to_owned(), Value::Bool(false));
        assert!(
            !super::ai_spend_confirmed(&native_false),
            "native false must not confirm"
        );

        let mut str_true = Map::new();
        str_true.insert(
            "confirm_ai_spend".to_owned(),
            Value::String("true".to_owned()),
        );
        assert!(
            super::ai_spend_confirmed(&str_true),
            "string \"true\" confirms"
        );

        let mut str_false = Map::new();
        str_false.insert(
            "confirm_ai_spend".to_owned(),
            Value::String("false".to_owned()),
        );
        assert!(
            !super::ai_spend_confirmed(&str_false),
            "string \"false\" must not confirm"
        );
    }

    /// Drive a credit-spending action through `call_tool` WITHOUT
    /// `confirm_ai_spend` and assert it is rejected with the spend message,
    /// and NOT with any missing-parameter error (i.e. the guard fires before
    /// param validation and before any network call).
    async fn assert_spend_rejected_without_confirm(tool: &str, action: &str) {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String(action.to_owned()));
        let res = router.call_tool(tool, args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("spends AI credits") && text.contains("confirm_ai_spend=true"),
            "`{tool}`/`{action}` must be rejected without confirm_ai_spend, got: {text}"
        );
        assert!(
            !text.contains("Missing required parameter"),
            "spend guard must fire BEFORE param validation for `{tool}`/`{action}`, got: {text}"
        );
    }

    /// Drive a credit-spending action through `call_tool` WITH
    /// `confirm_ai_spend=true` but missing `workspace_id`, and assert it
    /// proceeds PAST the spend guard (surfacing the missing-param error
    /// instead of the spend rejection — so no network call is attempted).
    async fn assert_spend_proceeds_with_confirm(tool: &str, action: &str) {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String(action.to_owned()));
        args.insert("confirm_ai_spend".to_owned(), Value::Bool(true));
        let res = router.call_tool(tool, args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("spends AI credits"),
            "`{tool}`/`{action}` must pass the spend guard when confirm_ai_spend=true, got: {text}"
        );
        assert!(
            text.contains("Missing required parameter: workspace_id"),
            "with confirm_ai_spend=true the handler should reach param validation, got: {text}"
        );
    }

    #[tokio::test]
    async fn mcp_credit_spending_actions_require_confirm_ai_spend() {
        // Workspace tool surface.
        for action in ["metadata-extract", "metadata-extract-and-wait"] {
            assert_spend_rejected_without_confirm("workspace", action).await;
            assert_spend_proceeds_with_confirm("workspace", action).await;
        }
        // Standalone metadata tool surface. `auto-match` / `extract-all` were
        // removed with the template surface (2026-08-27); the gate itself is
        // unchanged and still covers every credit-spending action that remains.
        for action in ["extract", "extract-and-wait"] {
            assert_spend_rejected_without_confirm("metadata", action).await;
            assert_spend_proceeds_with_confirm("metadata", action).await;
        }
    }

    #[tokio::test]
    async fn read_only_metadata_actions_do_not_require_confirm_ai_spend() {
        // A read-only action (metadata-details) must NOT be blocked by the
        // spend guard: with no confirm_ai_spend it still reaches param
        // validation rather than the spend rejection. Re-pointed off
        // `metadata-list` when that action was retired 2026-08-27 — the
        // property under test is the spend gate, not that particular action.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("metadata-details".to_owned()),
        );
        let res = router
            .call_tool("workspace", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("spends AI credits"),
            "read-only metadata-details must not be spend-gated, got: {text}"
        );
        assert!(
            text.contains("Missing required parameter"),
            "metadata-details should reach param validation, got: {text}"
        );
    }

    #[test]
    fn workspace_and_metadata_schemas_advertise_confirm_ai_spend() {
        let tools = ToolRouter::list_tools_with(true).tools;
        for tool_name in ["workspace", "metadata"] {
            let tool = tools
                .iter()
                .find(|t| t.name.as_ref() == tool_name)
                .expect("tool present");
            let schema = serde_json::to_string(&tool.input_schema).unwrap_or_default();
            assert!(
                schema.contains("confirm_ai_spend"),
                "`{tool_name}` schema must advertise the confirm_ai_spend param, got: {schema}"
            );
        }
    }

    // ── FIX 2: present-but-empty keys / fields rejection (MCP) ──────────────

    #[test]
    fn resolve_delete_keys_distinguishes_absent_from_empty() {
        // ABSENT keys → delete-all (allowed).
        let mut absent = Map::new();
        absent.insert("node_id".to_owned(), Value::String("n1".to_owned()));
        assert_eq!(super::resolve_delete_keys(&absent), Ok(None));

        // PRESENT-but-empty → rejected.
        let mut blank = Map::new();
        blank.insert("keys".to_owned(), Value::String(String::new()));
        assert_eq!(
            super::resolve_delete_keys(&blank),
            Err(super::EMPTY_KEYS_REJECTION)
        );

        // PRESENT-but-whitespace → rejected.
        let mut ws = Map::new();
        ws.insert("keys".to_owned(), Value::String("   ".to_owned()));
        assert_eq!(
            super::resolve_delete_keys(&ws),
            Err(super::EMPTY_KEYS_REJECTION)
        );

        // PRESENT empty JSON array "[]" → rejected (ambiguous delete-all).
        let mut empty_arr = Map::new();
        empty_arr.insert("keys".to_owned(), Value::String("[]".to_owned()));
        assert_eq!(
            super::resolve_delete_keys(&empty_arr),
            Err(super::EMPTY_KEYS_REJECTION)
        );

        // PRESENT empty JSON array with inner whitespace "[ ]" → rejected.
        let mut empty_arr_ws = Map::new();
        empty_arr_ws.insert("keys".to_owned(), Value::String("[ ]".to_owned()));
        assert_eq!(
            super::resolve_delete_keys(&empty_arr_ws),
            Err(super::EMPTY_KEYS_REJECTION)
        );

        // PRESENT non-array JSON (e.g. an object) → rejected.
        let mut non_array = Map::new();
        non_array.insert("keys".to_owned(), Value::String(r#"{"a":1}"#.to_owned()));
        assert_eq!(
            super::resolve_delete_keys(&non_array),
            Err(super::EMPTY_KEYS_REJECTION)
        );

        // PRESENT array with a non-string element → rejected.
        let mut mixed = Map::new();
        mixed.insert("keys".to_owned(), Value::String(r#"["a",1]"#.to_owned()));
        assert_eq!(
            super::resolve_delete_keys(&mixed),
            Err(super::EMPTY_KEYS_REJECTION)
        );

        // PRESENT non-empty JSON array of key names → forwarded verbatim.
        let mut present = Map::new();
        present.insert("keys".to_owned(), Value::String(r#"["a","b"]"#.to_owned()));
        assert_eq!(
            super::resolve_delete_keys(&present),
            Ok(Some(r#"["a","b"]"#))
        );
    }

    #[tokio::test]
    async fn metadata_delete_rejects_present_but_empty_keys() {
        // With workspace_id + node_id present but keys="" the handler must
        // reject (not silently delete-all) BEFORE any network call.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("metadata-delete".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("n1".to_owned()));
        args.insert("keys".to_owned(), Value::String(String::new()));
        let res = router
            .call_tool("workspace", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("keys is present but empty"),
            "present-but-empty keys must be rejected, got: {text}"
        );
    }

    #[test]
    fn resolve_extract_fields_distinguishes_absent_from_empty() {
        // ABSENT fields → full schema (allowed).
        let absent = Map::new();
        assert_eq!(super::resolve_extract_fields(&absent), Ok(None));

        // PRESENT-but-empty → rejected (would otherwise widen to full schema).
        let mut blank = Map::new();
        blank.insert("fields".to_owned(), Value::String(String::new()));
        assert_eq!(
            super::resolve_extract_fields(&blank),
            Err(super::EMPTY_FIELDS_REJECTION)
        );

        // PRESENT-but-whitespace → rejected.
        let mut ws = Map::new();
        ws.insert("fields".to_owned(), Value::String("  ".to_owned()));
        assert_eq!(
            super::resolve_extract_fields(&ws),
            Err(super::EMPTY_FIELDS_REJECTION)
        );

        // PRESENT, non-empty → forwarded verbatim.
        let mut present = Map::new();
        present.insert(
            "fields".to_owned(),
            Value::String(r#"["amount"]"#.to_owned()),
        );
        assert_eq!(
            super::resolve_extract_fields(&present),
            Ok(Some(r#"["amount"]"#))
        );
    }

    #[test]
    fn string_list_arg_filters_csv_blanks_but_rejects_array_blanks() {
        // CSV string: trims and drops stray/trailing-comma blanks (loose human input).
        let mut csv = Map::new();
        csv.insert(
            "target_ids".to_owned(),
            Value::String(" n1 , , n2 ,".to_owned()),
        );
        assert_eq!(
            super::string_list_arg(&csv, "target_ids").expect("csv ok"),
            vec!["n1".to_owned(), "n2".to_owned()]
        );

        // Native array: a blank entry fails closed (machine-built list with a hole).
        let mut arr_blank = Map::new();
        arr_blank.insert("comment_ids".to_owned(), json!(["c1", ""]));
        assert!(super::string_list_arg(&arr_blank, "comment_ids").is_err());

        // Native array of clean strings → collected.
        let mut arr = Map::new();
        arr.insert("comment_ids".to_owned(), json!(["c1", "c2"]));
        assert_eq!(
            super::string_list_arg(&arr, "comment_ids").expect("array ok"),
            vec!["c1".to_owned(), "c2".to_owned()]
        );

        // Non-string array entry rejected.
        let mut arr_num = Map::new();
        arr_num.insert("comment_ids".to_owned(), json!(["c1", 7]));
        assert!(super::string_list_arg(&arr_num, "comment_ids").is_err());

        // Absent → empty (caller enforces non-empty).
        assert!(
            super::string_list_arg(&Map::new(), "target_ids")
                .expect("absent ok")
                .is_empty()
        );
    }

    #[test]
    fn workspace_update_fields_forward_intelligence() {
        // FIX 1: the MCP workspace-update handler must forward the advertised
        // `intelligence` toggle into the form body as a string.
        let mut args = Map::new();
        args.insert("intelligence".to_owned(), Value::Bool(true));
        let fields = super::build_workspace_update_fields(&args);
        assert_eq!(fields.get("intelligence").map(String::as_str), Some("true"));

        // Omitted → not sent.
        assert!(!super::build_workspace_update_fields(&Map::new()).contains_key("intelligence"));
    }

    // ─── Sign (E-Signature) MCP discipline ────────────────────────────────────

    fn sign_tool_actions() -> Vec<&'static str> {
        super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "sign")
            .expect("sign tool registered")
            .actions
            .to_vec()
    }

    #[test]
    fn sign_tool_is_registered_and_read_draft_oriented() {
        // Explicitly list with E-Sign enabled: the production `list_tools()`
        // reads the construction-time flag (disabled unless FASTIO_ENABLE_ESIGN=1
        // at server startup) and would omit `sign`. The disabled surface is
        // covered by `sign_*_disabled` below.
        let tools = ToolRouter::list_tools_with(true).tools;
        let sign = tools
            .iter()
            .find(|t| t.name.as_ref() == "sign")
            .expect("sign tool present");
        let desc = sign.description.as_deref().unwrap_or_default();
        // The description must honestly state send/void are CLI-only and that
        // it is workspace-scoped.
        assert!(
            desc.contains("CLI-binary-only") && desc.to_lowercase().contains("send"),
            "sign tool must state send/void are CLI-only, got: {desc}"
        );
        assert!(
            desc.to_lowercase().contains("workspace"),
            "sign tool description must be workspace-scoped, got: {desc}"
        );
    }

    // ─── E-Sign kill-switch (feature sunset 2026-07) ─────────────────────────

    /// The disabled surface (`list_tools_with(false)`) drops `sign` while every
    /// other tool is untouched — the filter is sign-specific, not a truncation.
    #[test]
    fn list_tools_disabled_omits_sign_only() {
        let disabled = ToolRouter::list_tools_with(false).tools;
        let has = |name: &str| disabled.iter().any(|t| t.name.as_ref() == name);
        assert!(
            !has("sign"),
            "sign must be filtered out when E-Sign is disabled"
        );
        // Every other registered tool is still present — only `sign` was dropped.
        for def in TOOL_DEFS {
            if def.name == "sign" {
                continue;
            }
            assert!(
                has(def.name),
                "non-sign tool '{}' must remain when E-Sign is disabled",
                def.name
            );
        }
        assert_eq!(
            ToolRouter::list_tools_with(true).tools.len(),
            disabled.len() + 1,
            "disabling E-Sign removes exactly one tool (sign)"
        );
    }

    /// The enabled surface (`list_tools_with(true)`) advertises `sign`.
    #[test]
    fn list_tools_enabled_contains_sign() {
        let tools = ToolRouter::list_tools_with(true).tools;
        assert!(
            tools.iter().any(|t| t.name.as_ref() == "sign"),
            "sign must be advertised when E-Sign is enabled"
        );
    }

    /// A disabled router's `sign` dispatch returns the kill-switch error text
    /// BEFORE any auth/client/arg work — a tool-level error, not an auth error.
    #[tokio::test]
    async fn call_tool_sign_disabled_returns_disabled_error() {
        let router = ToolRouter::new_with_esign(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            false,
        );
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-list".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("1".to_owned()));
        let res = router.call_tool("sign", args).await.expect("call_tool ok");
        // The disabled gate returns an ERROR result (is_error == Some(true)), not
        // a success payload.
        assert_eq!(
            res.is_error,
            Some(true),
            "disabled sign call must be an error result"
        );
        let text = result_to_string(&res);
        assert!(
            text.contains(
                "E-Sign is currently disabled. Set FASTIO_ENABLE_ESIGN=1 to use sign commands"
            ),
            "disabled sign call must return the kill-switch error, got: {text}"
        );
        // It is the kill-switch error, not the auth gate — the gate wins first.
        assert!(
            !text.contains("Not authenticated"),
            "disabled sign gate must win over the auth gate, got: {text}"
        );
    }

    /// D4: the disabled INSTANCE listing path (`list_tools()` on a router built
    /// with E-Sign disabled) omits `sign` — this exercises the production
    /// instance method, not just the parameterized `list_tools_with(false)`.
    #[test]
    fn instance_list_tools_disabled_omits_sign() {
        let router = ToolRouter::new_with_esign(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            false,
        );
        let tools = router.list_tools().tools;
        assert!(
            !tools.iter().any(|t| t.name.as_ref() == "sign"),
            "disabled instance list_tools() must omit sign"
        );
        // Every other registered tool is still advertised.
        for def in TOOL_DEFS {
            if def.name == "sign" {
                continue;
            }
            assert!(
                tools.iter().any(|t| t.name.as_ref() == def.name),
                "non-sign tool '{}' must remain on a disabled instance",
                def.name
            );
        }
    }

    /// D4: the disabled `sign` gate beats ARG EXTRACTION, not just auth — a call
    /// with only `action` (no `workspace_id`, which the handler would otherwise
    /// require) still returns the disabled error, proving the gate short-circuits
    /// before any parameter validation.
    #[tokio::test]
    async fn call_tool_sign_disabled_beats_arg_extraction() {
        let router = ToolRouter::new_with_esign(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            false,
        );
        let mut args = Map::new();
        // Minimal args: action only, deliberately NO workspace_id.
        args.insert(
            "action".to_owned(),
            Value::String("envelope-list".to_owned()),
        );
        let res = router.call_tool("sign", args).await.expect("call_tool ok");
        assert_eq!(
            res.is_error,
            Some(true),
            "disabled sign call must be an error result"
        );
        let text = result_to_string(&res);
        assert!(
            text.contains("E-Sign is currently disabled"),
            "gate must win over arg extraction, got: {text}"
        );
        // Not a missing-parameter error — the gate ran before arg extraction.
        assert!(
            !text.contains("workspace_id"),
            "the disabled gate must short-circuit before requiring workspace_id, got: {text}"
        );
    }

    // ─── `--tools` allow-list filter (P1) ────────────────────────────────────

    // Deliberately Some-wrapped so call sites read `filter_of(&[...])` next to
    // the `None` (no-filter) cases with no wrapping noise.
    #[allow(clippy::unnecessary_wraps)]
    fn filter_of(names: &[&str]) -> Option<std::collections::HashSet<String>> {
        Some(names.iter().map(|s| (*s).to_owned()).collect())
    }

    fn filtered_router(
        esign_enabled: bool,
        filter: Option<std::collections::HashSet<String>>,
    ) -> ToolRouter {
        ToolRouter::new_with_filter(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            esign_enabled,
            filter,
        )
    }

    /// A `--tools` allow-list advertises EXACTLY the requested (known) tools —
    /// no more, no fewer — in `list_tools`.
    #[test]
    fn filter_list_tools_shows_exactly_allowed() {
        let router = filtered_router(true, filter_of(&["howto", "id"]));
        let names: std::collections::HashSet<String> = router
            .list_tools()
            .tools
            .iter()
            .map(|t| t.name.as_ref().to_owned())
            .collect();
        assert_eq!(
            names,
            ["howto", "id"].iter().map(|s| (*s).to_owned()).collect(),
            "list_tools must show exactly the allow-listed tools, got: {names:?}"
        );
    }

    /// Calling a tool EXCLUDED by the filter returns the not-enabled error
    /// (`is_error` == `Some(true)`), before any dispatch.
    #[tokio::test]
    async fn filter_call_excluded_tool_is_not_enabled_error() {
        let router = filtered_router(true, filter_of(&["howto", "id"]));
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        // `org` is not in the allow-list.
        let res = router.call_tool("org", args).await.expect("call_tool ok");
        assert_eq!(
            res.is_error,
            Some(true),
            "excluded tool call must be an error result"
        );
        let text = result_to_string(&res);
        assert!(
            text.contains("Tool 'org' is not enabled on this server"),
            "excluded tool must return the not-enabled error, got: {text}"
        );
    }

    /// An INCLUDED tool still dispatches normally under a filter — it reaches its
    /// own handler (here, the auth gate) rather than the not-enabled error.
    #[tokio::test]
    async fn filter_included_tool_dispatches() {
        let router = filtered_router(true, filter_of(&["org", "id"]));
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        let res = router.call_tool("org", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("is not enabled on this server"),
            "an allow-listed tool must dispatch, not be filtered, got: {text}"
        );
    }

    /// The hidden `ai` alias is gated by its CANONICAL name `ripley`: a filter of
    /// `{ripley}` admits an `ai` call (routes through), and a filter WITHOUT
    /// `ripley` rejects it.
    #[tokio::test]
    async fn filter_gates_alias_by_canonical_name() {
        // `ripley` in the allow-list → the `ai` alias is admitted (reaches its
        // handler / auth gate, not the not-enabled error).
        let admits = filtered_router(true, filter_of(&["ripley"]));
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("ask".to_owned()));
        args.insert("question".to_owned(), Value::String("hi".to_owned()));
        let res = admits.call_tool("ai", args.clone()).await.expect("ok");
        assert!(
            !result_to_string(&res).contains("is not enabled on this server"),
            "ai alias must be admitted when the canonical `ripley` is allow-listed"
        );

        // A filter WITHOUT `ripley` → the `ai` alias is rejected under its
        // canonical name.
        let rejects = filtered_router(true, filter_of(&["org"]));
        let res = rejects.call_tool("ai", args).await.expect("ok");
        assert_eq!(res.is_error, Some(true));
        assert!(
            result_to_string(&res).contains("Tool 'ripley' is not enabled on this server"),
            "ai alias must reject under its canonical name `ripley`"
        );
    }

    /// The `how-to` alias is gated by its canonical name `howto` — the symmetric
    /// path to `ai`→`ripley`. A filter of `{howto}` admits a `how-to` call; a
    /// filter without `howto` rejects it under the canonical name.
    #[tokio::test]
    async fn filter_gates_how_to_alias_by_canonical_name() {
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("ask".to_owned()));
        args.insert("question".to_owned(), Value::String("hi".to_owned()));

        // `howto` allow-listed → `how-to` alias admitted.
        let admits = filtered_router(true, filter_of(&["howto"]));
        let res = admits.call_tool("how-to", args.clone()).await.expect("ok");
        assert!(
            !result_to_string(&res).contains("is not enabled on this server"),
            "how-to alias must be admitted when the canonical `howto` is allow-listed"
        );

        // `howto` absent → `how-to` alias rejected under `howto`.
        let rejects = filtered_router(true, filter_of(&["org"]));
        let res = rejects.call_tool("how-to", args).await.expect("ok");
        assert_eq!(res.is_error, Some(true));
        assert!(
            result_to_string(&res).contains("Tool 'howto' is not enabled on this server"),
            "how-to alias must reject under its canonical name `howto`"
        );
    }

    /// `has_visible_tools` reflects BOTH gates: a filter of only `sign` with
    /// E-Sign disabled leaves zero visible tools (the empty-effective-surface
    /// case `serve` refuses), whereas any other allow-listed tool keeps it true.
    #[test]
    fn has_visible_tools_reflects_esign_and_filter() {
        let sign_only_disabled = filtered_router(false, filter_of(&["sign"]));
        assert!(
            !sign_only_disabled.has_visible_tools(),
            "sign-only + E-Sign disabled has no visible tools"
        );
        let sign_only_enabled = filtered_router(true, filter_of(&["sign"]));
        assert!(
            sign_only_enabled.has_visible_tools(),
            "sign-only + E-Sign enabled has a visible tool"
        );
        let org_only = filtered_router(false, filter_of(&["org"]));
        assert!(
            org_only.has_visible_tools(),
            "org is visible regardless of E-Sign"
        );
        assert!(
            filtered_router(false, None).has_visible_tools(),
            "unfiltered surface is always non-empty"
        );
    }

    /// `visible_tool_names` returns exactly the advertised set (canonical names,
    /// `TOOL_DEFS` order), honoring both gates — the source the server intro uses
    /// to enumerate a filtered surface.
    #[test]
    fn visible_tool_names_matches_list_tools() {
        let router = filtered_router(true, filter_of(&["org", "howto", "id"]));
        let from_names: std::collections::HashSet<&str> =
            router.visible_tool_names().into_iter().collect();
        let from_list: std::collections::HashSet<String> = router
            .list_tools()
            .tools
            .iter()
            .map(|t| t.name.as_ref().to_owned())
            .collect();
        assert_eq!(
            from_names.len(),
            from_list.len(),
            "visible_tool_names must match list_tools cardinality"
        );
        for n in &from_names {
            assert!(
                from_list.contains(*n),
                "visible_tool_names / list_tools mismatch on {n}"
            );
        }
    }

    /// A filter that includes `sign` still respects the E-Sign kill-switch: with
    /// E-Sign disabled, `sign` is neither advertised nor callable even when it is
    /// in the allow-list (BOTH gates must pass).
    #[tokio::test]
    async fn filter_sign_still_gated_by_esign_flag() {
        let router = filtered_router(false, filter_of(&["sign", "org"]));
        // Not advertised: E-Sign disabled overrides the allow-list inclusion.
        assert!(
            !router
                .list_tools()
                .tools
                .iter()
                .any(|t| t.name.as_ref() == "sign"),
            "sign must stay hidden when E-Sign is disabled, even if allow-listed"
        );
        // Not callable: returns the E-Sign disabled error (the filter admits it,
        // then the E-Sign gate refuses it).
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-list".to_owned()),
        );
        let res = router.call_tool("sign", args).await.expect("ok");
        assert_eq!(res.is_error, Some(true));
        assert!(
            result_to_string(&res).contains("E-Sign is currently disabled"),
            "an allow-listed sign is still refused by the E-Sign gate when disabled"
        );
    }

    /// A `None` filter (the default) leaves the full surface intact — every
    /// registered tool (sign included, since E-Sign is enabled here) is
    /// advertised, matching the un-filtered production behavior.
    #[test]
    fn filter_none_is_full_surface() {
        let router = filtered_router(true, None);
        let names: std::collections::HashSet<String> = router
            .list_tools()
            .tools
            .iter()
            .map(|t| t.name.as_ref().to_owned())
            .collect();
        for def in TOOL_DEFS {
            assert!(
                names.contains(def.name),
                "None filter must advertise every tool; missing '{}'",
                def.name
            );
        }
        assert_eq!(
            names.len(),
            TOOL_DEFS.len(),
            "None filter advertises exactly the full TOOL_DEFS surface"
        );
    }

    /// Disabling E-Sign does NOT affect any other tool: `howto` still routes to
    /// its own handler (reaching the auth gate) rather than the sign gate.
    #[tokio::test]
    async fn call_tool_disabled_sign_does_not_affect_other_tools() {
        let router = ToolRouter::new_with_esign(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            false,
        );
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("ask".to_owned()));
        args.insert(
            "question".to_owned(),
            Value::String("How do I create a share?".to_owned()),
        );
        let res = router.call_tool("howto", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Not authenticated"),
            "howto must reach its own handler, got: {text}"
        );
        assert!(
            !text.contains("E-Sign is currently disabled"),
            "the E-Sign gate must not leak into other tools, got: {text}"
        );
    }

    #[test]
    fn sign_tool_omits_outward_destructive_terminal_actions() {
        // send / void are CLI-binary-only; delete is not a real action
        // (envelopes are voided). None may be advertised.
        let actions = sign_tool_actions();
        for forbidden in [
            "envelope-send",
            "send",
            "envelope-void",
            "void",
            "envelope-delete",
            "delete",
            // The sign-template SETTERS are CLI-binary-only — never advertised.
            "template-create",
            "template-update",
            "template-delete",
        ] {
            assert!(
                !actions.contains(&forbidden),
                "sign MCP tool must NOT advertise outward/destructive action '{forbidden}'"
            );
        }
        // The reversible read + draft-drive actions MUST be present — including
        // the sign-template reads + reversible draft creation.
        for present in [
            "envelope-create",
            "envelope-update",
            "envelope-list",
            "envelope-get",
            "template-list",
            "template-details",
            "template-instantiate",
            "document-download",
            "signed-download",
            "audit-download",
            "describe",
        ] {
            assert!(
                actions.contains(&present),
                "sign MCP tool must advertise read/draft action '{present}'"
            );
        }
    }

    #[test]
    fn sign_tool_schema_is_workspace_only() {
        // The schema must carry workspace_id and must NOT carry the removed
        // parent_type / parent_id params.
        let sign = TOOL_DEFS
            .iter()
            .find(|d| d.name == "sign")
            .expect("sign tool registered");
        let names: Vec<&str> = sign.params.iter().map(|(n, _, _)| *n).collect();
        assert!(
            names.contains(&"workspace_id"),
            "sign tool must declare workspace_id"
        );
        assert!(
            !names.contains(&"parent_type") && !names.contains(&"parent_id"),
            "sign tool must NOT declare the removed parent_type/parent_id params"
        );
        // The new list filters are present.
        for filter in ["envelope_status", "created_after", "created_before"] {
            assert!(
                names.contains(&filter),
                "sign tool must declare list filter '{filter}'"
            );
        }
        // workspace_id is SCHEMA-OPTIONAL (false), matching the registry
        // convention for multi-action tools (e.g. `apps`): the
        // describe / send / void / delete actions short-circuit BEFORE workspace
        // extraction, so marking it required=true would make a schema-strict MCP
        // client reject action='describe'. The real per-action requirement is
        // carried by action='describe' (common_required + per-action lists).
        let ws_required = sign
            .params
            .iter()
            .find(|(n, _, _)| *n == "workspace_id")
            .is_some_and(|(_, _, req)| *req);
        assert!(
            !ws_required,
            "workspace_id must be schema-optional so action='describe' is not \
             rejected by schema-strict clients; per-action requiredness is in describe"
        );
    }

    #[test]
    fn upload_limits_schema_uses_limit_action_selector() {
        // The create/update selector for `limits` must be a DISTINCT param
        // (`limit_action`), never the tool's routing `action` (always "limits"
        // here). The routing `action` is auto-injected by `action_schema`, so it
        // must NOT also appear as a documented tool param.
        let upload = TOOL_DEFS
            .iter()
            .find(|d| d.name == "upload")
            .expect("upload tool registered");
        let names: Vec<&str> = upload.params.iter().map(|(n, _, _)| *n).collect();
        assert!(
            names.contains(&"limit_action"),
            "upload tool must declare the limit_action selector param"
        );
        assert!(
            !names.contains(&"action"),
            "upload tool must NOT declare a bare `action` param (routing action is \
             auto-injected; the limits selector is `limit_action`)"
        );
    }

    #[tokio::test]
    async fn upload_limits_reads_limit_action_not_routing_action() {
        // Route to the limits handler (routing action="limits") and ask for the
        // `update` context via `limit_action`, omitting instance_id/file_id. The
        // pre-network validator must reject on the MISSING update fields — which
        // proves the handler read `limit_action="update"`. If it had read the
        // routing `action="limits"` (the old bug), the selector would be neither
        // create nor update, validation would pass, and the call would instead
        // fail at the network layer.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("limits".to_owned()));
        args.insert(
            "limit_action".to_owned(),
            Value::String("update".to_owned()),
        );
        let res = router
            .call_tool("upload", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("instance-id") && text.contains("update"),
            "expected the update-context validation error (proving limit_action was \
             read as the selector), got: {text}"
        );
    }

    #[tokio::test]
    async fn upload_limits_create_requires_instance_id_via_limit_action() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("limits".to_owned()));
        args.insert(
            "limit_action".to_owned(),
            Value::String("create".to_owned()),
        );
        let res = router
            .call_tool("upload", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("instance-id") && text.contains("create"),
            "expected the create-context validation error, got: {text}"
        );
    }

    #[tokio::test]
    async fn sign_describe_communicates_workspace_id_requirement() {
        // The describe payload (rendered markdown) is the authoritative
        // per-action requirement source now that the schema marks workspace_id
        // optional. It must still communicate that workspace_id is
        // commonly required — `common_required` carries it.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("common_required"),
            "describe must list common_required, got: {text}"
        );
        assert!(
            text.contains("workspace_id"),
            "describe must reference workspace_id requirement, got: {text}"
        );
    }

    #[tokio::test]
    async fn sign_describe_needs_no_auth_and_lists_actions() {
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("cli_only_actions") || text.contains("cli only"),
            "describe should name CLI-only ops, got: {text}"
        );
        // describe accuracy: every advertised action appears in the payload.
        for action in sign_tool_actions() {
            assert!(
                text.contains(action),
                "describe payload must document advertised action '{action}'"
            );
        }
    }

    #[tokio::test]
    async fn sign_send_void_are_cli_only() {
        // The outward/terminal actions route to the CLI-only fallback with a
        // clear pointer, and must not be advertised.
        let router = authed_router().await;
        for action in ["send", "void"] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String(action.to_owned()));
            args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
            args.insert("envelope_id".to_owned(), Value::String("env1".to_owned()));
            let res = router.call_tool("sign", args).await.expect("ok");
            let text = result_to_string(&res);
            assert!(
                text.contains("CLI-binary-only") && text.contains("fastio sign envelope"),
                "'{action}' must be rejected over MCP with a CLI pointer, got: {text}"
            );
        }
    }

    #[tokio::test]
    async fn sign_delete_reports_unsupported_not_cli_only() {
        // delete is NOT a real action — envelopes are voided, not deleted. It
        // must report that distinctly, not "CLI-binary-only".
        let router = authed_router().await;
        for action in ["delete", "envelope-delete"] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String(action.to_owned()));
            args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
            args.insert("envelope_id".to_owned(), Value::String("env1".to_owned()));
            let res = router.call_tool("sign", args).await.expect("ok");
            let text = result_to_string(&res);
            assert!(
                text.to_lowercase().contains("not supported")
                    && text.to_lowercase().contains("voided"),
                "'{action}' must report envelopes are voided, not deleted, got: {text}"
            );
        }
    }

    #[tokio::test]
    async fn sign_create_rejects_missing_documents_and_recipients() {
        // No *_json and no simple flags → rejected before any network call.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-create".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("needs documents"),
            "create without documents must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn sign_create_requires_workspace_id() {
        // workspace_id is required; an envelope-create without it is rejected
        // before any network call (replaces the obsolete bad-parent-type test).
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-create".to_owned()),
        );
        args.insert(
            "source_node_id".to_owned(),
            Value::String("node-1".to_owned()),
        );
        args.insert(
            "recipient_email".to_owned(),
            Value::String("a@b.com".to_owned()),
        );
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Missing required parameter: workspace_id"),
            "envelope-create must require workspace_id, got: {text}"
        );
    }

    #[tokio::test]
    async fn sign_envelope_get_requires_envelope_id() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-get".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Missing required parameter: envelope_id"),
            "envelope-get must require envelope_id, got: {text}"
        );
    }

    #[test]
    fn sign_tool_advertises_envelope_retry() {
        // retry is idempotent + no-op-success and notifies no one, so it IS
        // exposed over MCP (unlike the CLI-only send/void). It must be advertised
        // and must NOT be in the forbidden outward/terminal set.
        let actions = sign_tool_actions();
        assert!(
            actions.contains(&"envelope-retry"),
            "sign MCP tool must advertise the idempotent-recovery action 'envelope-retry'"
        );
    }

    #[tokio::test]
    async fn sign_envelope_retry_requires_envelope_id() {
        // retry is routed through the real handler (NOT the CLI-only guidance),
        // so a call with workspace_id but no envelope_id surfaces the missing
        // envelope_id — proving retry is exposed, not gated like send/void.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-retry".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Missing required parameter: envelope_id"),
            "envelope-retry must require envelope_id (and not be CLI-only-gated), got: {text}"
        );
    }

    #[tokio::test]
    async fn sign_update_requires_a_field() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-update".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("envelope_id".to_owned(), Value::String("env1".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("no fields to update"),
            "empty update must be rejected, got: {text}"
        );
    }

    // ─── FIX 1: empty recipient replace on update rejected pre-network ─────────

    #[tokio::test]
    async fn sign_update_empty_recipients_rejected() {
        // recipients_json:"[]" is a full-replacement wipe; reject before update.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-update".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("envelope_id".to_owned(), Value::String("env1".to_owned()));
        args.insert("recipients_json".to_owned(), Value::String("[]".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.to_lowercase().contains("recipient"),
            "empty recipient replace must be rejected, got: {text}"
        );
    }

    // ─── FIX 3: present-but-mistyped JSON field rejected, not dropped ──────────

    #[tokio::test]
    async fn sign_create_rejects_non_numeric_coordinate() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-create".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert(
            "source_node_id".to_owned(),
            Value::String("node-1".to_owned()),
        );
        args.insert(
            "recipient_email".to_owned(),
            Value::String("a@b.com".to_owned()),
        );
        args.insert(
            "fields_json".to_owned(),
            Value::String(r#"[{"recipient_email":"a@b.com","x_norm":"abc"}]"#.to_owned()),
        );
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("x_norm") && text.to_lowercase().contains("number"),
            "a non-numeric x_norm must be rejected, got: {text}"
        );
    }

    // ─── FIX 4: non-object body_json / policy_json rejected ────────────────────

    #[tokio::test]
    async fn sign_create_rejects_non_object_body_json() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-create".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("body_json".to_owned(), Value::String("[1,2,3]".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("body_json") && text.to_lowercase().contains("object"),
            "a non-object body_json must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn sign_create_rejects_non_object_policy_json() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("envelope-create".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert(
            "source_node_id".to_owned(),
            Value::String("node-1".to_owned()),
        );
        args.insert(
            "recipient_email".to_owned(),
            Value::String("a@b.com".to_owned()),
        );
        args.insert("policy_json".to_owned(), Value::String("[1,2]".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("policy_json") && text.to_lowercase().contains("object"),
            "a non-object policy_json must be rejected, got: {text}"
        );
    }

    // ─── MEDIUM A: non-object array element rejected pre-network ───────────────

    #[tokio::test]
    async fn sign_create_rejects_non_object_array_elements() {
        // A scalar/null/array element in recipients_json / documents_json /
        // fields_json must be rejected (naming the array + index) — never
        // accepted as an EMPTY spec that would pass the recipients-required
        // guard and ship garbage. `sign_build_create_params` parses documents
        // → recipients → fields in order, so the prereqs for each later array
        // (a valid simple-path document / recipient) are supplied so the parser
        // under test is actually reached rather than short-circuited by a
        // "needs documents/recipients" guard.
        let router = authed_router().await;
        // (malformed array, index of the offending element).
        for (bad, idx) in [("[1]", 0), ("[null]", 0), (r#"["x"]"#, 0), ("[{},1]", 1)] {
            for (key, label) in [
                ("documents_json", "documents"),
                ("recipients_json", "recipients"),
                ("fields_json", "fields"),
            ] {
                let mut args = Map::new();
                args.insert(
                    "action".to_owned(),
                    Value::String("envelope-create".to_owned()),
                );
                args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
                // Simple-path prereqs so document/recipient resolution succeeds
                // and the parser under test is reached.
                args.insert(
                    "source_node_id".to_owned(),
                    Value::String("node-1".to_owned()),
                );
                args.insert(
                    "recipient_email".to_owned(),
                    Value::String("a@b.com".to_owned()),
                );
                args.insert(key.to_owned(), Value::String(bad.to_owned()));
                let res = router.call_tool("sign", args).await.expect("ok");
                let text = result_to_string(&res);
                let expected = format!("{label}[{idx}] must be a JSON object");
                assert!(
                    text.contains(&expected),
                    "{key}={bad} must be rejected with '{expected}', got: {text}"
                );
            }
        }
    }

    // ─── FIX 6: CLI-only actions route to guidance BEFORE auth/workspace ───────

    #[tokio::test]
    async fn sign_send_cli_only_without_workspace_args() {
        // action=send with NO workspace_id must return the CLI-only guidance,
        // not "Missing required parameter".
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("send".to_owned()));
        let res = router.call_tool("sign", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("CLI-binary-only") && text.contains("fastio sign envelope"),
            "send with no workspace args must return the CLI-only message, got: {text}"
        );
        assert!(
            !text.contains("Missing required parameter"),
            "must not surface a parent-arg error, got: {text}"
        );
    }

    // ─── FIX 2: action/code-specific error mapping ─────────────────────────────

    fn sign_api_err(code: u32, http_status: u16) -> fastio_cli::error::CliError {
        fastio_cli::error::CliError::Api(fastio_cli::error::ApiError::new(
            code,
            None,
            "boom".to_owned(),
            http_status,
        ))
    }

    #[test]
    fn mcp_unreadable_error_body_is_not_artifact_not_ready() {
        // Twin of the CLI's `an_unreadable_error_body_is_not_treated_as_artifact_not_ready`.
        // MCP `sign_download` uses the same `download_file_stream`, which marks an
        // unreadable error body with `ERR_BODY_UNAVAILABLE` and `code: 0`. Zero
        // passes `!= 9992`, so a slow-but-valid `9992` (route not found — polling
        // can never resolve it) would tell an AGENT to poll indefinitely.
        use super::{SignOp, sign_err_to_result};
        let api = fastio_cli::error::CliError::Api(fastio_cli::error::ApiError::new(
            0,
            Some(fastio_cli::error::ERR_BODY_UNAVAILABLE.to_owned()),
            "the server's error body could not be read".to_owned(),
            404,
        ));
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            let m = result_to_string(&sign_err_to_result(&api, "failed to download artifact", op));
            assert!(
                !m.to_lowercase().contains("not ready") && !m.to_lowercase().contains("poll"),
                "an unreadable body must not read as pending ({op:?}): {m}"
            );
        }
        // CONTROL — a genuinely bodyless 404 still reads as not-ready, so the
        // guard is keyed on the MARKER and has not disabled the whole branch.
        let plain = result_to_string(&sign_err_to_result(
            &sign_api_err(0, 404),
            "failed to download artifact",
            SignOp::SignedFetch,
        ));
        assert!(
            plain.to_lowercase().contains("not ready"),
            "control: bodyless 404 still pends: {plain}"
        );
    }

    #[test]
    fn sign_err_artifact_fetch_404_1609_says_not_ready() {
        use super::{SignOp, sign_err_to_result};
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            let m = result_to_string(&sign_err_to_result(
                &sign_api_err(0, 404),
                "failed to download artifact",
                op,
            ));
            assert!(m.contains("not ready yet"), "got ({op:?}): {m}");
            let m = result_to_string(&sign_err_to_result(
                &sign_api_err(1609, 404),
                "failed to download artifact",
                op,
            ));
            assert!(m.contains("not ready yet"), "got ({op:?}): {m}");
        }
    }

    #[test]
    fn sign_err_artifact_not_ready_wording_is_artifact_appropriate() {
        // Item 2: the signed-PDF not-ready guidance steers to "completes"; the
        // audit-certificate guidance steers to "terminal state"
        // (completed/declined/voided/expired/failed), mirroring the CLI surface.
        use super::{SignOp, sign_err_to_result};
        let signed = result_to_string(&sign_err_to_result(
            &sign_api_err(0, 404),
            "failed to download signed document",
            SignOp::SignedFetch,
        ));
        assert!(
            signed.contains("completes") && signed.contains("signed document"),
            "signed not-ready must steer to completion: {signed}"
        );
        assert!(
            !signed.contains("terminal state"),
            "signed not-ready must not promise a terminal state: {signed}"
        );
        let audit = result_to_string(&sign_err_to_result(
            &sign_api_err(0, 404),
            "failed to download audit certificate",
            SignOp::AuditFetch,
        ));
        assert!(
            audit.contains("terminal state") && audit.contains("audit certificate"),
            "audit not-ready must steer to a terminal state: {audit}"
        );
        assert!(
            audit.contains("voided"),
            "audit not-ready must mention voided as a valid terminal state: {audit}"
        );
    }

    #[test]
    fn sign_err_general_404_does_not_say_not_ready() {
        use super::{SignOp, sign_err_to_result};
        // get / list / source-download: a 404/1609 is a genuine
        // not-found and must NOT be reframed as "not ready".
        for ctx in [
            "failed to get sign envelope",
            "failed to download source document",
        ] {
            let m = result_to_string(&sign_err_to_result(
                &sign_api_err(1609, 404),
                ctx,
                SignOp::General,
            ));
            assert!(
                !m.contains("not ready"),
                "general 404 must not say ready: {m}"
            );
        }
    }

    #[test]
    fn sign_err_restricted_1670_is_signing_scoped() {
        // 1670 now carries signing-scoped wording pointing at the org's
        // capabilities.signing (mirrors the CLI's map_signing_error).
        use super::{SignOp, sign_err_to_result};
        let m = result_to_string(&sign_err_to_result(
            &sign_api_err(1670, 403),
            "failed to create sign envelope",
            SignOp::General,
        ));
        assert!(m.contains("1670"), "got: {m}");
        assert!(
            m.contains("capabilities.signing"),
            "1670 must point at the org's capabilities.signing: {m}"
        );
    }

    #[test]
    fn sign_err_access_codes_override_generic_401() {
        // 10545 (workspace membership) / 115069 (envelope access) get
        // signing-scoped notes and must not steer to "auth login".
        use super::{SignOp, sign_err_to_result};
        let ws = result_to_string(&sign_err_to_result(
            &sign_api_err(10545, 401),
            "failed to list sign envelopes",
            SignOp::General,
        ));
        assert!(
            ws.contains("10545") && ws.to_lowercase().contains("member"),
            "got: {ws}"
        );
        assert!(
            !ws.to_lowercase().contains("auth login"),
            "10545 must not steer to auth login: {ws}"
        );
        let env = result_to_string(&sign_err_to_result(
            &sign_api_err(115_069, 401),
            "failed to get sign envelope",
            SignOp::General,
        ));
        assert!(
            env.contains("115069") && env.to_lowercase().contains("access"),
            "got: {env}"
        );
    }

    #[test]
    fn sign_err_unknown_route_9992_and_permission_1680() {
        use super::{SignOp, sign_err_to_result};
        let route = result_to_string(&sign_err_to_result(
            &sign_api_err(9992, 404),
            "failed to get sign envelope",
            SignOp::General,
        ));
        // 9992 is the API director's path walk ending unmatched — the copy must
        // name that call site and diagnose NO side. Asserting "edge" here is
        // what pinned the previous (wrong) model in place; asserting
        // "recognize"/"route" pinned the one before that.
        let rl = route.to_lowercase();
        assert!(
            route.contains("9992") && rl.contains("director"),
            "got: {route}"
        );
        assert!(
            !rl.contains("edge layer") && !rl.contains("rejected the response"),
            "9992 copy must not resurrect the edge-rejection model: {route}"
        );
        // Forbid AFFIRMATIVE domain claims only — the copy legitimately tells an
        // agent NOT to re-check ids, and a blunt substring ban flagged its own
        // prohibition. (That mistake is the same shape as the bug being fixed:
        // matching a token instead of the claim it appears in.)
        for affirmative in [
            "does not recognize",
            "removed or renamed",
            "check for a `fastio` update",
            "no such route",
        ] {
            assert!(
                !rl.contains(affirmative),
                "9992 copy must assert no domain cause, found {affirmative:?}: {route}"
            );
        }
        assert!(
            rl.contains("cannot say which side") || rl.contains("look identical"),
            "9992 copy must refuse to attribute a side: {route}"
        );
        let perm = result_to_string(&sign_err_to_result(
            &sign_api_err(1680, 403),
            "failed to update sign envelope",
            SignOp::General,
        ));
        assert!(
            perm.contains("1680") && perm.to_lowercase().contains("permission"),
            "got: {perm}"
        );
    }

    #[test]
    fn sign_err_artifact_128301_and_146422_say_not_ready() {
        // Live not-ready codes on an artifact fetch read "not ready": 128301
        // (audit certificate) and 146422 (signed PDF). Both are HTTP 404, so the
        // 404-keyed branch handles them. The MCP result must carry the server
        // code once and must NOT append the generic-404 "Verify the ID or path
        // is correct." hint (LV-2: the ids are fine, the artifact isn't ready).
        use super::{SignOp, sign_err_to_result};
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            for code in [128_301_u32, 146_422] {
                let m = result_to_string(&sign_err_to_result(
                    &sign_api_err(code, 404),
                    "failed to download artifact",
                    op,
                ));
                assert!(
                    m.contains("not ready yet"),
                    "code {code} ({op:?}): got: {m}"
                );
                assert!(
                    !m.contains("Verify the ID or path is correct"),
                    "code {code} ({op:?}): not-ready must not carry the generic-404 hint: {m}"
                );
                assert_eq!(
                    m.matches(&format!("code {code}")).count(),
                    1,
                    "code {code} ({op:?}): server code must appear exactly once: {m}"
                );
            }
        }
    }

    #[test]
    fn sign_err_artifact_146422_non_404_status_says_not_ready() {
        // Item 3: code 146422 with a NON-404 http_status must STILL map to
        // not-ready, proving the explicit `code == 146422` predicate arm rather
        // than the bare-404 arm. Covers both artifact surfaces.
        use super::{SignOp, sign_err_to_result};
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            let m = result_to_string(&sign_err_to_result(
                &sign_api_err(146_422, 200),
                "failed to download artifact",
                op,
            ));
            assert!(
                m.contains("not ready yet"),
                "146422 with status 200 must say not ready ({op:?}): {m}"
            );
        }
        // On a General op, 146422 is NOT reframed (not the artifact surface).
        let m = result_to_string(&sign_err_to_result(
            &sign_api_err(146_422, 200),
            "failed to get sign envelope",
            SignOp::General,
        ));
        assert!(
            !m.contains("not ready"),
            "general 146422 must not say ready: {m}"
        );
    }

    #[test]
    fn sign_err_artifact_9992_404_is_route_not_found_not_poll() {
        // 9992 on an artifact fetch must NOT be reframed as "not ready — poll
        // and retry". The reason is mechanical: 9992 is the API director's path
        // walk ending with no `index.php` matched, so no handler ever ran and a
        // route does not appear by polling — the poll framing would loop an
        // agent forever. The copy must still diagnose no side.
        use super::{SignOp, sign_err_to_result};
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            let m = result_to_string(&sign_err_to_result(
                &sign_api_err(9992, 404),
                "failed to download artifact",
                op,
            ));
            assert!(
                !m.contains("not ready"),
                "9992 on {op:?} must not say not-ready/poll: {m}"
            );
            assert!(
                m.contains("9992") && m.to_lowercase().contains("director"),
                "9992 on {op:?} must name the director call site: {m}"
            );
            assert!(
                !m.to_lowercase().contains("does not recognize"),
                "9992 on {op:?} must not claim the path is unrecognized: {m}"
            );
        }
    }

    #[test]
    fn sign_err_credits_1685_and_terminal_1660_code_specific() {
        use super::{SignOp, sign_err_to_result};
        let credits = result_to_string(&sign_err_to_result(
            &sign_api_err(1685, 412),
            "failed to send",
            SignOp::General,
        ));
        assert!(credits.contains("1685"), "got: {credits}");
        let terminal = result_to_string(&sign_err_to_result(
            &sign_api_err(1660, 409),
            "failed to void",
            SignOp::General,
        ));
        assert!(terminal.contains("1660"), "got: {terminal}");
        assert!(
            terminal.to_lowercase().contains("terminal"),
            "got: {terminal}"
        );
    }

    #[test]
    fn sign_err_unrelated_409_412_not_mislabeled() {
        use super::{SignOp, sign_err_to_result};
        let m = result_to_string(&sign_err_to_result(
            &sign_api_err(0, 409),
            "failed to create",
            SignOp::General,
        ));
        assert!(
            !m.to_lowercase().contains("already terminal"),
            "unrelated 409 must not claim terminal: {m}"
        );
        let m = result_to_string(&sign_err_to_result(
            &sign_api_err(0, 412),
            "failed to update",
            SignOp::General,
        ));
        assert!(
            !m.to_lowercase().contains("insufficient signing credits"),
            "unrelated 412 must not claim credits: {m}"
        );
    }

    // ─── Ripley: needs_input + publish-403 + 409-too-large ───────────────────

    /// Build a `CliError::Api` for the Ripley MCP error-mapping tests.
    fn ai_api_err(code: u32, http_status: u16) -> fastio_cli::error::CliError {
        fastio_cli::error::CliError::Api(fastio_cli::error::ApiError::new(
            code,
            None,
            "boom".to_owned(),
            http_status,
        ))
    }

    #[test]
    fn mcp_needs_input_note_surfaces_clarification() {
        use super::mcp_needs_input_note;
        let body = serde_json::json!({
            "message": {"state": "needs_input"},
            "clarification": {"type": "clarification", "question": "Which workspace?"},
        });
        let note = mcp_needs_input_note(&body, "C1");
        assert!(
            note.contains("Which workspace?"),
            "must restate the question: {note}"
        );
        assert!(note.contains("chat_id=C1"), "must say how to reply: {note}");
    }

    #[test]
    fn mcp_needs_input_note_without_question_still_guides_reply() {
        use super::mcp_needs_input_note;
        let body = serde_json::json!({"message": {"state": "needs_input"}});
        let note = mcp_needs_input_note(&body, "C2");
        assert!(
            note.contains("more information") && note.contains("chat_id=C2"),
            "must still guide a reply: {note}"
        );
    }

    #[test]
    fn mcp_needs_input_note_surfaces_share_turn_clarification() {
        // A SHARE ask wraps the detail under `turn`, not `message`.
        // The shared `extract_clarification_question` (via `message_detail`) must
        // still find the clarification so the MCP note restates it.
        use super::mcp_needs_input_note;
        let body = serde_json::json!({
            "turn": {"state": "needs_input"},
            "clarification": {"type": "clarification", "question": "Which share folder?"},
        });
        let note = mcp_needs_input_note(&body, "CS");
        assert!(
            note.contains("Which share folder?") && note.contains("chat_id=CS"),
            "share turn clarification must be surfaced: {note}"
        );
    }

    #[test]
    fn ai_publish_403_maps_to_disabled_message() {
        use super::ai_publish_err_to_result;
        let m = result_to_string(&ai_publish_err_to_result(&ai_api_err(0, 403)));
        assert!(
            m.to_lowercase().contains("disabled"),
            "must say disabled: {m}"
        );
    }

    #[test]
    fn ai_publish_non_403_not_mislabeled() {
        use super::ai_publish_err_to_result;
        let m = result_to_string(&ai_publish_err_to_result(&ai_api_err(1658, 406)));
        assert!(
            !m.to_lowercase().contains("disabled"),
            "a non-403 must not claim publishing is disabled: {m}"
        );
    }

    #[test]
    fn ai_send_too_large_codes_map_but_bare_409_does_not() {
        use super::ai_send_err_to_result;
        // The specific per-call-site STATE_TOO_LARGE codes map to the too-large
        // "start a new chat" guidance regardless of the reported HTTP status.
        for code in [168_116u32, 153_795, 148_135, 144_657] {
            let m = result_to_string(&ai_send_err_to_result(&ai_api_err(code, 409)));
            assert!(
                m.to_lowercase().contains("too large") && m.to_lowercase().contains("new chat"),
                "code {code} must map to too large + new chat: {m}"
            );
        }
        // A bare 409 WITHOUT a too-large code (e.g. the retryable
        // SEQUENCE_FAILURE the create/message endpoints also return) must NOT be
        // mislabeled "too large" — that would tell the agent to start a new chat
        // when it should retry the same idempotency key.
        let m = result_to_string(&ai_send_err_to_result(&ai_api_err(0, 409)));
        assert!(
            !m.to_lowercase().contains("too large"),
            "a bare 409 (SEQUENCE_FAILURE) must NOT be labeled too large: {m}"
        );
    }

    #[test]
    fn ai_send_unrelated_error_not_mislabeled() {
        use super::ai_send_err_to_result;
        let m = result_to_string(&ai_send_err_to_result(&ai_api_err(0, 402)));
        assert!(
            !m.to_lowercase().contains("too large"),
            "a non-409 must not claim the conversation is too large: {m}"
        );
    }

    // ─── billing: org tool action surface + subscribe onboarding ─────────────

    #[test]
    fn org_tool_advertises_renamed_billing_actions_and_drops_removed() {
        let org = TOOL_DEFS
            .iter()
            .find(|d| d.name == "org")
            .expect("org tool registered");
        for required in [
            "billing-details",
            "billing-usage",
            "billing-subscribe",
            "billing-reactivate",
        ] {
            assert!(
                org.actions.contains(&required),
                "org tool must advertise '{required}'"
            );
        }
        for removed in ["billing-activate", "billing-reset"] {
            assert!(
                !org.actions.contains(&removed),
                "org tool must NOT advertise removed action '{removed}'"
            );
        }
    }

    #[test]
    fn inject_onboarding_url_added_only_when_payment_needed() {
        // New subscription needing a payment method → URL injected.
        let mut v = json!({
            "setup_intent": {"id": "seti_1", "status": "requires_payment_method"},
            "is_active": false
        });
        inject_onboarding_url(&mut v);
        assert_eq!(v["onboarding_url"], "https://go.fast.io/onboarding");
        // client_secret / public_key are not invented by us.
        assert!(v.get("public_key").is_none());

        // Already-active update → no URL injected.
        let mut active = json!({"setup_intent": null, "is_active": true});
        inject_onboarding_url(&mut active);
        assert!(active.get("onboarding_url").is_none());

        // No setup_intent → no URL injected.
        let mut none = json!({"is_active": false});
        inject_onboarding_url(&mut none);
        assert!(none.get("onboarding_url").is_none());
    }

    #[test]
    fn sanitize_subscribe_strips_client_secret_and_public_key_but_keeps_onboarding() {
        // Reproduces the handler's post-inject composition: a 201 subscribe
        // response with a real client_secret + public_key, after onboarding
        // injection, must surface onboarding_url but echo NEITHER secret.
        let mut v = json!({
            "result": true,
            "setup_intent": {
                "id": "seti_1",
                "client_secret": "seti_1_secret_LIVE",
                "status": "requires_payment_method"
            },
            "is_active": false,
            "public_key": "pk_live_should_never_log"
        });
        inject_onboarding_url(&mut v);
        sanitize_subscribe_response(&mut v);
        let rendered = v.to_string();
        assert!(
            !rendered.contains("client_secret"),
            "client_secret key/value leaked: {rendered}"
        );
        assert!(
            !rendered.contains("seti_1_secret_LIVE"),
            "client_secret value leaked: {rendered}"
        );
        assert!(
            !rendered.contains("public_key"),
            "public_key leaked: {rendered}"
        );
        assert!(
            rendered.contains("onboarding_url"),
            "onboarding_url must be present: {rendered}"
        );
        assert_eq!(v["onboarding_url"], "https://go.fast.io/onboarding");
        // Non-sensitive setup_intent fields retained.
        assert_eq!(v["setup_intent"]["id"], "seti_1");
        assert_eq!(v["setup_intent"]["status"], "requires_payment_method");
    }

    #[test]
    fn sanitize_preview_strips_redundant_download_token_keeps_path() {
        // The get/thumbnail preauthorize response carries a
        // redundant standalone downloadToken alongside a tokenized path; the MCP
        // output must drop the standalone token but keep the tokenized path.
        let mut v = json!({
            "result": true,
            "downloadToken": "eyJhbGciOiJIUzI1NiJ9.SHOULD_NOT_SURFACE",
            "path": "/current/workspace/123/storage/abc/preview/thumbnail/read/eyJhbGci.../file/preview.png",
            "primaryFilename": "preview.png"
        });
        sanitize_preview_response(&mut v);
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("downloadToken"),
            "standalone downloadToken key must be removed: {v}"
        );
        assert!(
            obj.contains_key("path"),
            "tokenized path must be retained: {v}"
        );
        assert_eq!(
            obj.get("primaryFilename").and_then(Value::as_str),
            Some("preview.png")
        );
        assert_eq!(obj.get("result").and_then(Value::as_bool), Some(true));
    }

    #[tokio::test]
    async fn billing_cancel_rejected_without_confirm() {
        // billing-cancel must reject pre-network unless confirm_cancel=true,
        // mirroring the CLI --yes gate. The authed router points at an
        // unroutable base URL, so a rejection (not a network error) proves the
        // gate fired before any API call.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("billing-cancel".to_owned()),
        );
        args.insert("org_id".to_owned(), Value::String("123".to_owned()));
        let res = router.call_tool("org", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("confirm_cancel=true"),
            "missing confirm should be rejected with the gate message, got: {text}"
        );
    }

    #[tokio::test]
    async fn billing_cancel_false_confirm_also_rejected() {
        // An explicit confirm_cancel=false is still a rejection.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("billing-cancel".to_owned()),
        );
        args.insert("org_id".to_owned(), Value::String("123".to_owned()));
        args.insert("confirm_cancel".to_owned(), Value::Bool(false));
        let res = router.call_tool("org", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("confirm_cancel=true"),
            "confirm_cancel=false must be rejected, got: {text}"
        );
    }

    // ─── File Share MCP tool ────────────────────────────────────────────────

    /// `optional_str_strict` must keep the four cases apart: absent, `null`,
    /// empty string, and a mistyped value.
    ///
    /// Its live caller is [`super::handle_import_update_source`], where a
    /// present-but-non-string `status` read as absent would send a DIFFERENT
    /// request than the caller wrote — no action inserted, siblings still
    /// applied. Plain `optional_str` is `as_str()`, so `5` would read as "not
    /// supplied" and quietly perform that silent no-op — the failure this
    /// helper exists to prevent.
    #[test]
    fn optional_str_strict_separates_absent_null_empty_and_mistyped() {
        let mut args = Map::new();
        assert_eq!(
            super::optional_str_strict(&args, "status").ok().flatten(),
            None,
            "a missing key is not supplied"
        );

        args.insert("status".to_owned(), Value::Null);
        assert_eq!(
            super::optional_str_strict(&args, "status").ok().flatten(),
            None,
            "null is how a client spells an unset optional — not an error"
        );

        args.insert("status".to_owned(), Value::String(String::new()));
        assert_eq!(
            super::optional_str_strict(&args, "status").ok().flatten(),
            Some(""),
            "an empty string is an EXPLICIT empty value, distinct from absent"
        );

        args.insert("status".to_owned(), Value::String("ship it".to_owned()));
        assert_eq!(
            super::optional_str_strict(&args, "status").ok().flatten(),
            Some("ship it")
        );

        for mistyped in [json!(5), json!(true), json!([]), json!({})] {
            args.insert("status".to_owned(), mistyped.clone());
            let Err(err) = super::optional_str_strict(&args, "status") else {
                panic!("a present non-string ({mistyped}) must be REJECTED, not read as absent");
            };
            let text = result_to_string(&err);
            assert!(
                text.contains("status"),
                "the rejection must name the offending parameter: {text}"
            );
        }
    }

    /// A MISTYPED compare-and-swap precondition must be REJECTED, never read as
    /// absent.
    ///
    /// This is the silent-no-op the strict parser exists to stop, and it is
    /// specific to the JSON surface: `--if-version` through clap is always a
    /// String, but an MCP client can send `{"if_version": 5}`. Under plain
    /// `optional_str` that reads as `None`, and BOTH downstream guards key on
    /// `Some(..)` — the blank check and the metadata-only refusal — so the
    /// mistype would slip past both and send an UNGUARDED last-write-wins update
    /// while the agent believed a compare-and-swap protected it.
    #[tokio::test]
    async fn files_update_mistyped_if_version_is_rejected_not_read_as_absent() {
        for mistyped in [json!(5), json!(true), json!([]), json!({})] {
            let router = authed_router().await;
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String("update".to_owned()));
            args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
            args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
            args.insert("name".to_owned(), Value::String("renamed.txt".to_owned()));
            args.insert("if_version".to_owned(), mistyped.clone());
            let text = result_to_string(&router.call_tool("files", args).await.expect("ok"));
            assert!(
                text.contains("if_version must be a string"),
                "a present non-string ({mistyped}) precondition must be REJECTED, \
                 not silently dropped into an unguarded write; got: {text}"
            );
        }
    }

    /// The note-update path carries the same precondition and needs the same
    /// strictness — fixing one surface and leaving the sibling is how a
    /// corrected claim quietly survives.
    #[tokio::test]
    async fn update_note_mistyped_if_version_id_is_rejected_not_read_as_absent() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("update-note".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert("if_version_id".to_owned(), json!(5));
        let text = result_to_string(&router.call_tool("workspace", args).await.expect("ok"));
        assert!(
            text.contains("if_version_id must be a string"),
            "a present non-string precondition must be REJECTED on the note path too; got: {text}"
        );
    }

    /// A version PIN is also a field where absence CHANGES the request: omitted
    /// means "current". A present non-string read as absent silently returns the
    /// CURRENT version to a caller who asked for a historical one — wrong data,
    /// no error, nothing in the response saying the pin was dropped.
    ///
    /// `fileshare download` already guarded its `version` (see
    /// `fileshare_download_rejects_non_string_version`). These are the siblings
    /// that did not, found by sweeping every pin rather than trusting that one
    /// guarded path meant the class was handled.
    #[tokio::test]
    async fn download_file_url_rejects_non_string_version_id() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("file-url".to_owned()));
        args.insert("context_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert("version_id".to_owned(), json!(42));
        let text = result_to_string(&router.call_tool("download", args).await.expect("ok"));
        assert!(
            text.contains("version_id must be a string"),
            "a non-string version pin must be REJECTED, not silently serve the current file; \
             got: {text}"
        );
    }

    /// Same pin, the note surface.
    #[tokio::test]
    async fn read_note_rejects_non_string_version_id() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("read-note".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert("version_id".to_owned(), json!(42));
        let text = result_to_string(&router.call_tool("workspace", args).await.expect("ok"));
        assert!(
            text.contains("version_id must be a string"),
            "a non-string version pin must be REJECTED on the note read path; got: {text}"
        );
    }

    /// `sign download` writes a signed document (or audit cert) to disk. A
    /// present non-string `output_path` was silently dropped, so the file landed
    /// in the DEFAULT `.fastio/downloads/` directory instead of where the caller
    /// asked — the caller then cannot find it, or something at the default path
    /// gets overwritten.
    ///
    /// The File Share download and preview paths already guarded this; `sign`
    /// did not. A guard present on some siblings and absent on others is the
    /// hardest kind to notice.
    #[tokio::test]
    async fn sign_download_rejects_non_string_output_path() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("signed-download".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("envelope_id".to_owned(), Value::String("env1".to_owned()));
        args.insert("document_id".to_owned(), Value::String("doc1".to_owned()));
        args.insert("output_path".to_owned(), json!(42));
        let text = result_to_string(&router.call_tool("sign", args).await.expect("ok"));
        assert!(
            text.contains("`output_path` must be a string"),
            "a non-string output_path must be REJECTED, not silently write to the default \
             download directory; got: {text}"
        );
    }

    /// A SILENT DEFAULT that is not "unchanged" is the dangerous shape: the
    /// discarded argument is replaced by a DIFFERENT VALID REQUEST, so nothing
    /// downstream can tell the caller's value was dropped.
    ///
    /// `create-folder` is the write case — a mistyped `folder` under
    /// `.unwrap_or("root")` would create the folder at the ROOT of the workspace
    /// instead of inside the parent the caller named. The folder really is
    /// created; it is simply in the wrong place, and the success response looks
    /// completely normal.
    #[tokio::test]
    async fn create_folder_rejects_non_string_folder_instead_of_defaulting_to_root() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("create-folder".to_owned()),
        );
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("name".to_owned(), Value::String("new-folder".to_owned()));
        args.insert("folder".to_owned(), json!(42));
        let text = result_to_string(&router.call_tool("files", args).await.expect("ok"));
        assert!(
            text.contains("folder must be a string"),
            "a non-string parent must be REJECTED, not silently create at root; got: {text}"
        );
    }

    /// The read sibling of the same default: a mistyped `folder` would list the
    /// workspace ROOT while the caller believes they are seeing folder X.
    #[tokio::test]
    async fn files_list_rejects_non_string_folder_instead_of_listing_root() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("folder".to_owned(), json!(42));
        let text = result_to_string(&router.call_tool("files", args).await.expect("ok"));
        assert!(
            text.contains("folder must be a string"),
            "a non-string folder must be REJECTED, not silently list root; got: {text}"
        );
    }

    /// The sharpest instance of the shape: a mistyped PERMISSION silently became
    /// the default grant. The workspace is still created, with permissions the
    /// caller never asked for and no indication their value was discarded.
    #[tokio::test]
    async fn create_workspace_rejects_non_string_perm_join_instead_of_defaulting() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("create-workspace".to_owned()),
        );
        args.insert("org_id".to_owned(), Value::String("org1".to_owned()));
        args.insert("name".to_owned(), Value::String("ws-name".to_owned()));
        args.insert("perm_join".to_owned(), json!(42));
        let text = result_to_string(&router.call_tool("org", args).await.expect("ok"));
        assert!(
            text.contains("perm_join must be a string"),
            "a non-string permission must be REJECTED, not silently defaulted; got: {text}"
        );
    }

    /// Same pin, the e-signature surface — where sending a document other than
    /// the one the caller pinned matters most. Exercised through the pure
    /// param builder, which returns the rejection directly.
    #[test]
    fn sign_source_version_id_rejects_non_string() {
        let mut args = Map::new();
        args.insert(
            "source_node_id".to_owned(),
            Value::String("node-1".to_owned()),
        );
        args.insert("source_version_id".to_owned(), json!(42));
        let Err(err) = super::sign_build_create_params(&args) else {
            panic!("a non-string source_version_id must be REJECTED, not read as absent");
        };
        let text = result_to_string(&err);
        assert!(
            text.contains("source_version_id must be a string"),
            "the rejection must name the offending parameter: {text}"
        );
    }

    /// EVERY registered tool answers `action=describe` without auth — the
    /// cloud-server convention. Hand-written payloads (fileshare/id)
    /// and registry-generated ones alike must serve on an unauthenticated
    /// router, and none may return an error.
    #[tokio::test]
    async fn every_tool_describes_without_auth() {
        let router = filtered_router(true, None);
        for def in super::TOOL_DEFS {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String("describe".to_owned()));
            let result = router
                .call_tool(def.name, args)
                .await
                .unwrap_or_else(|e| panic!("{} describe MCP-errored: {e}", def.name));
            assert_ne!(
                result.is_error,
                Some(true),
                "{} describe must not be an error",
                def.name
            );
            let text = format!("{:?}", result.content);
            assert!(
                text.contains("# tool") || text.contains(def.name),
                "{} describe must identify the tool: {text}",
                def.name
            );
        }
    }

    /// The cloud-import kill-switch must outrank introspection too.
    ///
    /// `sign` gets this only by accident — it is named in the generic-describe
    /// fall-through list because it has a hand-written payload, and that
    /// happens to route it past the short-circuit into its gated match arm.
    /// `import` has no hand-written payload, so without an explicit gate
    /// `import describe` would answer in full on a server where every other
    /// `import` action is refused. A gate that holds for one tool because of an
    /// unrelated implementation detail is not a gate; this pins the behaviour
    /// directly.
    #[tokio::test]
    async fn import_describe_respects_disabled_gate() {
        let router = ToolRouter::new_with_cloud_import(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            false,
        );
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let result = router
            .call_tool("import", args)
            .await
            .expect("disabled import must not MCP-error");
        assert_eq!(result.is_error, Some(true), "disabled import is an error");
        let text = format!("{:?}", result.content);
        assert!(
            text.contains("FASTIO_ENABLE_CLOUD_IMPORT"),
            "must surface the kill-switch message, not a describe payload: {text}"
        );
    }

    /// The negative control: with the switch ON, describe still works.
    #[tokio::test]
    async fn import_describe_answers_when_enabled() {
        let router = ToolRouter::new_with_cloud_import(
            Arc::new(McpState::new_unauthenticated_for_test(
                "https://api.fast.io/current",
            )),
            true,
        );
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let result = router.call_tool("import", args).await.expect("ok");
        assert_ne!(
            result.is_error,
            Some(true),
            "an ENABLED import must still self-describe: {:?}",
            result.content
        );
    }

    /// The E-Sign kill-switch outranks introspection: `sign describe` on a
    /// DISABLED server returns the disabled error, not a describe payload.
    #[tokio::test]
    async fn sign_describe_respects_disabled_gate() {
        let router = filtered_router(false, None);
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let result = router
            .call_tool("sign", args)
            .await
            .expect("disabled sign must not MCP-error");
        assert_eq!(result.is_error, Some(true), "disabled sign is an error");
        let text = format!("{:?}", result.content);
        assert!(
            text.contains("disabled"),
            "must surface the kill-switch message: {text}"
        );
    }

    fn fileshare_tool_actions() -> Vec<&'static str> {
        super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "fileshare")
            .expect("fileshare tool registered")
            .actions
            .to_vec()
    }

    fn fs_api_err(code: u32, http_status: u16) -> fastio_cli::error::CliError {
        fastio_cli::error::CliError::Api(fastio_cli::error::ApiError::new(
            code,
            None,
            "boom".to_owned(),
            http_status,
        ))
    }

    #[test]
    fn fileshare_tool_is_registered_and_drive_oriented() {
        let tools = ToolRouter::list_tools_with(true).tools;
        let fs = tools
            .iter()
            .find(|t| t.name.as_ref() == "fileshare")
            .expect("fileshare tool present");
        let desc = fs.description.as_deref().unwrap_or_default();
        // The description must honestly state the gating split (CLI-only
        // write-back / ws-token) and the confirm gates.
        assert!(
            desc.contains("CLI-BINARY-ONLY") || desc.contains("CLI-binary-only"),
            "fileshare tool must state upload/ws-token are CLI-only, got: {desc}"
        );
        assert!(
            desc.contains("confirm_delete") && desc.contains("confirm_revoke"),
            "fileshare tool must state the confirm gates, got: {desc}"
        );
    }

    #[test]
    fn fileshare_tool_omits_cli_only_actions() {
        // upload (write-back) and ws-token are CLI-binary-only and must NOT be
        // advertised as routable actions.
        let actions = fileshare_tool_actions();
        for forbidden in ["upload", "ws-token", "writeback", "write-back"] {
            assert!(
                !actions.contains(&forbidden),
                "fileshare MCP tool must NOT advertise CLI-only action '{forbidden}'"
            );
        }
        // The exposed read/drive + confirm-gated actions MUST be present.
        for present in [
            "describe",
            "create",
            "list",
            "info",
            "update",
            "grants-list",
            "grants-add",
            "grants-remove",
            "delete",
            "versions",
            "download",
            "preview",
            "activity",
        ] {
            assert!(
                actions.contains(&present),
                "fileshare MCP tool must advertise action '{present}'"
            );
        }
    }

    #[tokio::test]
    async fn fileshare_describe_needs_no_auth_and_names_cli_only() {
        // describe must work UNAUTHENTICATED and document every advertised
        // action plus the CLI-only carve-outs.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("cli_only_actions"),
            "describe must name CLI-only ops, got: {text}"
        );
        assert!(
            text.contains("upload") && text.contains("ws-token"),
            "describe must name the CLI-only upload + ws-token, got: {text}"
        );
        // describe accuracy: every advertised action appears in the payload.
        for action in fileshare_tool_actions() {
            assert!(
                text.contains(action),
                "describe payload must document advertised action '{action}'"
            );
        }
    }

    #[tokio::test]
    async fn fileshare_upload_is_cli_only_and_does_not_touch_auth() {
        // The write-back `upload` action must return the CLI-only guidance even
        // UNAUTHENTICATED — i.e. it short-circuits BEFORE require_auth (so the
        // message is the CLI-only pointer, NOT "Not authenticated") and BEFORE
        // any arg extraction (no fileshare_id supplied).
        let router = unauthed_router();
        for action in ["upload", "writeback", "write-back"] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String(action.to_owned()));
            let res = router.call_tool("fileshare", args).await.expect("ok");
            let text = result_to_string(&res);
            assert!(
                text.contains("CLI-binary-only") && text.contains("fastio fileshare upload"),
                "upload must return the CLI-only message, got: {text}"
            );
            assert!(
                !text.contains("Not authenticated"),
                "upload must short-circuit BEFORE auth, got: {text}"
            );
            assert!(
                !text.contains("Missing required parameter"),
                "upload must short-circuit BEFORE arg extraction, got: {text}"
            );
        }
    }

    #[tokio::test]
    async fn fileshare_ws_token_is_cli_only_and_does_not_touch_auth() {
        // ws-token (realtime mint) is CLI-only — same short-circuit discipline.
        let router = unauthed_router();
        for action in ["ws-token", "websocket", "realtime-token"] {
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String(action.to_owned()));
            let res = router.call_tool("fileshare", args).await.expect("ok");
            let text = result_to_string(&res);
            assert!(
                text.contains("CLI-binary-only") && text.contains("fastio fileshare ws-token"),
                "ws-token must return the CLI-only message, got: {text}"
            );
            assert!(
                !text.contains("Not authenticated"),
                "ws-token must short-circuit BEFORE auth, got: {text}"
            );
        }
    }

    #[tokio::test]
    async fn fileshare_delete_rejected_without_confirm() {
        // delete must reject pre-AUTH and pre-arg-extraction unless
        // confirm_delete=true. An UNAUTHENTICATED, ARG-LESS probe (no
        // fileshare_id) must still see the gate message — proving the gate fires
        // before require_auth and before required_str. (Mirrors the CLI --yes
        // gate, which fires regardless of session state.)
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("delete".to_owned()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("confirm_delete=true"),
            "delete without confirm must be rejected with the gate message, got: {text}"
        );
        assert!(
            !text.contains("Not authenticated") && !text.contains("Missing required parameter"),
            "the gate must fire BEFORE auth and arg extraction, got: {text}"
        );
        // An explicit confirm_delete=false is still a rejection.
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("delete".to_owned()));
        args.insert("confirm_delete".to_owned(), Value::Bool(false));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        assert!(
            result_to_string(&res).contains("confirm_delete=true"),
            "confirm_delete=false must be rejected"
        );
    }

    #[tokio::test]
    async fn fileshare_grants_remove_rejected_without_confirm() {
        // grants-remove must reject pre-AUTH and pre-arg-extraction unless
        // confirm_revoke=true. An UNAUTHENTICATED, ARG-LESS probe (no
        // fileshare_id, no user/email) must still see the gate message — proving
        // the gate precedes both require_auth and the XOR validation.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("grants-remove".to_owned()),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("confirm_revoke=true"),
            "grants-remove without confirm must be rejected, got: {text}"
        );
        assert!(
            !text.contains("Not authenticated")
                && !text.contains("Missing required parameter")
                && !text.contains("invalid grant request"),
            "the gate must fire BEFORE auth, arg extraction, and XOR validation, got: {text}"
        );
    }

    /// A router with NO token whose client points at an unroutable base URL, so
    /// a consumption action that skips `require_auth` reaches the API-call path
    /// and fails with a NETWORK error (never "Not authenticated"). Proves the
    /// anonymous link-access path is wired.
    fn anon_router_bogus_base() -> ToolRouter {
        ToolRouter::new(
            Arc::new(McpState::new_unauthenticated_for_test(
                "http://127.0.0.1:1/current",
            )),
            None,
        )
    }

    #[tokio::test]
    async fn fileshare_info_runs_anonymously_and_reaches_network() {
        // The link-access `info` action must NOT require auth: an unauthenticated
        // call reaches the API-call path and fails with a NETWORK error against
        // the unroutable base, NOT the "Not authenticated" gate. This is the
        // anonymous-consumption path (spec §5 / CLI parity).
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("info".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("Not authenticated"),
            "info must run anonymously (skip require_auth), got: {text}"
        );
        assert!(
            text.contains("failed to get File Share details"),
            "info must reach the API-call path and surface the request failure, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_still_requires_auth_unauthenticated() {
        // Management actions keep require_auth: an unauthenticated `create` must
        // be refused with the auth message BEFORE any network call.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Not authenticated"),
            "unauthenticated create must be refused with the auth message, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_update_rejects_password_plus_clear_password() {
        // password + clear_password=true is contradictory; the MCP must reject it
        // explicitly (the CLI rejects --password + --clear-password via a clap
        // conflict) rather than silently clearing.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("update".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert("password".to_owned(), Value::String("p-secret".to_owned()));
        args.insert("clear_password".to_owned(), Value::Bool(true));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("cannot be combined"),
            "password + clear_password must be rejected, got: {text}"
        );
        assert!(
            !text.contains("p-secret"),
            "the password value must NEVER appear in the error, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_rejects_non_string_password() {
        // A non-string `password` (e.g. a JSON number) would be silently dropped
        // by Value::as_str → an UNPROTECTED share. It must be rejected with a
        // clear type error, never silently ignored.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert(
            "password".to_owned(),
            Value::Number(serde_json::Number::from(12345)),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("`password` must be a string"),
            "a non-string password must be rejected with a type error, got: {text}"
        );
    }

    #[tokio::test]
    async fn files_add_file_rejects_hash_in_workspace_context() {
        // MCP add-file is workspace-scoped, and the workspace add-file handler
        // does not accept a hash source (only `update` does). Passing `hash`
        // must be rejected with the SAME guard the CLI uses, BEFORE any network
        // call — so an agent doesn't build a request the server would 400.
        //
        // The router is authenticated (to clear require_auth) but pointed at an
        // unroutable base, so a regression that skipped the guard would surface
        // as a refused connection here, never a real API request.
        let state = Arc::new(McpState::new_unauthenticated_for_test(
            "http://127.0.0.1:1/current",
        ));
        state.set_token("test-token".to_owned()).await;
        let router = ToolRouter::new(state, None);

        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("add-file".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("name".to_owned(), Value::String("dedup.bin".to_owned()));
        args.insert("hash".to_owned(), Value::String("abc123".to_owned()));
        args.insert("hash_type".to_owned(), Value::String("sha256".to_owned()));

        let res = router.call_tool("files", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        // The exact guard message proves the workspace hash-context guard fired
        // (a network call to the bogus base could never produce this string).
        assert!(
            text.contains("only supported in a share context") && text.contains("use --upload-id"),
            "MCP add-file with a hash arg must be rejected by the workspace guard \
             before any request, got: {text}"
        );
    }

    // ─── FR-1: download target-selecting args must not be silently dropped ───

    #[tokio::test]
    async fn fileshare_download_rejects_non_string_version() {
        // A present-but-non-string `version` (e.g. a JSON number) would be
        // silently dropped by `optional_str` → the CURRENT file downloads instead
        // of the requested version (wrong bytes, no error). It must be rejected
        // pre-network with a clear type error. Run anonymously so reaching the
        // type error (and NOT the network "failed to download" error) proves the
        // check fires BEFORE any request.
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("download".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert(
            "version".to_owned(),
            Value::Number(serde_json::Number::from(42)),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("`version` must be a string"),
            "a non-string version must be rejected with a type error, got: {text}"
        );
        assert!(
            !text.contains("failed to download"),
            "the version type check must fire BEFORE the network call, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_download_rejects_non_string_output_path() {
        // A present-but-non-string `output_path` would be silently dropped →
        // the file is written to the DEFAULT path instead of where the caller
        // asked. Reject it pre-network.
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("download".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert("output_path".to_owned(), Value::Bool(true));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("`output_path` must be a string"),
            "a non-string output_path must be rejected with a type error, got: {text}"
        );
        assert!(
            !text.contains("failed to download"),
            "the output_path type check must fire BEFORE the network call, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_preview_rejects_non_string_output_path() {
        // Same target-selecting guard on the preview path's `output_path`.
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("preview".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert(
            "preview_type".to_owned(),
            Value::String("thumbnail".to_owned()),
        );
        args.insert(
            "output_path".to_owned(),
            Value::Number(serde_json::Number::from(7)),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("`output_path` must be a string"),
            "a non-string output_path must be rejected with a type error, got: {text}"
        );
        assert!(
            !text.contains("failed to download"),
            "the output_path type check must fire BEFORE the network call, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_download_accepts_string_version() {
        // A valid string `version` must NOT be rejected — it proceeds into the
        // download path (here failing on the unroutable base, NOT the type check).
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("download".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert("version".to_owned(), Value::String("v-123".to_owned()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("`version` must be a string"),
            "a valid string version must be accepted, got: {text}"
        );
    }

    // ─── FR-2: empty link password rejected on consumption paths (MCP) ──────

    #[tokio::test]
    async fn fileshare_info_rejects_empty_password() {
        // An empty `password` ("") on a CONSUMPTION path would be applied as an
        // empty `x-ve-password` header (the library validator never runs here).
        // A link password is 1-255 chars, so it must be rejected pre-network.
        // Run anonymously so reaching the empty-password error (NOT the network
        // error) proves it fires before any request.
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("info".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert("password".to_owned(), Value::String(String::new()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("link password cannot be empty"),
            "an empty consumption password must be rejected, got: {text}"
        );
        assert!(
            !text.contains("failed to get File Share details"),
            "the empty-password check must fire BEFORE the network call, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_versions_rejects_empty_password() {
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("versions".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert("password".to_owned(), Value::String(String::new()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("link password cannot be empty"),
            "an empty consumption password must be rejected, got: {text}"
        );
        assert!(
            !text.contains("failed to list File Share versions"),
            "the empty-password check must fire BEFORE the network call, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_download_rejects_empty_password() {
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("download".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert("password".to_owned(), Value::String(String::new()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("link password cannot be empty"),
            "an empty consumption password must be rejected, got: {text}"
        );
        assert!(
            !text.contains("failed to download"),
            "the empty-password check must fire BEFORE the network call, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_preview_rejects_empty_password() {
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("preview".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert(
            "preview_type".to_owned(),
            Value::String("thumbnail".to_owned()),
        );
        args.insert("password".to_owned(), Value::String(String::new()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("link password cannot be empty"),
            "an empty consumption password must be rejected, got: {text}"
        );
        assert!(
            !text.contains("failed to download"),
            "the empty-password check must fire BEFORE the network call, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_info_absent_password_is_not_rejected() {
        // An ABSENT password is the correct way to consume an UNPROTECTED share;
        // it must NOT trigger the empty-password rejection and must reach the
        // network path (here failing on the unroutable base).
        let router = anon_router_bogus_base();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("info".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("link password cannot be empty"),
            "an absent password must not be rejected, got: {text}"
        );
        assert!(
            text.contains("failed to get File Share details"),
            "an absent password must reach the consumption path, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_empty_password_still_uses_library_validator() {
        // FR-2 must NOT change management behavior: an empty `password` on
        // `create` is still handled by the library validator (not the new
        // consumption-path rejection). The error text must be the library's
        // "must not be empty" message, NOT the consumption-path wording.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert("password".to_owned(), Value::String(String::new()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("link password cannot be empty — omit"),
            "create must NOT use the consumption-path wording, got: {text}"
        );
        assert!(
            text.contains("invalid create request"),
            "create with an empty password must surface the library validator, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_password_never_echoed_on_validation_error() {
        // An explicit empty password ("") must reach the library validator
        // (rejected) — and the password value must NEVER appear in the error.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert(
            "password".to_owned(),
            Value::String("hunter2-secret".to_owned()),
        );
        // Both expiries set → a validation error before the network; the secret
        // must not leak into that error.
        args.insert("expires".to_owned(), Value::String("60".to_owned()));
        args.insert(
            "expires_at".to_owned(),
            Value::String("2030-01-01 00:00:00".to_owned()),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("invalid create request"),
            "a both-expiries create must fail validation pre-network, got: {text}"
        );
        assert!(
            !text.contains("hunter2-secret"),
            "the link password must NEVER appear in an error, got: {text}"
        );
    }

    // ─── fileshare strict expiry parse ─────────────────────────────────────
    //
    // `expires` was read via `optional_u64`, which silently returns None for a
    // present-but-invalid value — so a bad `expires` on create silently made a
    // DURABLE share. `fileshare_strict_expiry` now distinguishes ABSENT (→ None)
    // from PRESENT-but-invalid (→ a clear error) for `expires`, and rejects a
    // present non-string `expires_at`. Valid string-encoded integers are still
    // accepted (the `optional_u64` convenience is preserved for valid values).

    /// Helper: build a minimally-valid `create` arg map (workspace + node) on an
    /// authed router so the only thing under test is the expiry parse.
    async fn fileshare_create_with(key: &str, value: Value) -> (ToolRouter, String) {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert(key.to_owned(), value);
        let res = router.call_tool("fileshare", args).await.expect("ok");
        (router, result_to_string(&res))
    }

    const EXPIRES_ERR: &str = "`expires` must be a positive integer (seconds)";
    const EXPIRES_AT_ERR: &str = "`expires_at` must be a string";

    #[tokio::test]
    async fn fileshare_create_rejects_non_integer_string_expires() {
        let (_r, text) = fileshare_create_with("expires", Value::String("abc".to_owned())).await;
        assert!(
            text.contains(EXPIRES_ERR),
            "a non-integer string `expires` must be rejected, not silently dropped, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_rejects_negative_expires() {
        let (_r, text) =
            fileshare_create_with("expires", Value::Number(serde_json::Number::from(-1))).await;
        assert!(
            text.contains(EXPIRES_ERR),
            "a negative `expires` must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_rejects_fractional_expires() {
        let frac = serde_json::Number::from_f64(1.5).expect("finite");
        let (_r, text) = fileshare_create_with("expires", Value::Number(frac)).await;
        assert!(
            text.contains(EXPIRES_ERR),
            "a fractional `expires` must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_rejects_null_expires() {
        // An explicit `null` is PRESENT-but-invalid, not absent — it must be
        // rejected, never silently treated as "no expiry" (a durable share).
        let (_r, text) = fileshare_create_with("expires", Value::Null).await;
        assert!(
            text.contains(EXPIRES_ERR),
            "a null `expires` must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_rejects_non_string_expires_at() {
        let (_r, text) =
            fileshare_create_with("expires_at", Value::Number(serde_json::Number::from(12345)))
                .await;
        assert!(
            text.contains(EXPIRES_AT_ERR),
            "a non-string `expires_at` must be rejected, not silently dropped, got: {text}"
        );
    }

    /// The non-string `expires_at` error message is a multi-line `\`-continued
    /// string literal; a regression that bakes source indentation into the
    /// literal (instead of single-spacing the continuation) would render as
    /// long runs of spaces mid-message. Assert the rendered message single-
    /// spaces cleanly with no double-space runs.
    #[test]
    fn fileshare_strict_expiry_non_string_at_message_single_spaced() {
        use super::fileshare_strict_expiry;
        let mut args = Map::new();
        args.insert(
            "expires_at".to_owned(),
            Value::Number(serde_json::Number::from(12345)),
        );
        let err =
            fileshare_strict_expiry(&args).expect_err("a non-string `expires_at` must be rejected");
        let text = result_to_string(&err);
        assert!(
            text.contains(EXPIRES_AT_ERR),
            "the rendered error must carry the `expires_at` type message, got: {text}"
        );
        assert!(
            !text.contains("  "),
            "the `expires_at` error must single-space cleanly with no double-space runs \
             (source indentation must not leak into the literal), got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_update_rejects_non_integer_expires() {
        // Same strict parse on the update path.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("update".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert(
            "expires".to_owned(),
            Value::String("not-a-number".to_owned()),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains(EXPIRES_ERR),
            "an invalid `expires` on update must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_update_rejects_non_string_expires_at() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("update".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String("fs1".to_owned()));
        args.insert("expires_at".to_owned(), Value::Bool(true));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains(EXPIRES_AT_ERR),
            "a non-string `expires_at` on update must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_absent_expiry_is_not_rejected() {
        // No expires / expires_at → the strict parser yields (None, None) and the
        // request proceeds past validation toward the network (NOT a strict-parse
        // rejection). We assert ONLY that neither strict-expiry error fires.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains(EXPIRES_ERR) && !text.contains(EXPIRES_AT_ERR),
            "an absent expiry must NOT trigger a strict-parse error, got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_accepts_string_encoded_integer_expires() {
        // A valid string-encoded integer (`"60"`) must pass the strict parser —
        // preserving the `optional_u64` convenience for valid values. We pair it
        // with an `expires_at` so the LIBRARY validator rejects the mutually
        // exclusive pair: reaching "invalid create request" (and NOT the strict
        // `expires must be a positive integer` error) proves `"60"` was accepted
        // by the strict parser and forwarded into params.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert("expires".to_owned(), Value::String("60".to_owned()));
        args.insert(
            "expires_at".to_owned(),
            Value::String("2030-01-01 00:00:00".to_owned()),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains(EXPIRES_ERR),
            "a valid string-encoded integer `expires` must be accepted, got: {text}"
        );
        assert!(
            text.contains("invalid create request"),
            "a valid `\"60\"` should reach the library validator (both-expiries error), got: {text}"
        );
    }

    #[tokio::test]
    async fn fileshare_create_accepts_json_number_expires() {
        // A JSON-number `expires` (the normal form) must also pass the strict
        // parser. Same both-expiries technique to confirm it reached params.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node-1".to_owned()));
        args.insert(
            "expires".to_owned(),
            Value::Number(serde_json::Number::from(3600)),
        );
        args.insert(
            "expires_at".to_owned(),
            Value::String("2030-01-01 00:00:00".to_owned()),
        );
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains(EXPIRES_ERR),
            "a JSON-number `expires` must be accepted, got: {text}"
        );
        assert!(
            text.contains("invalid create request"),
            "a valid numeric `expires` should reach the library validator, got: {text}"
        );
    }

    // ─── R-2: activity empty fileshare_id guard ────────────────────────────

    #[tokio::test]
    async fn fileshare_activity_rejects_empty_id() {
        // `required_str` accepts a present-but-empty `""`; the activity handler
        // must reject it explicitly BEFORE calling `event::poll_activity`, so the
        // malformed `/activity/poll//` path is never built (and no network call
        // is made — the rejection fires synchronously, so this is hermetic).
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("activity".to_owned()));
        args.insert("fileshare_id".to_owned(), Value::String(String::new()));
        let res = router.call_tool("fileshare", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("`fileshare_id` must not be empty"),
            "an empty fileshare_id must be rejected before poll_activity, got: {text}"
        );
    }

    // ─── fileshare_download fallback filename sanitization ──────────────────

    #[test]
    fn fileshare_download_fallback_filename_is_path_safe() {
        // The download FALLBACK default name is derived from the
        // caller-influenced `fileshare_id` as `format!("{id}-download")` and run
        // through `sanitize_filename`. A crafted id with `../` or an absolute
        // path must NOT be able to escape `.fastio/downloads/`: sanitization
        // strips directory components, `..` sequences, and leading dots, leaving
        // a single safe basename (or "download").
        use fastio_cli::api::download::sanitize_filename;
        for malicious in [
            "../../etc/passwd",
            "/etc/passwd",
            "..\\..\\windows\\system32",
            "....//....//secret",
        ] {
            let name = sanitize_filename(&format!("{malicious}-download"));
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "sanitized fallback must have no path separators, got: {name}"
            );
            assert!(
                !name.contains(".."),
                "sanitized fallback must not contain `..`, got: {name}"
            );
            assert!(
                !name.starts_with('.'),
                "sanitized fallback must not start with a dot, got: {name}"
            );
            assert!(!name.is_empty(), "sanitized fallback must not be empty");
        }
        // A path-only id collapses to the empty→"download" fallback, never empty.
        assert_eq!(sanitize_filename(&format!("{}-download", "/")), "-download");
    }

    // ─── fileshare_err_to_result (mirrors the CLI map_fileshare_error) ──────

    #[test]
    fn fileshare_err_link_1650_vs_management_1650() {
        use super::{FsMcpOp, fileshare_err_to_result};
        // On a LINK-ACCESS op a 1650 steers to the password arg, never to login.
        let link = result_to_string(&fileshare_err_to_result(
            &fs_api_err(1650, 401),
            "failed to get File Share details",
            FsMcpOp::LinkAccess,
        ));
        assert!(
            link.contains("password"),
            "link 1650 must steer to the password arg: {link}"
        );
        assert!(
            !link.to_lowercase().contains("auth login"),
            "link 1650 must NOT suggest account login: {link}"
        );
        // On a MANAGEMENT op a 1650 is account auth — defers to the generic
        // login hint and must NOT mention a link password.
        let mgmt = result_to_string(&fileshare_err_to_result(
            &fs_api_err(1650, 401),
            "failed to list File Shares",
            FsMcpOp::ManagementOther,
        ));
        assert!(
            !mgmt.to_lowercase().contains("link password") && !mgmt.contains("the `password` arg"),
            "management 1650 must NOT frame a link password: {mgmt}"
        );
        assert!(
            mgmt.to_lowercase().contains("auth login"),
            "management 1650 must keep the generic account-login hint: {mgmt}"
        );
    }

    #[test]
    fn fileshare_err_1609_and_bare_404_are_uniform_unavailable() {
        use super::{FsMcpOp, fileshare_err_to_result};
        for code in [1609u32, 0] {
            let m = result_to_string(&fileshare_err_to_result(
                &fs_api_err(code, 404),
                "failed to get",
                FsMcpOp::LinkAccess,
            ));
            assert!(
                m.contains("unavailable"),
                "must say unavailable (code {code}): {m}"
            );
            assert!(
                m.contains("not exist") && m.contains("expired") && m.contains("revoked"),
                "must list all three reasons uniformly (code {code}): {m}"
            );
        }
    }

    #[test]
    fn fileshare_err_1700_describes_capability_order() {
        use super::{FsMcpOp, fileshare_err_to_result};
        let m = result_to_string(&fileshare_err_to_result(
            &fs_api_err(1700, 403),
            "failed to download",
            FsMcpOp::LinkAccess,
        ));
        assert!(
            m.contains("view") && m.contains("download") && m.contains("edit"),
            "must describe capability order: {m}"
        );
    }

    // ─── LV CLI-1: preview-specific 404 / 143705 (MCP mirror) ───────────────

    #[test]
    fn fileshare_err_preview_143705_is_preview_not_uniform_unavailable() {
        use super::{FsMcpOp, fileshare_err_to_result};
        // A 143705 is a PREVIEW miss — never the uniform share-gone wording.
        // Keyed on the code alone (op-independent), so it holds on the preview op.
        let m = result_to_string(&fileshare_err_to_result(
            &fs_api_err(143_705, 404),
            "failed to download File Share preview",
            FsMcpOp::Preview,
        ));
        assert!(
            m.contains("no preview of this type"),
            "143705 must use the preview-specific wording: {m}"
        );
        assert!(
            m.contains("preview_type"),
            "143705 must steer to another preview_type: {m}"
        );
        assert!(
            !m.contains("may have been revoked"),
            "143705 must NOT use the uniform share-gone wording: {m}"
        );
    }

    #[test]
    fn fileshare_err_preview_bare_404_is_preview_not_uniform_unavailable() {
        use super::{FsMcpOp, fileshare_err_to_result};
        // A bare 404 on the PREVIEW op is a preview miss, not a share-gone.
        let m = result_to_string(&fileshare_err_to_result(
            &fs_api_err(0, 404),
            "failed to download File Share preview",
            FsMcpOp::Preview,
        ));
        assert!(
            m.contains("no preview of this type"),
            "a bare 404 on the preview op must be preview-specific: {m}"
        );
        assert!(
            !m.contains("may have been revoked"),
            "a bare 404 on the preview op must NOT be the uniform share-gone wording: {m}"
        );
    }

    #[test]
    fn fileshare_err_preview_1609_stays_uniform_unavailable() {
        use super::{FsMcpOp, fileshare_err_to_result};
        // A 1609 on the preview op means the SHARE is gone — keep uniform wording.
        let m = result_to_string(&fileshare_err_to_result(
            &fs_api_err(1609, 404),
            "failed to download File Share preview",
            FsMcpOp::Preview,
        ));
        assert!(
            m.contains("unavailable") && m.contains("not exist") && m.contains("revoked"),
            "a 1609 on the preview op must stay uniform-unavailable: {m}"
        );
        assert!(
            !m.contains("no preview of this type"),
            "a 1609 (share gone) must NOT be reframed as a preview miss: {m}"
        );
    }

    #[test]
    fn fileshare_err_nonpreview_bare_404_stays_uniform_unavailable() {
        use super::{FsMcpOp, fileshare_err_to_result};
        // The uniform-404 discipline for NON-preview ops must be untouched.
        let m = result_to_string(&fileshare_err_to_result(
            &fs_api_err(0, 404),
            "failed to get",
            FsMcpOp::LinkAccess,
        ));
        assert!(
            m.contains("unavailable") && m.contains("revoked"),
            "a bare 404 on a non-preview op must stay uniform-unavailable: {m}"
        );
        assert!(
            !m.contains("no preview of this type"),
            "a non-preview 404 must NOT borrow the preview wording: {m}"
        );
    }

    #[test]
    fn fileshare_err_1680_matches_cli_without_permissions_tagline() {
        use super::{FsMcpOp, fileshare_err_to_result};
        // P3F-6: the MCP 1680 message must NOT append "This is a property of the
        // file, not your permissions." — the CLI headline omits it (exact parity;
        // the CLI carries that sentence only in its separate hint line).
        let m = result_to_string(&fileshare_err_to_result(
            &fs_api_err(1680, 403),
            "failed to download",
            FsMcpOp::LinkAccess,
        ));
        assert!(
            m.contains("cannot be served (1680)") && m.contains("DMCA"),
            "1680 must explain the bound file is not serveable: {m}"
        );
        assert!(
            !m.contains("property of the file"),
            "1680 must NOT append the permissions tagline (CLI parity): {m}"
        );
    }

    #[test]
    fn fileshare_err_1605_create_hints_node_must_be_file() {
        use super::{FsMcpOp, fileshare_err_to_result};
        let create = result_to_string(&fileshare_err_to_result(
            &fs_api_err(1605, 400),
            "failed to create",
            FsMcpOp::Create,
        ));
        assert!(
            create.contains("FILE node"),
            "create 1605 must hint node-is-file: {create}"
        );
        // Non-create 1605 just surfaces the server message, no file hint.
        let other = result_to_string(&fileshare_err_to_result(
            &fs_api_err(1605, 400),
            "failed to update",
            FsMcpOp::ManagementOther,
        ));
        assert!(
            !other.contains("FILE node"),
            "non-create 1605 must not add the file hint: {other}"
        );
    }

    #[test]
    fn fileshare_err_version_conflict_carries_current_version() {
        use super::{FsMcpOp, fileshare_err_to_result};
        let err = fastio_cli::error::CliError::VersionConflict {
            current_version: "v9-abc".to_owned(),
        };
        let m = result_to_string(&fileshare_err_to_result(
            &err,
            "failed to write back",
            FsMcpOp::LinkAccess,
        ));
        assert!(
            m.contains("v9-abc"),
            "a VersionConflict must surface the current version id: {m}"
        );
        // P3F-7: the rebase recipe from `suggestion()` must be carried into the
        // MCP result for parity with the CLI render.
        assert!(
            m.contains("Re-fetch") && m.contains("re-apply") && m.contains("current version id"),
            "a VersionConflict must carry the rebase suggestion: {m}"
        );
    }

    // ─── Strict optional-param parsers (constrained params) ──────────────────

    /// The retired `limit`/`offset` warning must fire only when a value was
    /// really supplied — present-and-`null` is an UNSET optional, not a
    /// requested slice.
    #[test]
    fn legacy_offset_args_supplied_treats_present_null_as_unset() {
        use super::legacy_offset_args_supplied;
        // Absent → not supplied.
        assert!(!legacy_offset_args_supplied(&Map::new()));

        // PRESENT-BUT-NULL → not supplied. This is the case that was wrong: a
        // client materializing every optional as `null` was told its slice had
        // been IGNORED when it asked for nothing. `null` means unset, exactly
        // as `optional_str_strict`/`optional_u32_strict` document.
        for key in ["limit", "offset"] {
            let mut args = Map::new();
            args.insert(key.to_owned(), Value::Null);
            assert!(
                !legacy_offset_args_supplied(&args),
                "{key}: null must read as UNSET, not as a requested slice"
            );
        }

        // Genuinely supplied → warn. The warning must not be lost by the fix.
        for value in [json!(1), json!("25"), json!(0)] {
            for key in ["limit", "offset"] {
                let mut args = Map::new();
                args.insert(key.to_owned(), value.clone());
                assert!(
                    legacy_offset_args_supplied(&args),
                    "{key}={value} was really supplied and must still warn"
                );
            }
        }
    }

    #[test]
    fn optional_u32_strict_rejects_malformed_passes_valid_and_absent() {
        use super::optional_u32_strict;
        // Absent → Ok(None).
        let empty = Map::new();
        assert_eq!(
            optional_u32_strict(&empty, "rotate").ok().flatten(),
            None,
            "an absent param must resolve to None"
        );
        // Valid numeric → Ok(Some(v)).
        let mut num = Map::new();
        num.insert("rotate".to_owned(), json!(90));
        assert_eq!(
            optional_u32_strict(&num, "rotate").ok().flatten(),
            Some(90),
            "a valid integer must parse"
        );
        // Valid numeric string → Ok(Some(v)).
        let mut numstr = Map::new();
        numstr.insert("rotate".to_owned(), json!("90"));
        assert_eq!(
            optional_u32_strict(&numstr, "rotate").ok().flatten(),
            Some(90),
            "a valid integer string must parse"
        );
        // Present-but-malformed → Err with a param-named message.
        let mut bad = Map::new();
        bad.insert("rotate".to_owned(), json!("90deg"));
        let err = optional_u32_strict(&bad, "rotate")
            .expect_err("a present-but-non-integer value must error");
        let text = result_to_string(&err);
        assert!(
            text.contains("rotate") && text.contains("integer"),
            "the error must name the bad param and say it must be an integer: {text}"
        );
        // Null is treated as absent (parity with the lenient helper).
        let mut null = Map::new();
        null.insert("rotate".to_owned(), Value::Null);
        assert_eq!(
            optional_u32_strict(&null, "rotate").ok().flatten(),
            None,
            "an explicit null must resolve to None, not an error"
        );
    }

    /// `optional_bool_strict` must reject a PRESENT-but-invalid value
    /// (e.g. `intelligence: "tru"`) instead of silently defaulting to `false`,
    /// while passing through valid and absent values.
    #[test]
    fn optional_bool_strict_rejects_malformed_passes_valid_and_absent() {
        use super::optional_bool_strict;
        // Absent → Ok(None).
        let empty = Map::new();
        assert_eq!(
            optional_bool_strict(&empty, "intelligence").ok().flatten(),
            None,
            "an absent param must resolve to None"
        );
        // Native bool → Ok(Some(v)).
        let mut native = Map::new();
        native.insert("intelligence".to_owned(), json!(true));
        assert_eq!(
            optional_bool_strict(&native, "intelligence").ok().flatten(),
            Some(true),
            "a native bool must parse"
        );
        // String "false" → Ok(Some(false)).
        let mut boolstr = Map::new();
        boolstr.insert("intelligence".to_owned(), json!("false"));
        assert_eq!(
            optional_bool_strict(&boolstr, "intelligence")
                .ok()
                .flatten(),
            Some(false),
            "a valid bool string must parse"
        );
        // Present-but-malformed → Err with a param-named message.
        let mut bad = Map::new();
        bad.insert("intelligence".to_owned(), json!("tru"));
        let err = optional_bool_strict(&bad, "intelligence")
            .expect_err("a present-but-invalid bool must error");
        let text = result_to_string(&err);
        assert!(
            text.contains("intelligence") && text.contains("boolean"),
            "the error must name the bad param and say it must be a boolean: {text}"
        );
    }

    /// The MCP `upload web-list` handler validates `status` against the same
    /// set the CLI's clap `value_parser` enforces — a bad value must error
    /// before reaching the server, and every valid value must be accepted.
    #[tokio::test]
    async fn upload_web_list_rejects_invalid_status() {
        use super::WEB_UPLOAD_STATUSES;
        // The valid set is the contract set (upload.txt), `canceled` spelling.
        assert_eq!(
            WEB_UPLOAD_STATUSES,
            &[
                "pending",
                "queued",
                "downloading",
                "uploading",
                "complete",
                "failed",
                "canceled",
            ]
        );
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("web-list".to_owned()));
        args.insert("status".to_owned(), json!("cancelled")); // double-l typo
        let result = router
            .call_tool("upload", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&result);
        assert!(
            text.contains("Invalid status") && text.contains("cancelled"),
            "an invalid web-list status must be rejected before the call: {text}"
        );
    }

    /// The MCP `event search` handler reads `acknowledged` with the strict
    /// boolean parser, so a present-but-malformed value errors before the
    /// network round-trip instead of silently widening the query.
    #[tokio::test]
    async fn event_search_rejects_malformed_acknowledged() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("search".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("acknowledged".to_owned(), json!("yep"));
        let result = router.call_tool("event", args).await.expect("call_tool ok");
        let text = result_to_string(&result);
        assert!(
            text.contains("acknowledged") && text.contains("boolean"),
            "a malformed acknowledged must be rejected before the call: {text}"
        );
    }

    /// `event summarize` mirrors `search`'s strict `acknowledged` parsing.
    #[tokio::test]
    async fn event_summarize_rejects_malformed_acknowledged() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("summarize".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("acknowledged".to_owned(), json!("maybe"));
        let result = router.call_tool("event", args).await.expect("call_tool ok");
        let text = result_to_string(&result);
        assert!(
            text.contains("acknowledged") && text.contains("boolean"),
            "a malformed acknowledged must be rejected before the call: {text}"
        );
    }

    /// Inline `comment create` enforces the same ≤25 attachment cap as `attach`
    /// — 26 inline `target_ids` are rejected before any network round-trip.
    #[tokio::test]
    async fn comment_create_rejects_more_than_25_target_ids() {
        let router = authed_router().await;
        let ids: Vec<String> = (0..26).map(|i| format!("obj{i}")).collect();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        args.insert(
            "entity_type".to_owned(),
            Value::String("workspace".to_owned()),
        );
        args.insert("entity_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("node1".to_owned()));
        args.insert("text".to_owned(), Value::String("hi".to_owned()));
        args.insert("target_ids".to_owned(), Value::String(ids.join(",")));
        let result = router
            .call_tool("comment", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&result);
        assert!(
            text.contains("at most 25 attachments"),
            "create with 26 target_ids must be rejected, got: {text}"
        );
    }

    /// A one-shot server that CAPTURES the request line and answers `200`.
    ///
    /// Needed because `authed_router`'s dead port fails BEFORE a request is
    /// written, so nothing there can observe what the handler actually sent.
    /// Testing MCP argument parsing without seeing the outgoing query is how a
    /// silently-dropped scope id stays invisible: every layer looks fine in
    /// isolation and only the wire shows the field is gone.
    async fn capture_router() -> (ToolRouter, std::sync::Arc<std::sync::Mutex<String>>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&captured);
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                if let Ok(n) = sock.read(&mut buf).await {
                    *sink.lock().expect("capture lock") =
                        String::from_utf8_lossy(&buf[..n]).into_owned();
                }
                let body = br#"{"result":"yes","response":{}}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.flush().await;
            }
        });
        let state = Arc::new(McpState::new_unauthenticated_for_test(&format!(
            "http://{addr}"
        )));
        state.set_token("test-token".to_owned()).await;
        (ToolRouter::new_with_esign(state, true), captured)
    }

    /// A 19-digit id sent as a JSON NUMBER must still scope the search.
    ///
    /// `optional_str` reads a present number as absent, which would run the
    /// search **unscoped** and return another workspace's events as though they
    /// were the one asked for — wrong data, exit 0, and nothing server-side to
    /// catch it, because the server sees a request with no scope at all.
    #[tokio::test]
    async fn event_search_scopes_on_a_numeric_workspace_id() {
        let (router, captured) = capture_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("search".to_owned()));
        args.insert(
            "workspace_id".to_owned(),
            json!(4_687_730_903_718_774_523_u64),
        );
        let _ = router.call_tool("event", args).await.expect("call_tool ok");

        let req = captured.lock().expect("capture lock").clone();
        assert!(!req.is_empty(), "the capture server saw no request at all");
        assert!(
            req.contains("workspace_id=4687730903718774523"),
            "a numeric workspace_id must reach the query EXACTLY — dropping it \
             silently widens the search to every visible workspace.\nrequest:\n{req}"
        );
    }

    /// The negative control: a wrong-TYPE scope id is a caller bug and must
    /// error rather than coerce or vanish.
    #[tokio::test]
    async fn event_search_rejects_a_structured_scope_id() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("search".to_owned()));
        args.insert("workspace_id".to_owned(), json!({"id": "123"}));
        let res = router.call_tool("event", args).await.expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("must be an ID string"),
            "an object in a scope-id slot must be rejected, not ignored, got: {text}"
        );
    }

    /// Every AI turn must carry a replay guard on the wire.
    ///
    /// The CLI and MCP both advise "retry the same idempotency key", so a turn
    /// sent without one names something that never existed and the retry an
    /// agent actually performs bills a SECOND turn. Asserted at the
    /// wire because a key built in memory and never sent looks identical from
    /// inside the handler.
    #[tokio::test]
    async fn ai_turn_creation_sends_an_idempotency_key() {
        let (router, captured) = capture_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("chat-create".to_owned()));
        args.insert("context_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert(
            "query_text".to_owned(),
            Value::String("summarize this".to_owned()),
        );
        let _ = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");

        let req = captured.lock().expect("capture lock").clone();
        assert!(!req.is_empty(), "the capture server saw no request");
        assert!(
            req.contains("idempotency_key="),
            "a generated key must reach the body:\n{req}"
        );
    }

    /// A CALLER-supplied key must be used verbatim — that is the whole retry
    /// path. Generating a fresh one here would silently bill a second turn,
    /// which is the exact defect, just moved.
    #[tokio::test]
    async fn a_supplied_idempotency_key_is_sent_verbatim() {
        let (router, captured) = capture_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("chat-create".to_owned()));
        args.insert("context_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("query_text".to_owned(), Value::String("hi".to_owned()));
        args.insert(
            "idempotency_key".to_owned(),
            Value::String("replay-me-1234".to_owned()),
        );
        let _ = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");

        let req = captured.lock().expect("capture lock").clone();
        assert!(
            req.contains("idempotency_key=replay-me-1234"),
            "the caller's key must be sent unchanged:\n{req}"
        );
    }

    /// `archived` must actually reach the query on the org-scoped listing.
    ///
    /// The server defaults it to `"false"`, so an archived workspace is
    /// unreachable unless the parameter is sent — the default hides the rows.
    #[tokio::test]
    async fn workspace_list_sends_archived_on_the_org_scoped_route() {
        let (router, captured) = capture_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        args.insert("org_id".to_owned(), Value::String("org1".to_owned()));
        args.insert("archived".to_owned(), Value::Bool(true));
        let _ = router.call_tool("workspace", args).await.expect("ok");

        let req = captured.lock().expect("capture lock").clone();
        assert!(!req.is_empty(), "the capture server saw no request");
        assert!(
            req.contains("/org/org1/list/workspaces/"),
            "must use the org-scoped route:\n{req}"
        );
        assert!(
            req.contains("archived=true"),
            "the filter must reach the query:\n{req}"
        );
    }

    /// The negative control, and the reason this is not a blanket "send it
    /// everywhere": `/workspaces/all/` declares NO request parameters. It
    /// returns every workspace with an `archived` flag per row instead of
    /// filtering, so sending the parameter there would be a swallowed no-op
    /// dressed up as a working filter.
    #[tokio::test]
    async fn workspace_list_omits_archived_on_the_unfiltered_route() {
        let (router, captured) = capture_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        args.insert("archived".to_owned(), Value::Bool(true));
        let _ = router.call_tool("workspace", args).await.expect("ok");

        let req = captured.lock().expect("capture lock").clone();
        assert!(!req.is_empty(), "the capture server saw no request");
        assert!(req.contains("/workspaces/all/"), "unfiltered route:\n{req}");
        assert!(
            !req.contains("archived="),
            "`/workspaces/all/` takes no query parameters — sending one there \
             pretends to filter and does not:\n{req}"
        );
    }

    /// EVERY tool must DECLARE `describe`, not merely accept it.
    ///
    /// The router answers `describe` for all 26 tools. A strict MCP client
    /// validates against the schema and refuses to send an action the schema
    /// calls invalid, so a tool that omits `describe` from its action list
    /// leaves a documented universal capability unreachable for exactly the
    /// clients most careful about the contract.
    ///
    /// This iterates the ACTUAL `TOOL_DEFS` rather than a list written here —
    /// a hand-kept list would go stale the moment a 27th tool is added, and the
    /// new tool would be the one that is wrong.
    #[test]
    fn every_tool_declares_the_universal_describe_action() {
        let missing: Vec<&str> = TOOL_DEFS
            .iter()
            .filter(|d| !d.actions.contains(&"describe"))
            .map(|d| d.name)
            .collect();
        assert!(
            missing.is_empty(),
            "these tools accept `describe` but do not declare it: {missing:?}"
        );
        assert!(TOOL_DEFS.len() >= 26, "sanity: the tool table was not read");
    }

    /// And it must actually answer, for every one of them — declaring an action
    /// the router then rejects would be the same defect pointed the other way.
    #[tokio::test]
    async fn every_declared_describe_actually_answers() {
        for def in TOOL_DEFS {
            // `sign` and `import` are kill-switched off on this router by
            // default; their gated behaviour has its own dedicated tests.
            if matches!(def.name, "sign" | "import") {
                continue;
            }
            let router = unauthed_router();
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String("describe".to_owned()));
            let res = router
                .call_tool(def.name, args)
                .await
                .unwrap_or_else(|e| panic!("{} describe must not MCP-error: {e}", def.name));
            assert_ne!(
                res.is_error,
                Some(true),
                "{} declares `describe` but the router refused it: {:?}",
                def.name,
                res.content
            );
        }
    }

    /// `validation` must survive the MCP field parser, and a non-object
    /// must be REJECTED rather than dropped.
    ///
    /// A parser that rebuilds each field key-by-key loses any key it does not
    /// name — `validation` among them. Dropping a constraint silently on a
    /// signing surface produces an envelope that enforces nothing while looking
    /// like it enforces something.
    #[test]
    fn mcp_field_parser_preserves_validation_and_rejects_a_non_object() {
        let ok = vec![json!({
            "recipient_email": "a@b.com",
            "document_index": 0, "page": 1,
            "x_norm": 0.1, "y_norm": 0.1, "w_norm": 0.2, "h_norm": 0.05,
            "type": "text", "required": true,
            "validation": {"min_length": 2, "max_length": 64}
        })];
        let parsed = super::sign_parse_fields(&ok).expect("valid fields parse");
        // `to_json` is private to the api crate, so assert on the parsed spec —
        // the serializer's own coverage lives in `api::signing::tests`.
        let v = parsed[0]
            .validation
            .as_ref()
            .expect("the constraint must survive the parser");
        assert_eq!(v.get("max_length"), Some(&json!(64)), "got: {v}");
        assert!(v.is_object(), "it must stay an OBJECT: {v}");

        let bad = vec![json!({
            "recipient_email": "a@b.com",
            "document_index": 0, "page": 1,
            "x_norm": 0.1, "y_norm": 0.1, "w_norm": 0.2, "h_norm": 0.05,
            "type": "text", "required": true,
            "validation": "min_length=2"
        })];
        let err = super::sign_parse_fields(&bad).expect_err("a string validation is an error");
        let text = result_to_string(&err);
        assert!(
            text.contains("validation") && text.contains("object"),
            "the rejection must say what was wrong: {text}"
        );
    }

    /// Share storage must build a `/share/…` route, not `/workspace/…`.
    ///
    /// The published API docs document each of these storage operations for
    /// shares and say outright that "share storage follows identical patterns
    /// to workspace storage for all common operations", so every one of them
    /// must go through the same generic builder `update`/`addfile` use rather
    /// than a hardcoded workspace prefix.
    ///
    /// Asserted at the WIRE: a request-line check is the only thing that can
    /// tell a share route from a workspace route, since both return the same
    /// shape and the handler looks identical from the inside.
    #[tokio::test]
    async fn files_actions_build_share_routes_when_given_a_share_id() {
        for (action, extra, expect) in [
            ("list", vec![], "GET /share/sh1/storage/root/list/"),
            (
                "info",
                vec![("node_id", "n1")],
                "GET /share/sh1/storage/n1/details/",
            ),
            (
                "delete",
                vec![("node_id", "n1")],
                "DELETE /share/sh1/storage/n1/delete/",
            ),
            (
                "versions",
                vec![("node_id", "n1")],
                "GET /share/sh1/storage/n1/versions/",
            ),
            ("recent", vec![], "GET /share/sh1/storage/recent/"),
        ] {
            let (router, captured) = capture_router().await;
            let mut args = Map::new();
            args.insert("action".to_owned(), Value::String(action.to_owned()));
            args.insert("share_id".to_owned(), Value::String("sh1".to_owned()));
            for (k, v) in extra {
                args.insert(k.to_owned(), Value::String(v.to_owned()));
            }
            let _ = router.call_tool("files", args).await.expect("call_tool ok");

            let req = captured.lock().expect("capture lock").clone();
            assert!(!req.is_empty(), "action={action}: no request was captured");
            assert!(
                req.starts_with(expect),
                "action={action} must target the SHARE route.\nwant: {expect}\ngot:\n{req}"
            );
            assert!(
                !req.contains("/workspace/"),
                "action={action} must not build a workspace route:\n{req}"
            );
        }
    }

    /// The negative control: `workspace_id` still builds the workspace route,
    /// so generalizing did not quietly move the default.
    #[tokio::test]
    async fn files_list_still_builds_the_workspace_route_by_default() {
        let (router, captured) = capture_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        let _ = router.call_tool("files", args).await.expect("ok");
        let req = captured.lock().expect("capture lock").clone();
        assert!(
            req.starts_with("GET /workspace/ws1/storage/root/list/"),
            "the workspace leg must be unchanged:\n{req}"
        );
    }

    /// Naming BOTH is an error, not a silent precedence rule — a caller that
    /// sends both does not know which it wants, and choosing for them is how
    /// the wrong tree gets modified.
    #[tokio::test]
    async fn files_refuses_both_a_workspace_and_a_share() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("list".to_owned()));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("share_id".to_owned(), Value::String("sh1".to_owned()));
        let res = router.call_tool("files", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("not both"),
            "naming both must be refused, got: {text}"
        );
    }

    /// Build the argument map every node-scoped comment action needs.
    fn node_args(action: &str, entity_type: &str) -> Map<String, Value> {
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String(action.to_owned()));
        args.insert(
            "entity_type".to_owned(),
            Value::String(entity_type.to_owned()),
        );
        args.insert("entity_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("node_id".to_owned(), Value::String("n1".to_owned()));
        args.insert("content".to_owned(), Value::String("hi".to_owned()));
        args.insert("comment_id".to_owned(), Value::String("c1".to_owned()));
        args
    }

    /// The MCP surface must refuse the SAME entity types the CLI refuses.
    ///
    /// These three actions read `entity_type`; forwarding it unvalidated would
    /// accept `org` over MCP while clap refuses it on the
    /// CLI — one concept, two contracts. The router points at a dead port, so a
    /// forwarded value fails as a TRANSPORT error; asserting on the validation
    /// message therefore proves the rejection happened locally, before the
    /// request was built.
    #[tokio::test]
    async fn comment_node_actions_reject_an_entity_type_the_cli_rejects() {
        for action in ["list", "create", "reply"] {
            let router = authed_router().await;
            let res = router
                .call_tool("comment", node_args(action, "org"))
                .await
                .expect("call_tool ok");
            let text = result_to_string(&res);
            assert!(
                text.contains("invalid entity_type 'org'"),
                "comment action='{action}' must reject entity_type 'org' locally \
                 the way clap does, got: {text}"
            );
            assert!(
                text.contains("workspace") && text.contains("fileshare"),
                "the rejection must name the accepted types, got: {text}"
            );
        }
    }

    /// The negative control for the test above: `fileshare` IS valid on the
    /// node-scoped routes, so validation must let it through. Without this, a
    /// future tightening to `["workspace","share"]` everywhere would look green.
    #[tokio::test]
    async fn comment_node_actions_still_accept_fileshare() {
        for action in ["list", "create", "reply"] {
            let router = authed_router().await;
            let res = router
                .call_tool("comment", node_args(action, "fileshare"))
                .await
                .expect("call_tool ok");
            let text = result_to_string(&res);
            assert!(
                !text.contains("invalid entity_type"),
                "`fileshare` is valid on the node-scoped comment routes and must \
                 NOT be rejected by validation (action='{action}'), got: {text}"
            );
        }
    }

    /// `list-all` has a NARROWER list — no `fileshare`, because a File Share
    /// comment always targets a node. `org` was never valid on either.
    #[tokio::test]
    async fn comment_list_all_rejects_org_and_keeps_fileshare_guidance() {
        let router = authed_router().await;
        let mut args = node_args("list-all", "org");
        args.remove("node_id");
        let res = router.call_tool("comment", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("invalid entity_type 'org'"),
            "list-all must reject 'org', got: {text}"
        );
        assert!(
            !text.contains("fileshare"),
            "list-all's accepted-type list must NOT advertise fileshare, got: {text}"
        );

        // The fileshare arm keeps its actionable message rather than a bare list.
        let router = authed_router().await;
        let mut args = node_args("list-all", "fileshare");
        args.remove("node_id");
        let res = router.call_tool("comment", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("requires a node") && text.contains("action='list'"),
            "the fileshare-on-list-all rejection must stay actionable, got: {text}"
        );
    }

    /// The two lists are one source of truth shared with `commands::comment`
    /// (and, through it, with clap). Pin the relationship between them so a
    /// widening of the container list is a deliberate edit, not a side effect.
    #[test]
    fn container_entity_types_are_the_node_list_minus_fileshare() {
        use crate::commands::comment::{CONTAINER_ENTITY_TYPES, VALID_ENTITY_TYPES};

        let expected: Vec<&str> = VALID_ENTITY_TYPES
            .iter()
            .copied()
            .filter(|t| *t != "fileshare")
            .collect();
        assert_eq!(
            CONTAINER_ENTITY_TYPES,
            expected.as_slice(),
            "the container-scoped comment routes accept exactly the node-scoped \
             types except `fileshare`, which always targets a specific node"
        );
    }
}
