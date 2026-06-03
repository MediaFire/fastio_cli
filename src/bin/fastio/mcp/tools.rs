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

/// Extract an optional u32 parameter.
fn optional_u32(args: &Map<String, Value>, key: &str) -> Option<u32> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Extract an optional u8 parameter.
fn optional_u8(args: &Map<String, Value>, key: &str) -> Option<u8> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .and_then(|n| u8::try_from(n).ok())
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
/// Per the contract (ai.txt:2404 for extract-all, ai.txt:2631 for single-file
/// extract) an OMITTED `fields` means "extract the full schema" — the wide,
/// most expensive path. An absent `fields` is therefore allowed and resolves
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
/// Per the contract (ai.txt:2600-2612) `keys` is a JSON-encoded array of key
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
        properties.insert(
            name.to_owned(),
            json!({
                "type": "string",
                "description": desc,
            }),
        );
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

const TOOL_DEFS: &[ToolDef] = &[
    ToolDef {
        name: "auth",
        description: "Authentication: sign in, sign out, check status, manage API keys, 2FA, OAuth sessions, email/password management, token introspection.",
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
            "oauth-revoke",
            "oauth-revoke-all",
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
                "OAuth session ID (oauth-details, oauth-revoke)",
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
            "close",
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
        ],
        params: &[
            ("first_name", "First name (update)", false),
            ("last_name", "Last name (update)", false),
            ("display_name", "Display name (update)", false),
            ("query", "Search query (search)", false),
            ("confirmation", "Confirmation string (close)", false),
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
            ("phone_number", "Phone number (phone)", false),
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
        ],
        params: &[
            ("org_id", "Organization ID", false),
            ("name", "Org/workspace name", false),
            ("domain", "Org domain/slug", false),
            ("description", "Org description", false),
            ("industry", "Industry", false),
            ("billing_email", "Billing email", false),
            ("homepage_url", "Homepage URL", false),
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
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
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
            ("user_id", "User ID (members-details)", false),
        ],
    },
    ToolDef {
        name: "workspace",
        description: "Workspaces: list, create, view, update, delete, archive/unarchive, members, shares, notes, quickshares, metadata, import/export, workflow. SIDE EFFECTS — these metadata actions SPEND AI CREDITS: 'metadata-template-preview-match', 'metadata-template-suggest-fields', 'metadata-extract', 'metadata-extract-and-wait'. 'metadata-extract-and-wait' enqueues a single-file extraction and polls workspace jobs-status to a terminal state before returning.",
        actions: &[
            "list",
            "create",
            "info",
            "update",
            "delete",
            "enable-workflow",
            "disable-workflow",
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
            "quickshare-get",
            "quickshare-delete",
            "quickshares-list",
            "enable-import",
            "disable-import",
            "metadata-template-categories",
            "metadata-template-create",
            "metadata-template-preview-match",
            "metadata-template-suggest-fields",
            "metadata-template-delete",
            "metadata-template-list",
            "metadata-template-details",
            "metadata-template-settings",
            "metadata-template-update",
            "metadata-delete",
            "metadata-details",
            "metadata-extract",
            "metadata-extract-and-wait",
            "metadata-list",
            "metadata-template-select",
            "metadata-templates-in-use",
            "metadata-update",
            "metadata-view-save",
            "metadata-view-get",
            "metadata-view-delete",
            "metadata-views-list",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("org_id", "Organization ID (list, create)", false),
            ("name", "Workspace name", false),
            ("folder_name", "Folder name / slug (create)", false),
            ("description", "Description", false),
            ("intelligence", "Enable AI (true/false)", false),
            ("query", "Search query", false),
            ("confirm", "Confirmation string (delete)", false),
            ("share_id", "Share ID (import-share)", false),
            ("node_id", "Node ID (notes, quickshare, metadata)", false),
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
                "template_id",
                "Metadata template ID (also the key for workspace-level saved views: view-save/view-get/view-delete)",
                false,
            ),
            ("category", "Metadata template category", false),
            ("fields", "JSON-encoded field definitions (metadata)", false),
            (
                "node_ids",
                "JSON-encoded array (or comma-separated list) of 1-25 node IDs (metadata-template-suggest-fields, metadata-details)",
                false,
            ),
            (
                "user_context",
                "Short hint, max 64 chars, letters/numbers/spaces only (metadata-template-suggest-fields)",
                false,
            ),
            (
                "enabled",
                "Enabled state (true/false, metadata-template-settings)",
                false,
            ),
            (
                "priority",
                "Priority 1-5 (metadata-template-settings)",
                false,
            ),
            (
                "copy",
                "Create copy instead (true/false, metadata-template-update)",
                false,
            ),
            (
                "keys",
                "JSON array of keys to delete (metadata-delete)",
                false,
            ),
            (
                "key_values",
                "JSON object of key-value pairs (metadata-update)",
                false,
            ),
            (
                "filters",
                "JSON-encoded filter criteria (metadata-list)",
                false,
            ),
            ("order_by", "Field to order by (metadata-list)", false),
            ("order_desc", "Descending order (true/false)", false),
            (
                "config",
                "JSON-encoded saved-view config string: {version:1, columns:[{field,visible?,width?}], sort:{field,dir}, filters:[{field,operator,value_type,value}]} (metadata-view-save)",
                false,
            ),
            (
                "filter",
                "Filter: enabled/disabled (metadata-template-list)",
                false,
            ),
            (
                "poll_interval",
                "Seconds between job-status polls, 1-60, default 3 (metadata-extract-and-wait)",
                false,
            ),
            (
                "confirm_ai_spend",
                "REQUIRED to proceed (true/false) for the AI-credit-spending actions metadata-template-preview-match, metadata-template-suggest-fields, metadata-extract, metadata-extract-and-wait. These actions are rejected unless confirm_ai_spend=true. Ignored by read-only metadata actions.",
                false,
            ),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "files",
        description: "File operations: list, details, create folders, move, copy, rename, delete, restore, purge, trash, versions, search, recent, lock, quickshare, transfer, read content.",
        actions: &[
            "list",
            "info",
            "create-folder",
            "move",
            "copy",
            "rename",
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
            "quickshare",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("node_id", "File/folder node ID", false),
            (
                "folder",
                "Parent folder ID (list, create-folder); defaults to root",
                false,
            ),
            ("name", "Folder name (create-folder)", false),
            ("to", "Target parent folder ID (move, copy)", false),
            ("new_name", "New name (rename)", false),
            ("query", "Search query (search)", false),
            ("limit", "Max results (search)", false),
            ("offset", "Result offset (search)", false),
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
                "details",
                "Enrich each search hit with the full node resource (search)",
                false,
            ),
            (
                "output",
                "content_snippet verbosity: terse/standard/full (search)",
                false,
            ),
            ("sort_by", "Sort field (list)", false),
            ("sort_dir", "Sort direction: asc/desc", false),
            ("page_size", "Page size (list, trash)", false),
            ("cursor", "Pagination cursor (list, trash)", false),
            ("share_id", "Share ID (add-link)", false),
            ("to_workspace", "Target workspace ID (transfer)", false),
            ("version_id", "Version ID (version-restore)", false),
        ],
    },
    ToolDef {
        name: "upload",
        description: "Upload files: text content, URL import, session management, chunk ops, web imports, limits.",
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
            "extensions",
            "stream",
            "create-stream-session",
            "stream-send",
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
        ],
    },
    ToolDef {
        name: "download",
        description: "Downloads: get file download URLs, folder ZIP URLs, quickshare details. file-url returns a secret-bearing URL (short-lived scoped read token) — do not log or share it.",
        actions: &["file-url", "zip-url", "quickshare-details"],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("node_id", "File/folder node ID", false),
            ("version_id", "Version ID (file-url)", false),
            ("quickshare_id", "Quickshare ID (quickshare-details)", false),
        ],
    },
    ToolDef {
        name: "share",
        description: "Shares (data rooms): list, create, view, update, delete, archive/unarchive, password-auth, guest-auth, quickshare, workflow, discovery.",
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
            "quickshare-create",
            "available",
            "check-name",
            "enable-workflow",
            "disable-workflow",
        ],
        params: &[
            ("share_id", "Share ID", false),
            (
                "workspace_id",
                "Workspace ID (create, quickshare-create)",
                false,
            ),
            ("name", "Share name", false),
            ("description", "Description", false),
            ("access_options", "Access options", false),
            ("password", "Share password", false),
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
                "anonymous_uploads_enabled",
                "Allow anonymous uploads (true/false)",
                false,
            ),
            ("intelligence", "Enable AI intelligence (true/false)", false),
            ("confirm", "Confirmation (delete)", false),
            ("folder", "Folder ID (files-list)", false),
            ("email", "Member email (members-add)", false),
            ("role", "Member role", false),
            ("node_id", "Node ID (quickshare-create)", false),
            ("expires", "Expiration (quickshare-create)", false),
            (
                "expires_at",
                "ISO datetime expiration (quickshare-create)",
                false,
            ),
            ("sort_by", "Sort field (files-list)", false),
            ("sort_dir", "Sort direction (files-list)", false),
            ("page_size", "Page size", false),
            ("cursor", "Pagination cursor", false),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "ripley",
        description: "Offload multi-step work to Ripley, Fast.io's AI agent: ask a question and get the answer (ask — creates a chat and waits), lower-level chat create/list/details/update/delete/publish/cancel, message send/list/details/read, semantic search, generate AI shares (share-generate), transactions, autotitle, and self-only AI memory (memory-get/memory-set/memory-delete). (Formerly the `ai` tool; `ai` still works as a hidden alias.)",
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
            "search",
            "memory-get",
            "memory-set",
            "memory-delete",
        ],
        params: &[
            (
                "context_type",
                "Context: workspace or share (chat/message/share/transactions/autotitle); \
                 org or workspace for memory-* actions",
                false,
            ),
            ("context_id", "Workspace, share, or org ID", false),
            (
                "query_text",
                "Question or search query (ask, chat-create, message-send, search)",
                false,
            ),
            (
                "type",
                "Chat type: chat or chat_with_files (chat-create)",
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
            ("content", "Memory content, max 64KB (memory-set)", false),
            (
                "revision",
                "Optimistic-concurrency revision; write only if it matches (memory-set)",
                false,
            ),
            (
                "files_scope",
                "Comma-separated nodeId:versionId file scope pairs (chat-create, message-send)",
                false,
            ),
            (
                "folders_scope",
                "Comma-separated nodeId:depth folder scope pairs (chat-create, message-send)",
                false,
            ),
            (
                "files_attach",
                "Comma-separated file nodeId:versionId attachment pairs (chat-create, message-send)",
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
            ("personality", "Response style: concise or detailed", false),
            (
                "include_deleted",
                "List deleted chats instead (chat-list)",
                false,
            ),
            ("context", "Hint for autotitle", false),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
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
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "comment",
        description: "Comments: list, list-all, create, reply, delete, bulk-delete, details, reaction-add, reaction-remove, link, unlink, linked.",
        actions: &[
            "list",
            "list-all",
            "create",
            "reply",
            "delete",
            "bulk-delete",
            "details",
            "reaction-add",
            "reaction-remove",
            "link",
            "unlink",
            "linked",
        ],
        params: &[
            ("entity_type", "Entity type: workspace or share", false),
            ("entity_id", "Workspace or share ID", false),
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
                "linked_entity_type",
                "Linked entity type: task or approval",
                false,
            ),
            ("linked_entity_id", "Linked entity ID", false),
            ("sort", "Sort order (list)", false),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "event",
        description: "Events: search, summarize, details, ack, activity-list, activity-poll.",
        actions: &[
            "search",
            "summarize",
            "details",
            "ack",
            "activity-list",
            "activity-poll",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("share_id", "Share ID", false),
            ("user_id", "User ID", false),
            ("org_id", "Organization ID", false),
            ("event_id", "Event ID (details, ack)", false),
            ("event", "Event type filter", false),
            ("category", "Category filter", false),
            ("subcategory", "Subcategory filter", false),
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
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "invitation",
        description: "Invitations: list, accept, decline, delete invitations.",
        actions: &["list", "accept", "decline", "delete"],
        params: &[
            ("invitation_id", "Invitation ID", false),
            ("entity_type", "Entity type (decline, delete)", false),
            ("entity_id", "Entity ID (decline, delete)", false),
        ],
    },
    ToolDef {
        name: "preview",
        description: "Previews: get preview URLs and transform URLs for files. The returned `path`/url is a secret-bearing read capability (carries a short-lived embedded token) — do not log or share it.",
        actions: &["get", "thumbnail", "transform"],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("node_id", "File node ID", false),
            ("preview_type", "Preview type (get)", false),
            ("transform_name", "Transform name (transform)", false),
            ("width", "Width (transform)", false),
            ("height", "Height (transform)", false),
            ("output_format", "Output format (transform)", false),
            ("size", "Size preset (transform)", false),
        ],
    },
    ToolDef {
        name: "asset",
        description: "Assets: list, list types, delete brand assets on orgs/workspaces.",
        actions: &["list", "types", "remove"],
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
        name: "task",
        description: "[legacy] Tasks: manage task lists and tasks. Legacy workflow primitive, superseded by the `workflow` orchestration tool; remains functional for now. list-lists, create-list, list-details, update-list, delete-list, list-tasks, create-task, task-details, update-task, delete-task, change-status, assign-task, bulk-status, move-task, reorder-tasks, reorder-lists, filter, summary.",
        actions: &[
            "list-lists",
            "create-list",
            "list-details",
            "update-list",
            "delete-list",
            "list-tasks",
            "create-task",
            "task-details",
            "update-task",
            "delete-task",
            "change-status",
            "assign-task",
            "bulk-status",
            "move-task",
            "reorder-tasks",
            "reorder-lists",
            "filter",
            "summary",
        ],
        params: &[
            ("profile_type", "Profile type: workspace or share", false),
            (
                "profile_id",
                "Profile ID (list-lists, create-list, reorder-lists)",
                false,
            ),
            ("list_id", "Task list ID", false),
            ("task_id", "Task ID", false),
            ("title", "Task title", false),
            ("description", "Description", false),
            (
                "status",
                "Status: pending, in_progress, complete, blocked",
                false,
            ),
            ("priority", "Priority (0-4)", false),
            ("assignee_id", "Assignee user ID", false),
            ("name", "Task list name", false),
            (
                "task_ids",
                "Comma-separated task IDs (bulk-status, reorder-tasks)",
                false,
            ),
            (
                "list_ids",
                "Comma-separated list IDs (reorder-lists)",
                false,
            ),
            ("target_task_list_id", "Target list ID (move-task)", false),
            ("sort_order", "Sort order (move-task)", false),
            (
                "node_id",
                "Node ID to link (create-task, update-task)",
                false,
            ),
            (
                "filter",
                "Filter: assigned, created, status (filter action)",
                false,
            ),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "worklog",
        description: "[legacy] Worklogs: list, append, interject, details, acknowledge, unacknowledged, list-all, filter, summary. Legacy workflow primitive, superseded by the `workflow` orchestration tool; remains functional for now.",
        actions: &[
            "list",
            "append",
            "interject",
            "details",
            "acknowledge",
            "unacknowledged",
            "list-all",
            "filter",
            "summary",
        ],
        params: &[
            (
                "entity_type",
                "Entity type: profile (default), task, task_list, or node",
                false,
            ),
            ("entity_id", "Entity ID", false),
            ("entry_id", "Worklog entry ID (details, acknowledge)", false),
            ("message", "Worklog content", false),
            (
                "profile_type",
                "Profile type: workspace or share (list-all, filter, summary)",
                false,
            ),
            (
                "profile_id",
                "Profile ID (list-all, filter, summary)",
                false,
            ),
            (
                "filter",
                "Filter: authored, interjections (filter action)",
                false,
            ),
            ("entry_type", "Entry type filter (authored filter)", false),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "approval",
        description: "[legacy] Approvals: list, request, details, approve, reject, update, delete, filter, summary, user-approvals. Legacy workflow primitive, superseded by the `workflow` orchestration tool; remains functional for now.",
        actions: &[
            "list",
            "request",
            "details",
            "approve",
            "reject",
            "update",
            "delete",
            "filter",
            "summary",
            "user-approvals",
        ],
        params: &[
            ("workspace_id", "Workspace ID (list)", false),
            (
                "profile_type",
                "Profile type: workspace or share (request, details, approve, reject, update, delete, filter, summary)",
                false,
            ),
            (
                "profile_id",
                "Profile ID. Required for request/filter/summary; for details/approve/reject/update/delete, omit to use the legacy unscoped route",
                false,
            ),
            (
                "approval_id",
                "Approval ID (details, approve, reject, update, delete)",
                false,
            ),
            (
                "entity_type",
                "Entity type: task, node, worklog_entry, share (request)",
                false,
            ),
            ("entity_id", "Entity ID (request)", false),
            ("description", "Description (request, update)", false),
            ("approver_id", "Approver user ID (request, update)", false),
            (
                "deadline",
                "Deadline YYYY-MM-DD HH:MM:SS (request, update)",
                false,
            ),
            ("node_id", "Artifact node ID (request, update)", false),
            (
                "properties",
                "Metadata properties as a JSON object (request, update)",
                false,
            ),
            ("comment", "Comment (approve, reject)", false),
            (
                "filter",
                "Filter (filter: pending/created/assigned/resolved; user-approvals: pending/created/resolved)",
                false,
            ),
            (
                "status",
                "Status filter (list, filter, user-approvals)",
                false,
            ),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "todo",
        description: "[legacy] Todos: list, create, details, update, delete, toggle, bulk-toggle, filter, summary. Legacy workflow primitive, superseded by the `workflow` orchestration tool; remains functional for now.",
        actions: &[
            "list",
            "create",
            "details",
            "update",
            "toggle",
            "delete",
            "bulk-toggle",
            "filter",
            "summary",
        ],
        params: &[
            ("profile_type", "Profile type: workspace or share", false),
            (
                "profile_id",
                "Profile ID (list, create, bulk-toggle)",
                false,
            ),
            ("todo_id", "Todo ID", false),
            ("title", "Todo title", false),
            ("assignee_id", "Assignee user ID", false),
            ("done", "Completion state (true/false)", false),
            ("todo_ids", "Comma-separated todo IDs (bulk-toggle)", false),
            (
                "filter",
                "Filter: assigned, created, done, pending (filter action)",
                false,
            ),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "apps",
        description: "MCP Apps: list, details, launch, get-tool-apps. Discover and launch interactive UI widgets.",
        actions: &["list", "details", "launch", "get-tool-apps"],
        params: &[
            ("app_id", "App identifier (details, launch)", false),
            ("tool_name", "MCP tool name (get-tool-apps)", false),
            (
                "context_type",
                "Context: workspace or share (launch)",
                false,
            ),
            ("context_id", "Context ID (launch)", false),
        ],
    },
    ToolDef {
        name: "import",
        description: "Cloud import: manage provider identities, sources, sync jobs, write-backs. Connect Google Drive, Dropbox, Box, OneDrive.",
        actions: &[
            "list-providers",
            "list-identities",
            "provision-identity",
            "identity-details",
            "revoke-identity",
            "list-sources",
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
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            (
                "provider",
                "Cloud provider: google_drive, box, onedrive_business, dropbox",
                false,
            ),
            ("identity_id", "Provider identity ID", false),
            ("source_id", "Import source ID", false),
            ("job_id", "Import job ID", false),
            ("remote_path", "Remote folder path (create-source)", false),
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
                "Action: keep or delete (disconnect)",
                false,
            ),
            (
                "status",
                "Status filter (list-sources, list-writebacks, update-source)",
                false,
            ),
            ("writeback_id", "Write-back job ID", false),
            ("node_id", "Storage node ID (push-writeback)", false),
            (
                "conflict_resolution",
                "Resolution: keep_local or keep_remote",
                false,
            ),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "lock",
        description: "File locking: acquire, status, release, heartbeat (renew) exclusive locks on files in workspaces or shares.",
        actions: &["acquire", "status", "release", "heartbeat"],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("node_id", "File node ID", false),
            ("lock_token", "Lock token (release, heartbeat)", false),
        ],
    },
    ToolDef {
        name: "metadata",
        description: "Metadata extraction: list eligible files, manage template-file mappings, AI-based matching, batch extraction, async single-file extraction (returns job_id; poll via workspace jobs-status), lexical keyword search over metadata values, and async TSV export of the caller's saved view. SIDE EFFECTS — these actions SPEND AI CREDITS: 'suggest-fields', 'auto-match', 'extract-all', 'extract', 'extract-and-wait'. The 'extract-and-wait' action enqueues a single-file extraction and polls workspace jobs-status to a terminal state before returning (the offload-friendly compound).",
        actions: &[
            "eligible",
            "add-nodes",
            "remove-nodes",
            "list-nodes",
            "auto-match",
            "extract-all",
            "extract",
            "extract-and-wait",
            "search",
            "export-view",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            (
                "template_id",
                "Metadata template ID (optional on extract/extract-and-wait; required on auto-match/extract-all/list-nodes/add-nodes/remove-nodes/export-view)",
                false,
            ),
            ("node_id", "File node ID (extract, extract-and-wait)", false),
            (
                "node_ids",
                "JSON-encoded array of node IDs (add-nodes, remove-nodes)",
                false,
            ),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
            (
                "sort_field",
                "Template field name to sort by (list-nodes)",
                false,
            ),
            (
                "sort_dir",
                "Sort direction: \"asc\" or \"desc\" (list-nodes)",
                false,
            ),
            (
                "fields",
                "JSON-encoded array of field names for partial extraction (extract, extract-and-wait, extract-all)",
                false,
            ),
            (
                "force",
                "Re-extract every mapped node even if it already has values (true/false, extract-all)",
                false,
            ),
            (
                "batch_size",
                "Optional server-clamped batch-size override (auto-match)",
                false,
            ),
            (
                "poll_interval",
                "Seconds between job-status polls, 1-60, default 3 (extract-and-wait)",
                false,
            ),
            (
                "confirm_ai_spend",
                "REQUIRED to proceed (true/false) for the AI-credit-spending actions auto-match, extract-all, extract, extract-and-wait. These actions are rejected unless confirm_ai_spend=true. Ignored by read-only actions.",
                false,
            ),
            ("query", "Search keyword(s), max 1024 chars (search)", false),
            (
                "parent_node_id",
                "Destination folder for export TSV (export-view; defaults to workspace root, max 64 chars)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "workflow",
        description: "Workflow Orchestration (v3.2): the durable multi-step runtime — distinct from the legacy task/approval/todo primitives. OFFLOAD multi-step orchestration here instead of hand-driving primitives: the compound actions 'instantiate-and-wait', 'trigger-fire-and-wait', and 'audit-export-and-download' do the full fire→poll→download loop for you. This tool exposes READ + DRIVE actions only; admin/destructive/crypto operations (workflow cancel + purge, template/pool/trigger create+lifecycle, outbound subscription management, secret/key rotation, dual-control redaction, schema set/derive, realtime token mint) are intentionally CLI-binary-only (`fastio workflow …`) — including the terminal 'cancel' lifecycle mutation, which is NOT available over MCP. Call action='describe' for the authoritative action/param reference. Idempotency keys for instantiate/fire are REQUIRED for replay safety and have no MCP auto-generate. CAS step output/advance surfaces 409 conflicts by default. The audit 'check-integrity' is integrity-only (chunk SHA-256 + content-hash chain + completeness), NOT HMAC authenticity.",
        actions: &[
            "describe",
            "get",
            "list",
            "state",
            "instantiate",
            "instantiate-and-wait",
            "pause",
            "resume",
            "grant-list",
            "step-get",
            "step-output",
            "step-advance",
            "step-occurrences",
            "template-list",
            "template-get",
            "trigger-list",
            "trigger-get",
            "trigger-fire",
            "trigger-fire-and-wait",
            "trigger-dry-run",
            "obligation-list",
            "obligation-get",
            "obligation-claim",
            "obligation-release",
            "obligation-resolve",
            "inbox-me",
            "inbox-workspace",
            "inbox-pool",
            "schema-get",
            "audit-events",
            "audit-export-start",
            "audit-export-list",
            "audit-export-get",
            "audit-export-and-download",
            "subject-workflows",
        ],
        params: &[
            ("workspace_id", "Workspace ID (19-digit)", false),
            ("workflow_id", "Workflow ID (19-digit profile id)", false),
            ("template_id", "Template revision OpaqueId", false),
            ("trigger_id", "Trigger OpaqueId", false),
            ("job_id", "Audit export job OpaqueId", false),
            ("step_occurrence_id", "Step occurrence OpaqueId", false),
            ("step_id", "Step definition OpaqueId", false),
            (
                "obligation_id",
                "Obligation id (plain numeric sequence)",
                false,
            ),
            ("subject_id", "External-subject correlation handle", false),
            ("pool_key", "Concurrency pool key", false),
            (
                "idempotency_key",
                "REQUIRED for instantiate / instantiate-and-wait / trigger-fire / trigger-fire-and-wait — replay-safe key. There is NO MCP auto-generate.",
                false,
            ),
            (
                "trigger_payload",
                "Resolved input bindings as a JSON string (instantiate / trigger-fire)",
                false,
            ),
            (
                "external_subject_id",
                "Integrator correlation handle (instantiate)",
                false,
            ),
            (
                "output",
                "Step output envelope as a JSON string (step-output / step-advance)",
                false,
            ),
            (
                "retry_on_conflict",
                "Re-read and retry once on a CAS 409 (step-output / step-advance); default false surfaces the conflict",
                false,
            ),
            (
                "role",
                "Grant role: viewer / participant / admin (grant-add — CLI only)",
                false,
            ),
            (
                "status",
                "Obligation status filter (obligation-list)",
                false,
            ),
            (
                "assigned_user_id",
                "Assigned-user filter (obligation-list)",
                false,
            ),
            (
                "resolution_payload",
                "Resolution payload as a JSON string (obligation-resolve)",
                false,
            ),
            (
                "enabled_filter",
                "Trigger filter: true / false / all (trigger-list)",
                false,
            ),
            (
                "include_payload",
                "Inline audit event payload (audit-events)",
                false,
            ),
            ("include_body", "Inline template_body (template-get)", false),
            (
                "scope",
                "Audit export scope, e.g. full (audit-export-*)",
                false,
            ),
            (
                "include_overlays",
                "Include redaction overlays (audit-export-*)",
                false,
            ),
            (
                "redaction_pin_strategy",
                "Redaction pin strategy (audit-export-*)",
                false,
            ),
            (
                "window_days",
                "Backtest window in days, ≤90 (trigger-dry-run)",
                false,
            ),
            ("sample_limit", "Sample-match cap (trigger-dry-run)", false),
            (
                "apply_guards",
                "Apply guard checks during a backtest (trigger-dry-run)",
                false,
            ),
            (
                "output_path",
                "Destination directory for downloaded bundle files (audit-export-and-download; default .fastio/downloads/)",
                false,
            ),
            (
                "poll_interval",
                "Seconds between polls, 1-60, default 3 (instantiate-and-wait / trigger-fire-and-wait)",
                false,
            ),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
            ("cursor", "Cursor for grant-list pagination", false),
        ],
    },
    ToolDef {
        name: "sign",
        description: "E-signature (SignEnvelope): draft and drive electronic-signature envelopes (PDFs sent to recipients). Every envelope is parented to a workspace (workspace_id; the former org surface was removed). This tool exposes READ + reversible-DRAFT-drive actions ONLY: envelope-create (creates a DRAFT — reversible), envelope-update (draft-only; recipients are a full replacement), envelope-list (filter via envelope_status / created_after / created_before), envelope-get, document-download (covers preview needs — the download bytes ARE the source/preview PDF, so there is no separate MCP preview action), signed-download, audit-download, describe. The OUTWARD-FACING / TERMINAL actions — send (EMAILS REAL RECIPIENTS) and void (terminal) — are intentionally CLI-binary-only (`fastio sign envelope send|void …`) and are NOT routable over MCP (mirrors how the workflow tool keeps cancel CLI-only). Envelopes are voided, not deleted — there is no delete action. Binary downloads write to the agent's local filesystem and return a path + byte count (NOT base64). Signing is a paid-plan feature (a non-entitled org returns 1670; access also requires workspace membership). Call action='describe' for the authoritative per-action reference.",
        actions: &[
            "describe",
            "envelope-create",
            "envelope-update",
            "envelope-list",
            "envelope-get",
            "document-download",
            "signed-download",
            "audit-download",
        ],
        params: &[
            // Required for every action EXCEPT describe / send / void / delete,
            // which short-circuit before workspace extraction. Marked schema-
            // optional (false) — matching the registry convention for
            // multi-action tools (e.g. `workflow`, `apps`) — with the real
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
                "UTC auto-expiry timestamp (envelope-create / -update)",
                false,
            ),
            (
                "body_json",
                "Whole create request as a JSON object STRING (envelope-create; overrides the other create params)",
                false,
            ),
            (
                "policy_json",
                "Policy bag as a JSON object STRING (envelope-create / -update)",
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
                "Field placements as a JSON array STRING (full replace on envelope-update)",
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
            ("limit", "Pagination limit (envelope-list)", false),
            ("offset", "Pagination offset (envelope-list)", false),
        ],
    },
    ToolDef {
        name: "instructions",
        description: "AI instructions (markdown blob, max 65,536 raw bytes) per profile. Scopes: user (self only), org/workspace/share (profile-wide admin slot + per-user override at /me/). User has no /me/ variant. clear-* maps to DELETE; setting empty content is equivalent.",
        actions: &[
            "get-user",
            "set-user",
            "clear-user",
            "get-org",
            "set-org",
            "clear-org",
            "get-org-user",
            "set-org-user",
            "clear-org-user",
            "get-workspace",
            "set-workspace",
            "clear-workspace",
            "get-workspace-user",
            "set-workspace-user",
            "clear-workspace-user",
            "get-share",
            "set-share",
            "clear-share",
            "get-share-user",
            "set-share-user",
            "clear-share-user",
        ],
        params: &[
            ("org_id", "Org ID (org / org-user actions)", false),
            (
                "workspace_id",
                "Workspace ID (workspace / workspace-user actions)",
                false,
            ),
            ("share_id", "Share ID (share / share-user actions)", false),
            (
                "content",
                "Markdown content for set-* actions (max 65,536 raw bytes)",
                false,
            ),
        ],
    },
    ToolDef {
        name: "system",
        description: "System health: ping (health check) and status (system status). No authentication required.",
        actions: &["ping", "status"],
        params: &[],
    },
];

// ─── Tool Router ────────────────────────────────────────────────────────────

/// Routes MCP tool calls to the appropriate handler function.
#[derive(Clone)]
pub struct ToolRouter {
    state: Arc<McpState>,
}

impl ToolRouter {
    /// Create a new tool router with shared state.
    pub fn new(state: Arc<McpState>) -> Self {
        Self { state }
    }

    /// List all registered tools as MCP `Tool` descriptors.
    pub fn list_tools() -> ListToolsResult {
        let tools = TOOL_DEFS
            .iter()
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

        match name {
            "auth" => handle_auth(&self.state, action, &args).await,
            "user" => handle_user(&self.state, action, &args).await,
            "org" => handle_org(&self.state, action, &args).await,
            "workspace" => handle_workspace(&self.state, action, &args).await,
            "files" => handle_files(&self.state, action, &args).await,
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
            "invitation" => handle_invitation(&self.state, action, &args).await,
            "preview" => handle_preview(&self.state, action, &args).await,
            "asset" => handle_asset(&self.state, action, &args).await,
            "task" => handle_task(&self.state, action, &args).await,
            "worklog" => handle_worklog(&self.state, action, &args).await,
            "approval" => handle_approval(&self.state, action, &args).await,
            "todo" => handle_todo(&self.state, action, &args).await,
            "apps" => handle_apps(&self.state, action, &args).await,
            "import" => handle_import(&self.state, action, &args).await,
            "lock" => handle_lock(&self.state, action, &args).await,
            "metadata" => handle_metadata(&self.state, action, &args).await,
            "workflow" => handle_workflow(&self.state, action, &args).await,
            "sign" => handle_sign(&self.state, action, &args).await,
            "instructions" => handle_instructions(&self.state, action, &args).await,
            "system" => handle_system(&self.state, action, &args).await,
            _ => Ok(error_text(&format!("Unknown tool: {name}"))),
        }
    }
}

// ─── Tool Handlers ──────────────────────────────────────────────────────────

/// Auth tool handler.
async fn handle_auth(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    match action {
        "signin" => handle_auth_signin(state, args).await,
        "signout" => Ok(handle_auth_signout(state, args)),
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

fn handle_auth_signout(state: &McpState, _args: &Map<String, Value>) -> CallToolResult {
    let _ = state;
    if let Ok(dir) = fastio_cli::config::Config::default_dir()
        && let Ok(mut creds_file) = CredentialsFile::load(&dir)
        && let Err(e) = creds_file.remove("default", &dir)
    {
        tracing::warn!("failed to persist credentials: {e}");
    }
    success_json(&json!({ "status": "signed_out" }))
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
        None,
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
    let client = state.client().read().await;
    match api::auth::api_key_update(
        &client,
        key_id,
        optional_str(args, "name"),
        optional_str(args, "scopes"),
    )
    .await
    {
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

async fn handle_auth_oauth_revoke_all(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match api::auth::oauth_revoke_all(&client).await {
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
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "info" => handle_user_info(state, args).await,
        "update" => handle_user_update(state, args).await,
        "search" => handle_user_search(state, args).await,
        "close" => handle_user_close(state, args).await,
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
    // display_name is an alias for first_name; merge them
    let effective_first =
        optional_str(args, "first_name").or_else(|| optional_str(args, "display_name"));
    match api::user::update_user(
        &client,
        effective_first,
        optional_str(args, "last_name"),
        None,
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

async fn handle_user_close(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let confirmation = match required_str(args, "confirmation") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::user::close_account(&client, confirmation).await {
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
/// 1696) so a subscription/credit error steers the agent to the plan surface.
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
/// The 201 create response (orgs.txt:1671) carries `setup_intent.client_secret`
/// (a real one-time secret) and a top-level `public_key`. The caller completes
/// payment via the injected `onboarding_url`, never by handling the raw secret,
/// so both are removed; `setup_intent.id` / `setup_intent.status` and the rest
/// of the response are retained. Call AFTER [`inject_onboarding_url`] so the
/// onboarding decision still sees the untouched `setup_intent` / `is_active`.
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
    match api::org::list_workspaces(&client, org_id, None, None).await {
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
    let folder_name = optional_str(args, "folder_name").unwrap_or(name);
    match api::org::create_workspace(
        &client,
        org_id,
        folder_name,
        name,
        optional_str(args, "description"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Sub-path for a single workspace-level saved view (`view-save` /
/// `view-get` / `view-delete`). Saved views are WORKSPACE-level and keyed by
/// `template_id`, NOT node-scoped — `metadata_api` prepends
/// `/workspace/{id}/`. Contract: storage.txt saved-view section, ai.txt:2735.
const METADATA_VIEW_SUBPATH: &str = "metadata/view/";
/// Sub-path for listing the caller's workspace-level saved views.
const METADATA_VIEWS_SUBPATH: &str = "metadata/views/";

/// Build the form-field map for an MCP `workspace` update from the tool args.
///
/// Forwards the advertised `intelligence` toggle as the string `"true"`/
/// `"false"` (the `/workspace/{id}/update/` endpoint takes it as a string form
/// field — workspaces.txt) so AI indexing can be toggled through MCP.
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
            let folder_name = optional_str(args, "folder_name").unwrap_or(name);
            match api::workspace::create_workspace(
                &client,
                &api::workspace::CreateWorkspaceParams {
                    org_id,
                    folder_name,
                    name,
                    description: optional_str(args, "description"),
                    intelligence: optional_bool(args, "intelligence"),
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
        "enable-workflow" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::enable_workflow(&client, ws_id).await {
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
            // Decoupled (Phase 3) from `api::workspace::search_workspace`
            // (removed): route through the single `search_files` builder so
            // the action keeps the standard file-search shape.
            let params = api::storage::SearchFilesParams::new()
                .limit(optional_u32(args, "limit"))
                .offset(optional_u32(args, "offset"));
            match api::storage::search_files(&client, ws_id, query, params).await {
                // Normalize the node-id-keyed `files` MAP into a one-row-per-file
                // ARRAY so MCP renders the search result the same way the CLI
                // does (CLI/MCP parity).
                Ok(v) => Ok(success_json(&api::storage::normalize_search_response(v))),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "limits" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Limits are part of workspace details
            match api::workspace::get_workspace(&client, ws_id).await {
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
            match api::workspace::list_workspace_shares(
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
            let parent = optional_str(args, "parent_id").unwrap_or("root");
            let name = match required_str(args, "name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // `content` is required by `createnote/` (storage.txt:553).
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
            match api::workspace::update_note(
                &client,
                ws_id,
                node_id,
                optional_str(args, "name"),
                optional_str(args, "content"),
                optional_str(args, "if_version_id"),
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
            match api::workspace::read_note(
                &client,
                ws_id,
                node_id,
                optional_str(args, "version_id"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "quickshare-get" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::quickshare_get(&client, ws_id, node_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "quickshare-delete" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::quickshare_delete(&client, ws_id, node_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "quickshares-list" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::quickshares_list(
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
        "disable-workflow" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::disable_workflow(&client, ws_id).await {
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
        "metadata-template-categories" => {
            match api::workspace::metadata_api(
                &client,
                "",
                "metadata/templates/categories/",
                "GET",
                None,
                None,
                None,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-create" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let name = match required_str(args, "name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let desc = match required_str(args, "description") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let cat = match required_str(args, "category") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fields = match required_str(args, "fields") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::metadata::create_template(&client, ws_id, name, desc, cat, fields).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-preview-match" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let name = match required_str(args, "name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let desc = match required_str(args, "description") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::metadata::preview_match(&client, ws_id, name, desc).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-suggest-fields" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_ids = match required_str(args, "node_ids") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let desc = match required_str(args, "description") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let user_ctx = optional_str(args, "user_context");
            match api::metadata::suggest_fields(&client, ws_id, node_ids, desc, user_ctx).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-delete" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!("metadata/templates/{}/", urlencoding::encode(tid));
            match api::workspace::metadata_api(&client, ws_id, &sub, "DELETE", None, None, None)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-list" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = if let Some(f) = optional_str(args, "filter") {
                format!("metadata/templates/list/{}/", urlencoding::encode(f))
            } else {
                "metadata/templates/list/".to_owned()
            };
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None, None).await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-details" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!("metadata/templates/{}/details/", urlencoding::encode(tid));
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None, None).await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-settings" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Contract (ai.txt:2076-2094) is form-encoded with stringy
            // `enabled` ("true"/"false") and `priority` ("1".."5").
            let mut form: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            if let Some(v) = optional_bool(args, "enabled") {
                form.insert("enabled".to_owned(), v.to_string());
            }
            if let Some(v) = optional_u8(args, "priority") {
                form.insert("priority".to_owned(), v.to_string());
            }
            let sub = format!("metadata/templates/{}/settings/", urlencoding::encode(tid));
            match api::workspace::metadata_api(
                &client,
                ws_id,
                &sub,
                "POST",
                None,
                Some(&form),
                None,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-update" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let is_copy = optional_bool(args, "copy").unwrap_or(false);
            let sub = if is_copy {
                format!(
                    "metadata/templates/{}/update/create/",
                    urlencoding::encode(tid)
                )
            } else {
                format!("metadata/templates/{}/update/", urlencoding::encode(tid))
            };
            // Contract (ai.txt:2102-2129) is form-encoded; every field is
            // optional and only provided fields are updated. `fields` is a
            // JSON-encoded array passed through as a form value.
            let mut form: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            if let Some(v) = optional_str(args, "name") {
                form.insert("name".to_owned(), v.to_owned());
            }
            if let Some(v) = optional_str(args, "description") {
                form.insert("description".to_owned(), v.to_owned());
            }
            if let Some(v) = optional_str(args, "category") {
                form.insert("category".to_owned(), v.to_owned());
            }
            if let Some(v) = optional_str(args, "fields") {
                form.insert("fields".to_owned(), v.to_owned());
            }
            match api::workspace::metadata_api(
                &client,
                ws_id,
                &sub,
                "POST",
                None,
                Some(&form),
                None,
            )
            .await
            {
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
            // Forward the documented `keys` parameter (ai.txt:2600-2612) as a
            // query string on the DELETE. `keys` is a JSON-encoded array of
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
            if unique.len() == 1 {
                match api::metadata::get_node_metadata_details(&client, ws_id, &unique[0]).await {
                    Ok(v) => Ok(success_json(&v)),
                    Err(e) => Ok(cli_err_to_result(&e)),
                }
            } else {
                match api::metadata::get_bulk_node_metadata_details(&client, ws_id, &unique).await {
                    Ok(resp) => {
                        let payload = serde_json::json!({
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
            let tid = optional_str(args, "template_id").filter(|s| !s.trim().is_empty());
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            match api::metadata::extract_node_metadata(&client, ws_id, nid, tid, fields).await {
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
            let tid = optional_str(args, "template_id").filter(|s| !s.trim().is_empty());
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            let poll_interval = optional_u64(args, "poll_interval");
            Ok(metadata_extract_and_wait(&client, ws_id, nid, tid, fields, poll_interval).await)
        }
        "metadata-list" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!(
                "storage/{}/metadata/list/{}/",
                urlencoding::encode(nid),
                urlencoding::encode(tid)
            );
            let mut params = std::collections::HashMap::new();
            if let Some(v) = optional_str(args, "filters") {
                params.insert("filters".to_owned(), v.to_owned());
            }
            if let Some(v) = optional_str(args, "order_by") {
                params.insert("order_by".to_owned(), v.to_owned());
            }
            if let Some(v) = optional_bool(args, "order_desc") {
                params.insert("order_desc".to_owned(), v.to_string());
            }
            let p = if params.is_empty() {
                None
            } else {
                Some(&params)
            };
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None, p).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-template-select" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!(
                "storage/{}/metadata/template_select/",
                urlencoding::encode(nid)
            );
            match api::workspace::metadata_api(&client, ws_id, &sub, "POST", None, None, None).await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-templates-in-use" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!("storage/{}/metadata/templates/", urlencoding::encode(nid));
            let mut params = std::collections::HashMap::new();
            if let Some(v) = optional_str(args, "filters") {
                params.insert("filters".to_owned(), v.to_owned());
            }
            if let Some(v) = optional_str(args, "order_by") {
                params.insert("order_by".to_owned(), v.to_owned());
            }
            if let Some(v) = optional_bool(args, "order_desc") {
                params.insert("order_desc".to_owned(), v.to_string());
            }
            let p = if params.is_empty() {
                None
            } else {
                Some(&params)
            };
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None, p).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-update" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let kv = match required_str(args, "key_values") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!(
                "storage/{}/metadata/update/{}/",
                urlencoding::encode(nid),
                urlencoding::encode(tid)
            );
            // Contract (ai.txt:2073-2118): form-encoded `key_values` (a
            // JSON-encoded object passed through verbatim as the form value).
            let mut form: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            form.insert("key_values".to_owned(), kv.to_owned());
            match api::workspace::metadata_api(
                &client,
                ws_id,
                &sub,
                "POST",
                None,
                Some(&form),
                None,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-view-save" => {
            // Saved views are WORKSPACE-level and keyed by `template_id`
            // (NOT node-scoped). Contract: storage.txt saved-view section +
            // ai.txt:2735-2744. POST /workspace/{id}/metadata/view/ takes a
            // form-encoded body of `template_id` + `config` (a JSON-encoded
            // string holding the view config). A JSON body returns 406.
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let config = match required_str(args, "config") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let mut form: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            form.insert("template_id".to_owned(), tid.to_owned());
            form.insert("config".to_owned(), config.to_owned());
            match api::workspace::metadata_api(
                &client,
                ws_id,
                METADATA_VIEW_SUBPATH,
                "POST",
                None,
                Some(&form),
                None,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-view-get" => {
            // GET /workspace/{id}/metadata/view/?template_id={tid} — the
            // caller's saved view for a single template (1609 when absent).
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let mut params: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            params.insert("template_id".to_owned(), tid.to_owned());
            match api::workspace::metadata_api(
                &client,
                ws_id,
                METADATA_VIEW_SUBPATH,
                "GET",
                None,
                None,
                Some(&params),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-view-delete" => {
            // DELETE /workspace/{id}/metadata/view/?template_id={tid} — removes
            // only the caller's own view for that template.
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let tid = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let mut params: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            params.insert("template_id".to_owned(), tid.to_owned());
            match api::workspace::metadata_api(
                &client,
                ws_id,
                METADATA_VIEW_SUBPATH,
                "DELETE",
                None,
                None,
                Some(&params),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-views-list" => {
            // GET /workspace/{id}/metadata/views/ — every saved view the
            // caller owns in this workspace. Workspace-level, no node id.
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workspace::metadata_api(
                &client,
                ws_id,
                METADATA_VIEWS_SUBPATH,
                "GET",
                None,
                None,
                None,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
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
        "quickshare" => handle_files_quickshare(state, args).await,
        _ => Ok(error_text(&format!("Unknown files action: {action}"))),
    }
}

async fn handle_files_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let folder = optional_str(args, "folder").unwrap_or("root");
    match api::storage::list_files(
        &client,
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
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::get_file_details(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_create_folder(
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
    let parent = optional_str(args, "folder").unwrap_or("root");
    match api::storage::create_folder(&client, ws_id, parent, name).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}
async fn handle_files_move(
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
    let to = match required_str(args, "to") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::move_node(&client, ws_id, node_id, to).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_copy(
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
    let to = match required_str(args, "to") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::copy_node(&client, ws_id, node_id, to).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_rename(
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
    let new_name = match required_str(args, "new_name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::rename_node(&client, ws_id, node_id, new_name).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_delete(
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
    match api::storage::delete_node(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_restore(
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
    match api::storage::restore_node(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_purge(
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
    match api::storage::purge_node(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_trash(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::list_trash(
        &client,
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
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::list_versions(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_search(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let query = match required_str(args, "query") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let params = build_search_files_params(args);
    match api::storage::search_files(&client, ws_id, query, params).await {
        // Normalize the node-id-keyed `files` MAP into a one-row-per-file ARRAY
        // so MCP matches the CLI search renderer (CLI/MCP parity). The
        // `details` (results[]) shape is left untouched by the normalizer.
        Ok(v) => Ok(success_json(&api::storage::normalize_search_response(v))),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Build [`api::storage::SearchFilesParams`] from MCP tool arguments, shared by
/// `handle_files_search` and the re-pointed `handle_ai_search`.
fn build_search_files_params(args: &Map<String, Value>) -> api::storage::SearchFilesParams<'_> {
    api::storage::SearchFilesParams::new()
        .files_scope(optional_str(args, "files_scope"))
        .folders_scope(optional_str(args, "folders_scope"))
        .limit(optional_u32(args, "limit"))
        .offset(optional_u32(args, "offset"))
        .details(optional_bool(args, "details").unwrap_or(false))
        .output(optional_str(args, "output"))
}

async fn handle_files_recent(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ws_id = match required_str(args, "workspace_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::list_recent(&client, ws_id, None, None).await {
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
    let ws_id = match required_str(args, "workspace_id") {
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
    match api::storage::version_restore(&client, ws_id, node_id, version_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_lock_acquire(
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
    match api::storage::lock_acquire(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_lock_status(
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
    match api::storage::lock_status(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_lock_release(
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
    let lock_token = match required_str(args, "lock_token") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::storage::lock_release(&client, ws_id, node_id, lock_token).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_read(
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
    match api::storage::read_content(&client, ws_id, node_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_files_quickshare(
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
    match api::storage::quickshare_get(&client, ws_id, node_id).await {
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
    let folder = optional_str(args, "folder").unwrap_or("root");
    // Create session, upload as single chunk, complete
    let content_bytes = content.as_bytes().to_vec();
    let size = u64::try_from(content_bytes.len()).unwrap_or(u64::MAX);
    match api::upload::create_upload_session(&client, ws_id, folder, name, size).await {
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
    let folder = optional_str(args, "folder").unwrap_or("root");
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
    let folder = optional_str(args, "folder").unwrap_or("root");
    match api::upload::create_upload_session(&client, ws_id, folder, name, filesize).await {
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
    let chunk_num = optional_u32(args, "chunk_num").unwrap_or(0);
    match api::upload::chunk_delete(&client, upload_key, chunk_num).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_web_list(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::upload::web_list(&client).await {
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
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::upload::upload_limits(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_upload_extensions(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::upload::upload_extensions(&client).await {
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
    let folder = optional_str(args, "folder").unwrap_or("root");
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
            api::upload::create_stream_session(&client, ws_id, folder, name, max_size).await;
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
    let folder = optional_str(args, "folder").unwrap_or("root");
    let max_size = optional_str(args, "max_size").and_then(|s| s.parse::<u64>().ok());
    match api::upload::create_stream_session(&client, ws_id, folder, name, max_size).await {
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
            let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
            let ctx_id = match required_str(args, "context_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_id = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::download::get_download_url_ctx(
                &client,
                ctx_type,
                ctx_id,
                node_id,
                optional_str(args, "version_id"),
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
            let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
        "quickshare-details" => {
            let qs_id = match required_str(args, "quickshare_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::download::quickshare_details(&client, qs_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
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
        "quickshare-create" => handle_share_quickshare_create(state, args).await,
        "available" => handle_share_available(state, args).await,
        "check-name" => handle_share_check_name(state, args).await,
        "enable-workflow" => handle_share_enable_workflow(state, args).await,
        "disable-workflow" => handle_share_disable_workflow(state, args).await,
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
            description: optional_str(args, "description"),
            access_options: optional_str(args, "access_options"),
            password: optional_str(args, "password"),
            anonymous_uploads_enabled: optional_bool(args, "anonymous_uploads_enabled"),
            intelligence: optional_bool(args, "intelligence"),
            download_security,
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
            description: optional_str(args, "description"),
            access_options: optional_str(args, "access_options"),
            download_enabled: optional_bool(args, "download_enabled"),
            comments_enabled: optional_bool(args, "comments_enabled"),
            anonymous_uploads_enabled: optional_bool(args, "anonymous_uploads_enabled"),
            download_security,
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
    let folder = optional_str(args, "folder").unwrap_or("root");
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
    match api::share::add_share_member(&client, share_id, email, optional_str(args, "role")).await {
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

async fn handle_share_quickshare_create(
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
    match api::share::create_quickshare(
        &client,
        ws_id,
        node_id,
        optional_str(args, "expires"),
        optional_str(args, "expires_at"),
    )
    .await
    {
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

async fn handle_share_enable_workflow(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::enable_share_workflow(&client, share_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_share_disable_workflow(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let share_id = match required_str(args, "share_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::share::disable_share_workflow(&client, share_id).await {
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
        "search" => handle_ai_search(state, args).await,
        "memory-get" => handle_ai_memory_get(state, args).await,
        "memory-set" => handle_ai_memory_set(state, args).await,
        "memory-delete" => handle_ai_memory_delete(state, args).await,
        _ => Ok(error_text(&format!("Unknown ripley action: {action}"))),
    }
}

/// Guard the `/ai/agent/` mutual-exclusion rule: `files_attach` cannot be
/// combined with `files_scope`/`folders_scope` in the same request — the
/// server rejects both with `1605` (ai.txt:115,311,609). Returns an error
/// `CallToolResult` to surface to the agent if the combination is present.
fn check_files_attach_exclusion(args: &Map<String, Value>) -> Option<CallToolResult> {
    let has_attach = optional_str(args, "files_attach").is_some();
    let has_scope = optional_str(args, "files_scope").is_some()
        || optional_str(args, "folders_scope").is_some();
    if has_attach && has_scope {
        Some(error_text(
            "files_attach cannot be combined with files_scope/folders_scope — \
             use one or the other",
        ))
    } else {
        None
    }
}

async fn handle_ai_chat_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let chat_type = optional_str(args, "type").unwrap_or("chat");
    // The `/ai/agent/` create endpoint is form-encoded; emit only the
    // documented field set (no retired `nodes`/`folder_id`/`intelligence`).
    let mut form = std::collections::HashMap::new();
    form.insert("type".to_owned(), chat_type.to_owned());
    form.insert(
        "personality".to_owned(),
        optional_str(args, "personality")
            .unwrap_or("detailed")
            .to_owned(),
    );
    // `question` is REQUIRED for chat create (ai.txt:265). Reject a create
    // that omits it rather than silently sending a question-less body.
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
    if let Some(err) = check_files_attach_exclusion(args) {
        return Ok(err);
    }
    if let Some(v) = optional_str(args, "files_scope") {
        form.insert("files_scope".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "folders_scope") {
        form.insert("folders_scope".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "files_attach") {
        form.insert("files_attach".to_owned(), v.to_owned());
    }
    match api::ai::ai_api_form(&client, ctx_type, ctx_id, "agent/", &form).await {
        Ok(mut v) => {
            for w in &warnings {
                attach_warning(&mut v, Some(w));
            }
            Ok(success_json(&v))
        }
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_chat_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    // `kind` filters by chat kind (`user`/`agent`/`all`, ai.txt:331).
    if let Some(k) = optional_str(args, "kind") {
        params.insert("kind".to_owned(), k.to_owned());
    }
    let sub = if optional_bool(args, "include_deleted").unwrap_or(false) {
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_chat_cancel(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    // Follow-up messages are form-encoded; `type` is inherited from the chat.
    let mut form = std::collections::HashMap::new();
    form.insert("question".to_owned(), query.to_owned());
    if let Some(v) = optional_str(args, "personality") {
        form.insert("personality".to_owned(), v.to_owned());
    }
    if let Some(err) = check_files_attach_exclusion(args) {
        return Ok(err);
    }
    if let Some(v) = optional_str(args, "files_scope") {
        form.insert("files_scope".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "folders_scope") {
        form.insert("folders_scope".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "files_attach") {
        form.insert("files_attach".to_owned(), v.to_owned());
    }
    let sub = format!("agent/{}/message/", urlencoding::encode(chat_id));
    match api::ai::ai_api_form(&client, ctx_type, ctx_id, &sub, &form).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_message_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
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
    // The `/ai/share/` endpoint caps `files` at 1-25 (ai.txt:894); reject
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // AI transactions is WORKSPACE-ONLY (ai.txt:935-981) — there is no share
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Autotitle is SHARE-ONLY (ai.txt:1079-1112). Reject a workspace (or any
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

async fn handle_ai_search(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // Accept either `query_text` (legacy `/ai/search/` name) or `search`
    // (the storage-search name) so old callers keep working. Falls back to
    // `required_str` so a missing query yields the standard missing-arg error.
    let query = match optional_str(args, "query_text").or_else(|| optional_str(args, "search")) {
        Some(v) => v,
        None => match required_str(args, "query_text") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        },
    };
    // Re-pointed (Phase 3) off the deprecated `/ai/search/` onto the single
    // `api::storage::search_files` builder (`/storage/search/`).
    let params = build_search_files_params(args);
    let result = match ctx_type {
        "share" => api::storage::search_files_share(&client, ctx_id, query, params).await,
        _ => api::storage::search_files(&client, ctx_id, query, params).await,
    };
    match result {
        // Normalize the node-id-keyed `files` MAP into a one-row-per-file ARRAY
        // so MCP matches the CLI `ai search` renderer (CLI/MCP parity). Both the
        // workspace and share search contracts share the files-map shape.
        Ok(v) => Ok(success_json(&api::storage::normalize_search_response(v))),
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let question = match required_str(args, "query_text") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    if let Some(err) = check_files_attach_exclusion(args) {
        return Ok(err);
    }
    let mut form = std::collections::HashMap::new();
    form.insert("type".to_owned(), "chat_with_files".to_owned());
    form.insert("question".to_owned(), question.to_owned());
    form.insert(
        "personality".to_owned(),
        optional_str(args, "personality")
            .unwrap_or("detailed")
            .to_owned(),
    );
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
    if let Some(v) = optional_str(args, "files_scope") {
        form.insert("files_scope".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "folders_scope") {
        form.insert("folders_scope".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "files_attach") {
        form.insert("files_attach".to_owned(), v.to_owned());
    }

    let resp = match api::ai::ai_api_form(&client, ctx_type, ctx_id, "agent/", &form).await {
        Ok(v) => v,
        Err(e) => return Ok(cli_err_to_result(&e)),
    };

    let chat_id = resp
        .get("chat_id")
        .or_else(|| resp.get("chat").and_then(|c| c.get("id")))
        .or_else(|| resp.get("id"))
        .and_then(json_value_id_to_string);
    let message_id = resp
        .get("message_id")
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

    if optional_bool(args, "no_wait").unwrap_or(false) {
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
                let msg = msg_data.get("message").unwrap_or(&msg_data);
                // `state` may be a string OR a numeric JSON value; normalise via
                // the same string-or-numeric extraction the CLI wait loop uses.
                let state_str = json_value_field_to_string(msg, "state").unwrap_or_default();
                if state_str == "complete" || state_str == "errored" {
                    let mut msg_data = msg_data;
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

/// Read a JSON object field and normalise it (string OR numeric) to `String`.
///
/// Mirrors the CLI `extract_string_field` helper so the MCP `ask` wait loop
/// reads `state` identically whether the server returns it as a string or a
/// numeric JSON value.
fn json_value_field_to_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(json_value_id_to_string)
}

/// Resolve the AI-memory scope from the MCP `context_type` arg, which for
/// memory actions accepts `org` or `workspace` (NOT `share` — memory has no
/// share scope). Returns an error result for any other value.
fn resolve_memory_scope(
    args: &Map<String, Value>,
) -> Result<fastio_cli::api::ai_memory::MemoryScope, CallToolResult> {
    match optional_str(args, "context_type").unwrap_or("workspace") {
        "org" => Ok(fastio_cli::api::ai_memory::MemoryScope::Org),
        "workspace" => Ok(fastio_cli::api::ai_memory::MemoryScope::Workspace),
        other => Err(error_text(&format!(
            "memory context_type must be \"org\" or \"workspace\", got {other:?}"
        ))),
    }
}

async fn handle_ai_memory_get(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let scope = match resolve_memory_scope(args) {
        Ok(s) => s,
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::ai_memory::get(&client, scope, ctx_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_memory_set(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let scope = match resolve_memory_scope(args) {
        Ok(s) => s,
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let content = match required_str(args, "content") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    // `revision` is optional. Distinguish three cases:
    //   - key ABSENT              → unconditional (last-writer-wins) write
    //   - present, valid u64       → conditional (optimistic-concurrency) write
    //   - present, NOT a valid u64 → reject BEFORE the write. This includes an
    //     explicit `null`. Only a truly absent key means "unconditional"; a
    //     present-but-invalid value (null, bool, float, object, array,
    //     non-numeric string) is rejected. Silently dropping such a value —
    //     including treating `null` as absent — would downgrade an intended
    //     conditional write to an unconditional one (lost-update risk;
    //     orgs.txt:2265).
    // Accept both a JSON number and a numeric string (mirrors other MCP args).
    let revision = match args.get("revision") {
        None => None,
        Some(v) => {
            let parsed = v
                .as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()));
            match parsed {
                Some(rev) => Some(rev),
                None => {
                    return Ok(error_text(
                        "revision must be a non-negative integer (a number or numeric string)",
                    ));
                }
            }
        }
    };
    match api::ai_memory::set(&client, scope, ctx_id, content, revision).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_ai_memory_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let scope = match resolve_memory_scope(args) {
        Ok(s) => s,
        Err(e) => return Ok(e),
    };
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::ai_memory::delete(&client, scope, ctx_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
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
    let entity_type = optional_str(args, "entity_type").unwrap_or("workspace");
    match action {
        "list" => handle_member_list(state, args, entity_type).await,
        "add" => handle_member_add(state, args, entity_type).await,
        "remove" => handle_member_remove(state, args, entity_type).await,
        "update" => handle_member_update(state, args, entity_type).await,
        "info" => handle_member_info(state, args, entity_type).await,
        "transfer-ownership" => handle_member_transfer_ownership(state, args, entity_type).await,
        "leave" => handle_member_leave(state, args, entity_type).await,
        "join" => handle_member_join(state, args, entity_type).await,
        "join-invitation" => handle_member_join_invitation(state, args).await,
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

async fn handle_member_join_invitation(
    state: &McpState,
    args: &Map<String, Value>,
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
    match api::member::join_invitation(&client, entity_id, key, inv_action).await {
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
        "delete" => handle_comment_delete(state, args).await,
        "list-all" => handle_comment_list_all(state, args).await,
        "details" => handle_comment_details(state, args).await,
        "bulk-delete" => handle_comment_bulk_delete(state, args).await,
        "reaction-add" => handle_comment_reaction_add(state, args).await,
        "reaction-remove" => handle_comment_reaction_remove(state, args).await,
        "link" => handle_comment_link(state, args).await,
        "unlink" => handle_comment_unlink(state, args).await,
        "linked" => handle_comment_linked(state, args).await,
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
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::list_comments(
        &client,
        &api::comment::ListCommentsParams {
            entity_type,
            entity_id,
            node_id,
            sort: optional_str(args, "sort"),
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

async fn handle_comment_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let entity_type = match required_str(args, "entity_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
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
    match api::comment::add_comment(&client, entity_type, entity_id, node_id, text, None).await {
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
        entity_type,
        entity_id,
        node_id,
        text,
        Some(comment_id),
    )
    .await
    {
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
    let entity_id = match required_str(args, "entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::list_all_comments(
        &client,
        entity_type,
        entity_id,
        optional_str(args, "sort"),
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
    let ids_str = match required_str(args, "comment_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids: Vec<String> = ids_str.split(',').map(|s| s.trim().to_owned()).collect();
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

async fn handle_comment_link(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let etype = match required_str(args, "linked_entity_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let eid = match required_str(args, "linked_entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::link_comment(&client, comment_id, etype, eid).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_unlink(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let comment_id = match required_str(args, "comment_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::unlink_comment(&client, comment_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_comment_linked(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let etype = match required_str(args, "linked_entity_type") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let eid = match required_str(args, "linked_entity_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::comment::linked_comments(&client, etype, eid).await {
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
            match api::event::search_events(
                &client,
                &api::event::SearchEventsParams {
                    workspace_id: optional_str(args, "workspace_id"),
                    share_id: optional_str(args, "share_id"),
                    user_id: optional_str(args, "user_id"),
                    org_id: optional_str(args, "org_id"),
                    event: optional_str(args, "event"),
                    category: optional_str(args, "category"),
                    subcategory: optional_str(args, "subcategory"),
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

/// Invitation tool handler.
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
                Some("accepted"),
                None,
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
                Some("declined"),
                None,
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
/// The `get` / `thumbnail` preauthorize response (storage.txt:2343) carries
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
            let preview_type = if action == "thumbnail" {
                "thumbnail"
            } else {
                optional_str(args, "preview_type").unwrap_or("document")
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
        "transform" => {
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
            let transform = match required_str(args, "transform_name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::preview::get_transform_url(
                &client,
                &api::preview::TransformUrlParams {
                    context_type: ctx_type,
                    context_id: ctx_id,
                    node_id,
                    transform_name: transform,
                    width: optional_u32(args, "width"),
                    height: optional_u32(args, "height"),
                    output_format: optional_str(args, "output_format"),
                    size: optional_str(args, "size"),
                },
            )
            .await
            {
                // The `transform` requestread response (storage.txt:2503) is
                // `{result, token}` with NO tokenized `path`/url — the bare
                // `token` IS the sole deliverable the agent uses to build the
                // read URL. Stripping it would break the tool, so it is kept
                // (an accepted secret deliverable; redaction is handled by
                // SECRET_LOG_KEYS on the trace side).
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown preview action: {action}"))),
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

/// Task tool handler.
async fn handle_task(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "list-tasks" => handle_task_list_tasks(state, args).await,
        "create-task" => handle_task_create_task(state, args).await,
        "task-details" => handle_task_task_details(state, args).await,
        "update-task" => handle_task_update_task(state, args).await,
        "delete-task" => handle_task_delete_task(state, args).await,
        "assign-task" => handle_task_assign_task(state, args).await,
        "change-status" => handle_task_change_status(state, args).await,
        "list-lists" => handle_task_list_lists(state, args).await,
        "create-list" => handle_task_create_list(state, args).await,
        "list-details" => handle_task_list_details(state, args).await,
        "update-list" => handle_task_update_list(state, args).await,
        "delete-list" => handle_task_delete_list(state, args).await,
        "bulk-status" => handle_task_bulk_status(state, args).await,
        "move-task" => handle_task_move_task(state, args).await,
        "reorder-tasks" => handle_task_reorder_tasks(state, args).await,
        "reorder-lists" => handle_task_reorder_lists(state, args).await,
        "filter" => handle_task_filter(state, args).await,
        "summary" => handle_task_summary(state, args).await,
        _ => Ok(error_text(&format!("Unknown task action: {action}"))),
    }
}

/// `task` filter action: filtered task list for a workspace or share.
async fn handle_task_filter(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
    let profile_id = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let filter = match required_str(args, "filter") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let query = api::workflow::FilterQuery {
        limit: optional_u32(args, "limit"),
        offset: optional_u32(args, "offset"),
        status: optional_str(args, "status"),
        entry_type: None,
    };
    match api::workflow::list_tasks_filtered(&client, profile_type, profile_id, filter, &query)
        .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `task` summary action: task count summary for a workspace or share.
async fn handle_task_summary(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
    let profile_id = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::tasks_summary(&client, profile_type, profile_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_list_tasks(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::list_tasks(
        &client,
        list_id,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_create_task(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let title = match required_str(args, "title") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::create_task(
        &client,
        &api::workflow::CreateTaskParams {
            list_id,
            title,
            description: optional_str(args, "description"),
            status: optional_str(args, "status"),
            priority: optional_u8(args, "priority"),
            assignee_id: optional_str(args, "assignee_id"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_task_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let task_id = match required_str(args, "task_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::get_task(&client, list_id, task_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_update_task(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let task_id = match required_str(args, "task_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::update_task(
        &client,
        &api::workflow::UpdateTaskParams {
            list_id,
            task_id,
            title: optional_str(args, "title"),
            description: optional_str(args, "description"),
            status: optional_str(args, "status"),
            priority: optional_u8(args, "priority"),
            assignee_id: optional_str(args, "assignee_id"),
        },
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_delete_task(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let task_id = match required_str(args, "task_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::delete_task(&client, list_id, task_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_assign_task(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let task_id = match required_str(args, "task_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::assign_task(&client, list_id, task_id, optional_str(args, "assignee_id"))
        .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_change_status(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let task_id = match required_str(args, "task_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let status = match required_str(args, "status") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::change_task_status(&client, list_id, task_id, status).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_list_lists(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let pt = optional_str(args, "profile_type").unwrap_or("workspace");
    let pid = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::list_task_lists_ctx(
        &client,
        pt,
        pid,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_create_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let pt = optional_str(args, "profile_type").unwrap_or("workspace");
    let pid = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let name = match required_str(args, "name") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::create_task_list_ctx(
        &client,
        pt,
        pid,
        name,
        optional_str(args, "description"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_list_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::get_task_list(&client, list_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_update_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::update_task_list(
        &client,
        list_id,
        optional_str(args, "name"),
        optional_str(args, "description"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_delete_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::delete_task_list(&client, list_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_bulk_status(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids_str = match required_str(args, "task_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let status = match required_str(args, "status") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids: Vec<String> = ids_str.split(',').map(|s| s.trim().to_owned()).collect();
    match api::workflow::bulk_status_tasks(&client, list_id, &ids, status).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_move_task(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let task_id = match required_str(args, "task_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let target = match required_str(args, "target_task_list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::move_task(
        &client,
        list_id,
        task_id,
        target,
        optional_u32(args, "sort_order"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_reorder_tasks(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let list_id = match required_str(args, "list_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids_str = match required_str(args, "task_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids: Vec<String> = ids_str.split(',').map(|s| s.trim().to_owned()).collect();
    match api::workflow::reorder_tasks(&client, list_id, &ids).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_task_reorder_lists(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let pt = optional_str(args, "profile_type").unwrap_or("workspace");
    let pid = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids_str = match required_str(args, "list_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids: Vec<String> = ids_str.split(',').map(|s| s.trim().to_owned()).collect();
    match api::workflow::reorder_task_lists(&client, pt, pid, &ids).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Resolve the worklog `entity_type`, defaulting to `profile` (NOT
/// `workspace`, which is not a valid worklog entity type). The accepted set is
/// `profile`, `task`, `task_list`, and `node`; the value passes through to the
/// API unchanged so `node` is honored.
fn worklog_entity_type(args: &Map<String, Value>) -> &str {
    optional_str(args, "entity_type").unwrap_or("profile")
}

/// Worklog tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_worklog(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    // Per the worklog contract the entity-scoped endpoints accept
    // `profile` (default), `task`, `task_list`, and `node`.
    let entity_type = worklog_entity_type(args);
    match action {
        "list" => {
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::list_worklogs(
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
        "append" => {
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let message = match required_str(args, "message") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::append_worklog(&client, entity_type, entity_id, message).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "interject" => {
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let message = match required_str(args, "message") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::interject_worklog(&client, entity_type, entity_id, message).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "details" => {
            let entry_id = match required_str(args, "entry_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::worklog_details(&client, entry_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "acknowledge" => {
            let entry_id = match required_str(args, "entry_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::acknowledge_worklog(&client, entry_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "unacknowledged" => {
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::unacknowledged_worklogs(
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
        "list-all" => {
            let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
            let profile_id = match required_str(args, "profile_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let query = api::workflow::FilterQuery {
                limit: optional_u32(args, "limit"),
                offset: optional_u32(args, "offset"),
                status: None,
                entry_type: None,
            };
            match api::workflow::list_worklogs_ctx(&client, profile_type, profile_id, &query).await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "filter" => {
            let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
            let profile_id = match required_str(args, "profile_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let filter = match required_str(args, "filter") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let query = api::workflow::FilterQuery {
                limit: optional_u32(args, "limit"),
                offset: optional_u32(args, "offset"),
                status: None,
                entry_type: optional_str(args, "entry_type"),
            };
            match api::workflow::list_worklogs_filtered(
                &client,
                profile_type,
                profile_id,
                filter,
                &query,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "summary" => {
            let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
            let profile_id = match required_str(args, "profile_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::worklogs_summary(&client, profile_type, profile_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown worklog action: {action}"))),
    }
}

/// Approval tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_approval(
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
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::list_approvals(
                &client,
                ws_id,
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
        "request" => {
            let entity_type = match required_str(args, "entity_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let entity_id = match required_str(args, "entity_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let description = match required_str(args, "description") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            // Accept `profile_id` (scoped) and fall back to the legacy
            // `workspace_id` arg name; default the type to workspace.
            let profile_id = match approval_profile_id(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
            let properties = match approval_properties(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = api::workflow::CreateApprovalParams {
                profile_type,
                profile_id,
                entity_type,
                entity_id,
                description,
                approver_id: optional_str(args, "approver_id"),
                deadline: optional_str(args, "deadline"),
                node_id: optional_str(args, "node_id"),
                properties: properties.as_ref(),
            };
            match api::workflow::create_approval(&client, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "details" => {
            let scope = approval_scope_opt(args);
            let approval_id = match required_str(args, "approval_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::get_approval(&client, scope, approval_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "approve" | "reject" => {
            let scope = approval_scope_opt(args);
            let approval_id = match required_str(args, "approval_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::resolve_approval(
                &client,
                scope,
                approval_id,
                action,
                optional_str(args, "comment"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "update" => {
            let scope = approval_scope_opt(args);
            let approval_id = match required_str(args, "approval_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let properties = match approval_properties(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = api::workflow::UpdateApprovalParams {
                scope,
                approval_id,
                description: optional_str(args, "description"),
                approver_id: optional_str(args, "approver_id"),
                deadline: optional_str(args, "deadline"),
                node_id: optional_str(args, "node_id"),
                properties: properties.as_ref(),
            };
            if params.description.is_none()
                && params.approver_id.is_none()
                && params.deadline.is_none()
                && params.node_id.is_none()
                && params.properties.is_none()
            {
                return Ok(error_text(
                    "approval update requires at least one of: description, approver_id, \
                     deadline, node_id, properties",
                ));
            }
            match api::workflow::update_approval(&client, &params).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "delete" => {
            let scope = approval_scope_opt(args);
            let approval_id = match required_str(args, "approval_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::delete_approval(&client, scope, approval_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "filter" => {
            let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
            let profile_id = match approval_profile_id(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let filter = match required_str(args, "filter") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let query = api::workflow::FilterQuery {
                limit: optional_u32(args, "limit"),
                offset: optional_u32(args, "offset"),
                status: optional_str(args, "status"),
                entry_type: None,
            };
            match api::workflow::list_approvals_filtered(
                &client,
                profile_type,
                profile_id,
                filter,
                &query,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "summary" => {
            let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
            let profile_id = match approval_profile_id(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::approvals_summary(&client, profile_type, profile_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "user-approvals" => {
            let filter = match required_str(args, "filter") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let query = api::workflow::FilterQuery {
                limit: optional_u32(args, "limit"),
                offset: optional_u32(args, "offset"),
                status: optional_str(args, "status"),
                entry_type: None,
            };
            match api::workflow::user_approvals(&client, filter, &query).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown approval action: {action}"))),
    }
}

/// Resolve the scoped approval profile ID, accepting `profile_id` (preferred)
/// or the legacy `workspace_id` arg name.
fn approval_profile_id(args: &Map<String, Value>) -> Result<&str, CallToolResult> {
    optional_str(args, "profile_id")
        .or_else(|| optional_str(args, "workspace_id"))
        .ok_or_else(|| error_text("Missing required parameter: profile_id"))
}

/// Resolve an optional approval scope for per-approval action routes
/// (details/approve/reject/update/delete). Returns `Some((profile_type,
/// profile_id))` when a profile ID is supplied (preferred `profile_id`, legacy
/// `workspace_id`), or `None` to use the legacy unscoped route.
fn approval_scope_opt(args: &Map<String, Value>) -> Option<(&str, &str)> {
    let profile_id =
        optional_str(args, "profile_id").or_else(|| optional_str(args, "workspace_id"))?;
    let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
    Some((profile_type, profile_id))
}

/// Parse an optional `properties` MCP arg into a JSON object value. Accepts
/// either a JSON object passed directly or a JSON-object string; returns an
/// error result if it is neither a JSON object nor a string encoding one.
fn approval_properties(args: &Map<String, Value>) -> Result<Option<Value>, CallToolResult> {
    match args.get("properties") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(Some(Value::Object(map.clone()))),
        Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
            Ok(v) if v.is_object() => Ok(Some(v)),
            _ => Err(error_text(
                "properties must be a JSON object (e.g. {\"key\":\"value\"})",
            )),
        },
        Some(_) => Err(error_text(
            "properties must be a JSON object (e.g. {\"key\":\"value\"})",
        )),
    }
}

/// Todo tool handler.
async fn handle_todo(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    match action {
        "list" => handle_todo_list(state, args).await,
        "create" => handle_todo_create(state, args).await,
        "details" => handle_todo_details(state, args).await,
        "update" => handle_todo_update(state, args).await,
        "toggle" => handle_todo_toggle(state, args).await,
        "delete" => handle_todo_delete(state, args).await,
        "bulk-toggle" => handle_todo_bulk_toggle(state, args).await,
        "filter" => handle_todo_filter(state, args).await,
        "summary" => handle_todo_summary(state, args).await,
        _ => Ok(error_text(&format!("Unknown todo action: {action}"))),
    }
}

async fn handle_todo_list(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let pt = optional_str(args, "profile_type").unwrap_or("workspace");
    let pid = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::list_todos_ctx(
        &client,
        pt,
        pid,
        optional_u32(args, "limit"),
        optional_u32(args, "offset"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_todo_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let pt = optional_str(args, "profile_type").unwrap_or("workspace");
    let pid = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let title = match required_str(args, "title") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::create_todo_ctx(&client, pt, pid, title, optional_str(args, "assignee_id"))
        .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_todo_details(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let todo_id = match required_str(args, "todo_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::get_todo_details(&client, todo_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_todo_update(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let todo_id = match required_str(args, "todo_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::update_todo(
        &client,
        todo_id,
        optional_str(args, "title"),
        optional_bool(args, "done"),
        optional_str(args, "assignee_id"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_todo_toggle(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let todo_id = match required_str(args, "todo_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::toggle_todo(&client, todo_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_todo_delete(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let todo_id = match required_str(args, "todo_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::delete_todo(&client, todo_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_todo_bulk_toggle(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let pt = optional_str(args, "profile_type").unwrap_or("workspace");
    let pid = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let ids_str = match required_str(args, "todo_ids") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let done = optional_bool(args, "done").unwrap_or(true);
    let ids: Vec<String> = ids_str.split(',').map(|s| s.trim().to_owned()).collect();
    match api::workflow::bulk_toggle_todos(&client, pt, pid, &ids, done).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `todo` filter action: filtered todo list for a workspace or share.
async fn handle_todo_filter(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
    let profile_id = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let filter = match required_str(args, "filter") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let query = api::workflow::FilterQuery {
        limit: optional_u32(args, "limit"),
        offset: optional_u32(args, "offset"),
        status: None,
        entry_type: None,
    };
    match api::workflow::list_todos_filtered(&client, profile_type, profile_id, filter, &query)
        .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// `todo` summary action: todo count summary for a workspace or share.
async fn handle_todo_summary(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let profile_type = optional_str(args, "profile_type").unwrap_or("workspace");
    let profile_id = match required_str(args, "profile_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::workflow::todos_summary(&client, profile_type, profile_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Apps tool handler.
async fn handle_apps(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    // Apps discovery doesn't require auth except launch
    let client = state.client().read().await;
    match action {
        "list" => match api::apps::list_apps(&client).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "details" => {
            let app_id = match required_str(args, "app_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::apps::app_details(&client, app_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "launch" => {
            if let Err(e) = require_auth(state).await {
                return Ok(e);
            }
            let app_id = match required_str(args, "app_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let ctx_type = match required_str(args, "context_type") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let ctx_id = match required_str(args, "context_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::apps::launch_app(&client, app_id, ctx_type, ctx_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "get-tool-apps" => {
            let tool_name = match required_str(args, "tool_name") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::apps::get_tool_apps(&client, tool_name).await {
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
    match api::import::provision_identity(&client, ws_id, provider).await {
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
    match api::import::discover(&client, ws_id, iid).await {
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
    match api::import::update_source(
        &client,
        sid,
        optional_u32(args, "sync_interval"),
        optional_str(args, "status"),
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
    let ctx_type = optional_str(args, "context_type").unwrap_or("workspace");
    let ctx_id = match required_str(args, "context_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let node_id = match required_str(args, "node_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match action {
        "acquire" => match api::locking::lock_acquire(&client, ctx_type, ctx_id, node_id).await {
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
            match api::metadata::list_eligible(
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
        "add-nodes" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_ids = match required_str(args, "node_ids") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::metadata::add_nodes_to_template(&client, workspace_id, template_id, node_ids)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "remove-nodes" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let node_ids = match required_str(args, "node_ids") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::metadata::remove_nodes_from_template(
                &client,
                workspace_id,
                template_id,
                node_ids,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "list-nodes" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::metadata::list_template_nodes(
                &client,
                workspace_id,
                template_id,
                optional_u32(args, "limit"),
                optional_u32(args, "offset"),
                optional_str(args, "sort_field"),
                optional_str(args, "sort_dir"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "auto-match" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let batch_size = optional_u32(args, "batch_size");
            match api::metadata::auto_match_template(&client, workspace_id, template_id, batch_size)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "extract-all" => {
            if let Some(e) = require_ai_spend_confirmation(args) {
                return Ok(e);
            }
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            let force = optional_bool(args, "force").unwrap_or(false);
            match api::metadata::extract_all(&client, workspace_id, template_id, fields, force)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
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
            let template_id = optional_str(args, "template_id").filter(|s| !s.trim().is_empty());
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            match api::metadata::extract_node_metadata(
                &client,
                workspace_id,
                node_id,
                template_id,
                fields,
            )
            .await
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
            let template_id = optional_str(args, "template_id").filter(|s| !s.trim().is_empty());
            let fields = match resolve_extract_fields(args) {
                Ok(f) => f,
                Err(msg) => return Ok(error_text(msg)),
            };
            let poll_interval = optional_u64(args, "poll_interval");
            Ok(metadata_extract_and_wait(
                &client,
                workspace_id,
                node_id,
                template_id,
                fields,
                poll_interval,
            )
            .await)
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
            let template_id = optional_str(args, "template_id").filter(|s| !s.trim().is_empty());
            match api::metadata::search_metadata(
                &client,
                workspace_id,
                query,
                template_id,
                optional_u32(args, "limit"),
                optional_u32(args, "offset"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "export-view" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let template_id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let parent_node_id =
                optional_str(args, "parent_node_id").filter(|s| !s.trim().is_empty());
            match api::metadata::export_view(&client, workspace_id, template_id, parent_node_id)
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
/// re-auth error. When the enqueue resolves to an empty effective scope
/// (no `job_id`), the original `202` body is returned unchanged — there is
/// nothing to wait for.
async fn metadata_extract_and_wait(
    client: &fastio_cli::client::ApiClient,
    workspace_id: &str,
    node_id: &str,
    template_id: Option<&str>,
    fields: Option<&str>,
    poll_interval: Option<u64>,
) -> CallToolResult {
    use fastio_cli::api::metadata::ExtractJobState;

    let enqueue = match api::metadata::extract_node_metadata(
        client,
        workspace_id,
        node_id,
        template_id,
        fields,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return cli_err_to_result(&e),
    };

    // Pull the job_id out of the (possibly enveloped) 202 body.
    let payload = enqueue.get("response").unwrap_or(&enqueue);
    let job_id = payload
        .get("job_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let Some(job_id) = job_id else {
        // No job enqueued (empty effective scope) — return the 202 body.
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
                    ExtractJobState::Errored(msg) => {
                        let detail = msg.unwrap_or_else(|| "no error message provided".to_owned());
                        return error_text(&format!("extraction job {job_id} failed: {detail}"));
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

// ─── Workflow Orchestration tool ──────────────────────────────────────────────

/// Default seconds between `workflow` compound-wait polls.
const WORKFLOW_WAIT_POLL_DEFAULT_SECS: u64 = 3;
/// Lower bound on the `workflow` compound-wait poll interval.
const WORKFLOW_WAIT_POLL_MIN_SECS: u64 = 1;
/// Upper bound on the `workflow` compound-wait poll interval.
const WORKFLOW_WAIT_POLL_MAX_SECS: u64 = 60;
/// Hard ceiling on the `workflow` compound-wait poll loop (well under the
/// ~1-hour JWT lifetime).
const WORKFLOW_WAIT_MAX_SECS: u64 = 600;

/// The structured `describe` payload for the `workflow` tool — the
/// authoritative per-action reference. Admin/destructive/crypto actions are
/// intentionally ABSENT from this tool (they are CLI-binary-only); the
/// `cli_only_actions` field names them so an agent knows where they live.
// The length is a flat action-spec table, not branching logic.
#[allow(clippy::too_many_lines)]
fn workflow_describe() -> CallToolResult {
    // (action, required[], optional[], note). Built programmatically to keep
    // the `json!` macro shallow (a single deeply-nested literal blows the
    // macro recursion limit).
    let actions: &[(&str, &[&str], &[&str], &str)] = &[
        ("describe", &[], &[], ""),
        ("get", &["workflow_id"], &[], ""),
        ("list", &["workspace_id"], &["limit", "offset"], ""),
        ("state", &["workflow_id"], &[], ""),
        (
            "instantiate",
            &["workflow_id", "idempotency_key"],
            &["trigger_payload", "external_subject_id", "pool_key"],
            "",
        ),
        (
            "instantiate-and-wait",
            &["workflow_id", "idempotency_key"],
            &[
                "trigger_payload",
                "external_subject_id",
                "pool_key",
                "poll_interval",
            ],
            "fires then polls to a terminal lifecycle",
        ),
        ("pause", &["workflow_id"], &[], ""),
        ("resume", &["workflow_id"], &[], ""),
        // NOTE: `cancel` is intentionally NOT an MCP action — it is a terminal,
        // irreversible lifecycle mutation and is listed under cli_only below.
        (
            "grant-list",
            &["workflow_id"],
            &["limit", "cursor"],
            "cursor-paginated",
        ),
        ("step-get", &["workflow_id", "step_occurrence_id"], &[], ""),
        (
            "step-output",
            &["workflow_id", "step_occurrence_id", "output"],
            &["retry_on_conflict"],
            "CAS-guarded; 409 surfaced unless retry_on_conflict=true",
        ),
        (
            "step-advance",
            &["workflow_id", "step_occurrence_id"],
            &["output", "retry_on_conflict"],
            "CAS-guarded",
        ),
        (
            "step-occurrences",
            &["workflow_id", "step_id"],
            &["limit", "offset"],
            "",
        ),
        ("template-list", &["workspace_id"], &["limit", "offset"], ""),
        ("template-get", &["template_id"], &["include_body"], ""),
        ("trigger-list", &["workspace_id"], &["enabled_filter"], ""),
        ("trigger-get", &["trigger_id"], &[], ""),
        (
            "trigger-fire",
            &["trigger_id", "idempotency_key"],
            &["trigger_payload"],
            "409 carries a stable reason",
        ),
        (
            "trigger-fire-and-wait",
            &["trigger_id", "idempotency_key"],
            &["trigger_payload", "poll_interval"],
            "",
        ),
        (
            "trigger-dry-run",
            &["trigger_id"],
            &["window_days", "sample_limit", "apply_guards"],
            "",
        ),
        (
            "obligation-list",
            &["workflow_id"],
            &["status", "assigned_user_id", "limit", "offset"],
            "workflow_id is the required authz anchor",
        ),
        ("obligation-get", &["obligation_id"], &[], ""),
        ("obligation-claim", &["obligation_id"], &[], ""),
        ("obligation-release", &["obligation_id"], &[], ""),
        (
            "obligation-resolve",
            &["obligation_id"],
            &["resolution_payload"],
            "",
        ),
        ("inbox-me", &[], &[], ""),
        ("inbox-workspace", &["workspace_id"], &[], ""),
        ("inbox-pool", &["workspace_id", "pool_key"], &[], ""),
        ("schema-get", &["workflow_id"], &[], ""),
        (
            "audit-events",
            &["workflow_id"],
            &["include_payload", "limit", "offset"],
            "",
        ),
        (
            "audit-export-start",
            &["workflow_id"],
            &["scope", "include_overlays", "redaction_pin_strategy"],
            "",
        ),
        (
            "audit-export-list",
            &["workspace_id"],
            &["limit", "offset"],
            "",
        ),
        ("audit-export-get", &["job_id"], &[], ""),
        (
            "audit-export-and-download",
            &["workflow_id"],
            &[
                "scope",
                "include_overlays",
                "redaction_pin_strategy",
                "output_path",
                "poll_interval",
            ],
            "starts the export, polls to completed, streams manifest + all chunks to output_path",
        ),
        (
            "subject-workflows",
            &["workspace_id", "subject_id"],
            &[],
            "",
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
        "cancel",
        "create",
        "update",
        "delete",
        "purge",
        "transfer",
        "rotate-inbound-key",
        "grant add/revoke",
        "step cancel",
        "template create/publish/withdraw/deprecate",
        "trigger create/update/delete/purge/dry-run-draft/rotate-inbound-key",
        "trigger-alias get/set/remove",
        "schema set/derive",
        "audit redaction request/confirm/get",
        "audit check-integrity",
        "outbound create/update/delete/rotate-secret",
        "pool create/delete",
        "realtime token",
        "review create/decision/admin-resolve",
    ]
    .iter()
    .map(|s| Value::String((*s).to_owned()))
    .collect();

    let payload = serde_json::json!({
        "tool": "workflow",
        "summary": "Workflow Orchestration v3.2 — durable runtime, templates, triggers, \
                    obligations, signed audit, pools. Offload-oriented: prefer the compound \
                    *-and-wait / *-and-download actions over hand-driven poll loops.",
        "destructive_actions": [],
        "side_effects": "instantiate / trigger-fire (+ their *-and-wait variants) START runtime \
                         work and consume the workflow's credit budget. step-output / step-advance \
                         drive the runtime forward and are CAS-guarded (a 409 surfaces by default; \
                         pass retry_on_conflict=true to re-read and retry once). instantiate / \
                         trigger-fire REQUIRE an idempotency_key for replay safety — there is NO \
                         MCP auto-generate. The terminal 'cancel' lifecycle mutation is CLI-only \
                         (see cli_only_actions) and is NOT exposed over MCP.",
        "guidance": {
            "offload": "To run a workflow end-to-end, use instantiate-and-wait (or \
                        trigger-fire-and-wait). To obtain a verifiable audit bundle, use \
                        audit-export-and-download.",
            "integrity": "After audit-export-and-download, run `fastio workflow audit \
                          check-integrity` (CLI) — INTEGRITY only; HMAC authenticity is not \
                          implemented (deferred).",
            "cli_only_actions": cli_only,
        },
        "actions": Value::Object(action_map),
    });
    success_json(&payload)
}

/// Resolve the idempotency key for an MCP instantiate/fire action. The MCP
/// surface has NO auto-generate (unlike the CLI's explicit opt-in): a missing
/// key is a hard error so replay safety is never silently dropped.
fn require_idempotency_key(args: &Map<String, Value>) -> Result<&str, CallToolResult> {
    match optional_str(args, "idempotency_key").filter(|s| !s.trim().is_empty()) {
        Some(k) => Ok(k),
        None => Err(error_text(
            "idempotency_key is required for replay-safe instantiation/firing. The MCP surface \
             does not auto-generate one — supply a stable, caller-chosen key.",
        )),
    }
}

/// Workflow Orchestration tool handler (read + drive actions; offload-oriented
/// compounds). Admin/destructive/crypto actions are CLI-binary-only and absent.
// Justification: this is a single flat `match action { … }` dispatch over the
// ~35 read+drive workflow actions. Each arm is a few lines of arg extraction +
// one orchestration call; splitting it into sub-handlers would scatter the
// action surface across many functions and obscure the one-place action list
// that mirrors the tool's advertised `actions`. The length is inherent to the
// dispatch breadth, not accidental complexity — same pattern as the other
// per-tool handlers in this module.
#[allow(clippy::too_many_lines)]
async fn handle_workflow(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    use fastio_cli::api::orchestration as wf;

    // `describe` needs no auth.
    if action == "describe" {
        return Ok(workflow_describe());
    }
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;

    match action {
        "get" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::get_workflow(&client, id).await)
        }
        "list" => {
            let ws = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::list_workflows(
                    &client,
                    ws,
                    optional_u32(args, "limit"),
                    optional_u32(args, "offset"),
                )
                .await,
            )
        }
        "state" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::get_workflow_state(&client, id).await)
        }
        "instantiate" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let key = match require_idempotency_key(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = wf::InstantiateParams::new(key.to_owned())
                .trigger_payload(optional_str(args, "trigger_payload").map(str::to_owned))
                .external_subject_id(optional_str(args, "external_subject_id").map(str::to_owned))
                .pool_key(optional_str(args, "pool_key").map(str::to_owned));
            wf_render(wf::instantiate_workflow(&client, id, &params).await)
        }
        "instantiate-and-wait" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let key = match require_idempotency_key(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let params = wf::InstantiateParams::new(key.to_owned())
                .trigger_payload(optional_str(args, "trigger_payload").map(str::to_owned))
                .external_subject_id(optional_str(args, "external_subject_id").map(str::to_owned))
                .pool_key(optional_str(args, "pool_key").map(str::to_owned));
            if let Err(e) = wf::instantiate_workflow(&client, id, &params).await {
                return Ok(cli_err_to_result(&e));
            }
            Ok(workflow_wait_for_terminal(&client, id, optional_u64(args, "poll_interval")).await)
        }
        "pause" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::pause_workflow(&client, id).await)
        }
        "resume" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::resume_workflow(&client, id).await)
        }
        // `cancel` is intentionally ABSENT from MCP: it is a terminal lifecycle
        // mutation (cascades to sync sub-children, irreversible). It falls
        // through to the CLI-only fallback below — run `fastio workflow cancel`.
        "grant-list" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::list_grants(
                    &client,
                    id,
                    optional_u32(args, "limit"),
                    optional_str(args, "cursor"),
                )
                .await,
            )
        }
        "step-get" => {
            let (wid, oid) = match (
                required_str(args, "workflow_id"),
                required_str(args, "step_occurrence_id"),
            ) {
                (Ok(w), Ok(o)) => (w, o),
                (Err(e), _) | (_, Err(e)) => return Ok(e),
            };
            wf_render(wf::get_step_occurrence(&client, wid, oid).await)
        }
        "step-output" => {
            let (wid, oid) = match (
                required_str(args, "workflow_id"),
                required_str(args, "step_occurrence_id"),
            ) {
                (Ok(w), Ok(o)) => (w, o),
                (Err(e), _) | (_, Err(e)) => return Ok(e),
            };
            let output = match required_str(args, "output") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let retry = optional_bool(args, "retry_on_conflict") == Some(true);
            Ok(wf_step_cas(&client, wid, oid, retry, || {
                wf::submit_step_output(&client, wid, oid, output)
            })
            .await)
        }
        "step-advance" => {
            let (wid, oid) = match (
                required_str(args, "workflow_id"),
                required_str(args, "step_occurrence_id"),
            ) {
                (Ok(w), Ok(o)) => (w, o),
                (Err(e), _) | (_, Err(e)) => return Ok(e),
            };
            let output = optional_str(args, "output");
            let retry = optional_bool(args, "retry_on_conflict") == Some(true);
            Ok(wf_step_cas(&client, wid, oid, retry, || {
                wf::advance_step(&client, wid, oid, output)
            })
            .await)
        }
        "step-occurrences" => {
            let (wid, sid) = match (
                required_str(args, "workflow_id"),
                required_str(args, "step_id"),
            ) {
                (Ok(w), Ok(s)) => (w, s),
                (Err(e), _) | (_, Err(e)) => return Ok(e),
            };
            wf_render(
                wf::list_step_occurrences(
                    &client,
                    wid,
                    sid,
                    optional_u32(args, "limit"),
                    optional_u32(args, "offset"),
                )
                .await,
            )
        }
        "template-list" => {
            let ws = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::list_templates(
                    &client,
                    ws,
                    optional_u32(args, "limit"),
                    optional_u32(args, "offset"),
                )
                .await,
            )
        }
        "template-get" => {
            let id = match required_str(args, "template_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::get_template(
                    &client,
                    id,
                    optional_bool(args, "include_body") == Some(true),
                )
                .await,
            )
        }
        "trigger-list" => {
            let ws = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::list_triggers(&client, ws, optional_str(args, "enabled_filter")).await)
        }
        "trigger-get" => {
            let id = match required_str(args, "trigger_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::get_trigger(&client, id).await)
        }
        "trigger-fire" => {
            let id = match required_str(args, "trigger_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let key = match require_idempotency_key(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::fire_trigger(&client, id, key, optional_str(args, "trigger_payload")).await,
            )
        }
        "trigger-fire-and-wait" => {
            let id = match required_str(args, "trigger_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let key = match require_idempotency_key(args) {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let fired =
                match wf::fire_trigger(&client, id, key, optional_str(args, "trigger_payload"))
                    .await
                {
                    Ok(v) => v,
                    Err(e) => return Ok(cli_err_to_result(&e)),
                };
            // Resolve the instantiated workflow id from the fire response.
            let payload = fired.get("response").unwrap_or(&fired);
            let wid = payload
                .get("trigger_fire")
                .and_then(|t| t.get("instantiated_run").or_else(|| t.get("job_id")))
                .and_then(Value::as_str)
                .map(str::to_owned);
            match wid {
                Some(w) => Ok(workflow_wait_for_terminal(
                    &client,
                    &w,
                    optional_u64(args, "poll_interval"),
                )
                .await),
                None => Ok(success_json(&fired)),
            }
        }
        "trigger-dry-run" => {
            let id = match required_str(args, "trigger_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::dry_run_trigger(
                    &client,
                    id,
                    optional_u64(args, "window_days"),
                    optional_u64(args, "sample_limit"),
                    optional_bool(args, "apply_guards"),
                )
                .await,
            )
        }
        "obligation-list" => {
            let wid = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::list_obligations(
                    &client,
                    wid,
                    optional_str(args, "status"),
                    optional_str(args, "assigned_user_id"),
                    optional_u32(args, "limit"),
                    optional_u32(args, "offset"),
                )
                .await,
            )
        }
        "obligation-get" => wf_render_oblig(&client, args, "get").await,
        "obligation-claim" => wf_render_oblig(&client, args, "claim").await,
        "obligation-release" => wf_render_oblig(&client, args, "release").await,
        "obligation-resolve" => {
            let id = match required_str(args, "obligation_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::resolve_obligation(&client, id, optional_str(args, "resolution_payload")).await,
            )
        }
        "inbox-me" => wf_render(wf::inbox(&client).await),
        "inbox-workspace" => {
            let ws = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::inbox_workspace(&client, ws).await)
        }
        "inbox-pool" => {
            let (ws, pk) = match (
                required_str(args, "workspace_id"),
                required_str(args, "pool_key"),
            ) {
                (Ok(w), Ok(p)) => (w, p),
                (Err(e), _) | (_, Err(e)) => return Ok(e),
            };
            wf_render(wf::inbox_pool(&client, ws, pk).await)
        }
        "schema-get" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::get_extraction_schema(&client, id).await)
        }
        "audit-events" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::audit_events(
                    &client,
                    id,
                    optional_bool(args, "include_payload") == Some(true),
                    optional_u32(args, "limit"),
                    optional_u32(args, "offset"),
                )
                .await,
            )
        }
        "audit-export-start" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::start_audit_export(
                    &client,
                    id,
                    optional_str(args, "scope"),
                    optional_bool(args, "include_overlays"),
                    optional_str(args, "redaction_pin_strategy"),
                )
                .await,
            )
        }
        "audit-export-list" => {
            let ws = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(
                wf::list_audit_export_jobs(
                    &client,
                    ws,
                    optional_u32(args, "limit"),
                    optional_u32(args, "offset"),
                )
                .await,
            )
        }
        "audit-export-get" => {
            let id = match required_str(args, "job_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            wf_render(wf::get_audit_export_job(&client, id).await)
        }
        "audit-export-and-download" => {
            let id = match required_str(args, "workflow_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            Ok(workflow_export_and_download(&client, args, id).await)
        }
        "subject-workflows" => {
            let (ws, sid) = match (
                required_str(args, "workspace_id"),
                required_str(args, "subject_id"),
            ) {
                (Ok(w), Ok(s)) => (w, s),
                (Err(e), _) | (_, Err(e)) => return Ok(e),
            };
            wf_render(wf::subject_workflows(&client, ws, sid).await)
        }
        _ => Ok(error_text(&format!(
            "Unknown or CLI-only workflow action: {action}. Admin/destructive operations \
             (cancel, create/update/delete/purge, template/pool/trigger lifecycle, secret/key \
             rotation, redaction, schema set/derive, realtime token) are CLI-binary-only — run \
             them via `fastio workflow …` (e.g. `fastio workflow cancel <id>`). Call \
             action='describe' for the full MCP action list."
        ))),
    }
}

/// Render an orchestration `Result<Value>` as an MCP result.
///
/// Returns `Result<CallToolResult, McpError>` (never `Err`) so the ~20
/// `handle_workflow` match arms can return it directly as the handler's
/// `Result`-typed value without wrapping each in `Ok(...)`. The `McpError`
/// arm is structurally unreachable here — an API failure becomes a successful
/// tool result carrying `is_error`, matching every other handler in this
/// module.
#[allow(clippy::unnecessary_wraps)]
fn wf_render(
    result: Result<Value, fastio_cli::error::CliError>,
) -> Result<CallToolResult, McpError> {
    match result {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

/// Run an obligation lifecycle action that takes only `obligation_id`.
async fn wf_render_oblig(
    client: &fastio_cli::client::ApiClient,
    args: &Map<String, Value>,
    op: &str,
) -> Result<CallToolResult, McpError> {
    use fastio_cli::api::orchestration as wf;
    let id = match required_str(args, "obligation_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let result = match op {
        "get" => wf::get_obligation(client, id).await,
        "claim" => wf::claim_obligation(client, id).await,
        "release" => wf::release_obligation(client, id).await,
        _ => return Ok(error_text("internal: unknown obligation op")),
    };
    wf_render(result)
}

/// Run a CAS-guarded step mutation, surfacing a 409 by default and retrying
/// once (after a re-read) only when `retry_on_conflict` is set.
///
/// On a 409 with `retry_on_conflict=true`, the re-read is load-bearing (mirrors
/// the CLI `run_step_mutation_with_cas`): a failed re-read is surfaced rather
/// than blind-retried, and a re-read showing a terminal/non-mutable `state`
/// abandons the retry (it would only 409 again). The step endpoints take no
/// client-supplied CAS version, so the fresh value is used as a mutability gate
/// rather than threaded into the retry body.
async fn wf_step_cas<F, Fut>(
    client: &fastio_cli::client::ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
    retry_on_conflict: bool,
    op: F,
) -> CallToolResult
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Value, fastio_cli::error::CliError>>,
{
    use fastio_cli::api::orchestration as wf;
    match op().await {
        Ok(v) => success_json(&v),
        Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 409 => {
            if !retry_on_conflict {
                return error_text(
                    "step mutation hit a CAS conflict (409): the occurrence was modified \
                     concurrently. Re-read it (step-get) and retry, or pass retry_on_conflict=true \
                     to re-read and retry once automatically.",
                );
            }
            // The re-read must succeed before retrying; surface its failure.
            let snapshot = match wf::get_step_occurrence(client, workflow_id, step_occurrence_id)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    return error_text(&format!(
                        "CAS conflict (409): re-reading the step occurrence failed, so the retry \
                         was abandoned: {e}"
                    ));
                }
            };
            // A terminal/non-mutable state means the retry would just 409 again.
            if let Some(state) = step_occurrence_state(&snapshot)
                && matches!(
                    state.as_str(),
                    "completed" | "failed" | "skipped" | "cancelled"
                )
            {
                return error_text(&format!(
                    "CAS conflict (409): the step occurrence is now in terminal state '{state}' \
                     and can no longer be mutated; not retrying."
                ));
            }
            match op().await {
                Ok(v) => success_json(&v),
                Err(e) => error_text(&format!(
                    "step mutation still conflicted after one retry (CAS 409): {e}"
                )),
            }
        }
        Err(e) => cli_err_to_result(&e),
    }
}

/// Read a step occurrence's lifecycle `state` from a get-occurrence snapshot
/// (enveloped or flat shape).
fn step_occurrence_state(snapshot: &Value) -> Option<String> {
    let payload = snapshot.get("response").unwrap_or(snapshot);
    payload
        .get("step_occurrence")
        .and_then(|o| o.get("state"))
        .or_else(|| payload.get("state"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

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

/// Poll runtime state to a terminal lifecycle and return the final snapshot.
async fn workflow_wait_for_terminal(
    client: &fastio_cli::client::ApiClient,
    workflow_id: &str,
    poll_interval: Option<u64>,
) -> CallToolResult {
    use fastio_cli::api::orchestration as wf;
    let interval = poll_interval
        .unwrap_or(WORKFLOW_WAIT_POLL_DEFAULT_SECS)
        .clamp(WORKFLOW_WAIT_POLL_MIN_SECS, WORKFLOW_WAIT_POLL_MAX_SECS);
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(WORKFLOW_WAIT_MAX_SECS);
    loop {
        // Re-check the deadline at the TOP of every iteration, before issuing
        // the next state GET. The sleep below is clamped to the remaining wait
        // (and a 429 clamp can land exactly on the deadline); without this check
        // a woken iteration would issue one more request that could add the
        // client's request timeout and overrun WORKFLOW_WAIT_MAX_SECS. Mirrors
        // the `mcp_ask_wait` top-of-loop check.
        if tokio::time::Instant::now() >= deadline {
            return error_text(&format!(
                "timed out after ~{WORKFLOW_WAIT_MAX_SECS}s waiting for workflow {workflow_id} to \
                 reach a terminal state; it may still be running. Use action='state' to poll."
            ));
        }
        // Default cadence is the fixed interval; rate limits and transient
        // errors override it below (transient errors use the SAME bounded
        // jittered backoff as the CLI `wait`).
        let mut next_sleep = std::time::Duration::from_secs(interval);
        match wf::get_workflow_state(client, workflow_id).await {
            Ok(snapshot) => {
                let state = snapshot
                    .get("response")
                    .unwrap_or(&snapshot)
                    .get("workflow")
                    .and_then(|w| w.get("state"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if state.as_deref().is_some_and(|s| {
                    matches!(s, "completed" | "cancelled" | "archived" | "deleted")
                }) {
                    return success_json(&snapshot);
                }
            }
            Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 401 => {
                return error_text(&format!(
                    "authentication expired while waiting for workflow {workflow_id}; it may still \
                     be running. Re-authenticate and use action='state'."
                ));
            }
            Err(other) => match classify_wf_poll_error(&other) {
                WfPollAction::RateLimited { retry_after_secs } => {
                    if retry_after_secs > 0 {
                        next_sleep = std::time::Duration::from_secs(
                            retry_after_secs
                                .clamp(WORKFLOW_WAIT_POLL_MIN_SECS, WORKFLOW_WAIT_POLL_MAX_SECS),
                        );
                    }
                }
                // Transient blip: back off with bounded CSPRNG jitter (shared
                // CLI helper; jitter failure falls back to no jitter).
                WfPollAction::RetryTransient => {
                    next_sleep = crate::commands::workflow::jittered(interval);
                }
                // Persistent, non-transient: surface it rather than loop.
                WfPollAction::Fatal(result) => return result,
            },
        }
        if tokio::time::Instant::now() >= deadline {
            return error_text(&format!(
                "timed out after ~{WORKFLOW_WAIT_MAX_SECS}s waiting for workflow {workflow_id} to \
                 reach a terminal state; it may still be running. Use action='state' to poll."
            ));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(remaining.min(next_sleep)).await;
    }
}

/// Compound: start an audit export, poll the job to completed, then stream the
/// manifest and every chunk to `output_path` via the streaming download helper.
// Justification: this is one linear offload pipeline — start export → resolve
// job_id → bounded/rate-limit-aware poll loop → resolve output dir → stream
// manifest + N chunks. Splitting the poll loop or the streaming step into
// helpers would fragment a single sequential flow whose stages share local
// state (job_id, deadline, out_dir), so the modest overage is kept inline.
#[allow(clippy::too_many_lines)]
async fn workflow_export_and_download(
    client: &fastio_cli::client::ApiClient,
    args: &Map<String, Value>,
    workflow_id: &str,
) -> CallToolResult {
    use fastio_cli::api::orchestration as wf;

    let start = match wf::start_audit_export(
        client,
        workflow_id,
        optional_str(args, "scope"),
        optional_bool(args, "include_overlays"),
        optional_str(args, "redaction_pin_strategy"),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return cli_err_to_result(&e),
    };
    let payload = start.get("response").unwrap_or(&start);
    let Some(job_id) = payload
        .get("job_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return error_text("audit export did not return a job_id");
    };

    let interval = optional_u64(args, "poll_interval")
        .unwrap_or(WORKFLOW_WAIT_POLL_DEFAULT_SECS)
        .clamp(WORKFLOW_WAIT_POLL_MIN_SECS, WORKFLOW_WAIT_POLL_MAX_SECS);
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(WORKFLOW_WAIT_MAX_SECS);

    // Poll the job to completed (or terminal failure).
    let job = loop {
        // Re-check the deadline at the TOP of every iteration, before issuing
        // the next job-status GET. The sleep below is clamped to the remaining
        // wait (and a 429 clamp can land exactly on the deadline); without this
        // check a woken iteration would issue one more request that could add
        // the client's request timeout and overrun WORKFLOW_WAIT_MAX_SECS.
        // Mirrors the `mcp_ask_wait` top-of-loop check.
        if tokio::time::Instant::now() >= deadline {
            return error_text(&format!(
                "timed out after ~{WORKFLOW_WAIT_MAX_SECS}s waiting for export job {job_id}"
            ));
        }
        // Default cadence is the fixed interval; rate limits and transient
        // errors override it below (transient errors use the SAME bounded
        // jittered backoff as the CLI `wait`).
        let mut next_sleep = std::time::Duration::from_secs(interval);
        match wf::get_audit_export_job(client, &job_id).await {
            Ok(j) => {
                let status = j
                    .get("response")
                    .unwrap_or(&j)
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if status == "completed" {
                    break j;
                }
                if matches!(status, "failed" | "errored" | "cancelled") {
                    return error_text(&format!("audit export job {job_id} ended in '{status}'"));
                }
            }
            Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 401 => {
                return error_text("authentication expired while waiting for the export job");
            }
            Err(other) => match classify_wf_poll_error(&other) {
                WfPollAction::RateLimited { retry_after_secs } => {
                    if retry_after_secs > 0 {
                        next_sleep = std::time::Duration::from_secs(
                            retry_after_secs
                                .clamp(WORKFLOW_WAIT_POLL_MIN_SECS, WORKFLOW_WAIT_POLL_MAX_SECS),
                        );
                    }
                }
                // Transient blip: back off with bounded CSPRNG jitter (shared
                // CLI helper; jitter failure falls back to no jitter).
                WfPollAction::RetryTransient => {
                    next_sleep = crate::commands::workflow::jittered(interval);
                }
                WfPollAction::Fatal(result) => return result,
            },
        }
        if tokio::time::Instant::now() >= deadline {
            return error_text(&format!(
                "timed out after ~{WORKFLOW_WAIT_MAX_SECS}s waiting for export job {job_id}"
            ));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(remaining.min(next_sleep)).await;
    };

    let job_payload = job.get("response").unwrap_or(&job);
    let total_chunks = job_payload
        .get("total_chunks")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // Resolve the output directory (default .fastio/downloads/). Create it 0700
    // (private) — the audit bundle can carry sensitive workflow data, and this
    // mirrors the sign-download path's `create_dir_all_private` for the SAME
    // default dir.
    let out_dir =
        std::path::PathBuf::from(optional_str(args, "output_path").unwrap_or(".fastio/downloads"));
    if let Err(e) = create_dir_all_private(&out_dir) {
        return error_text(&format!(
            "failed to create output directory '{}': {e}",
            out_dir.display()
        ));
    }

    // Stream the manifest, then each chunk, to disk (NEVER buffer).
    let mut written = Vec::new();
    let manifest_path = out_dir.join("manifest.json");
    let manifest_api_path = wf::audit_bundle_chunk_path(&job_id, "manifest");
    if let Err(e) = client
        .download_file_stream(&manifest_api_path, &manifest_path)
        .await
    {
        return error_text(&format!("failed to download manifest: {e}"));
    }
    written.push(manifest_path.display().to_string());

    for i in 0..total_chunks {
        let chunk_path = out_dir.join(format!("chunk_{i:04}.jsonl"));
        let api_path = wf::audit_bundle_chunk_path(&job_id, &i.to_string());
        if let Err(e) = client.download_file_stream(&api_path, &chunk_path).await {
            return error_text(&format!("failed to download chunk {i}: {e}"));
        }
        written.push(chunk_path.display().to_string());
    }

    let result = serde_json::json!({
        "result": "yes",
        "job_id": job_id,
        "total_chunks": total_chunks,
        "downloaded": written,
        "next_step": "Run `fastio workflow audit check-integrity --manifest <manifest> --chunk <0> …` \
                      to verify chunk hashes + the content-hash chain + completeness. (HMAC \
                      authenticity verification is not implemented.)",
    });
    success_json(&result)
}

// ─── Sign (E-Signature) ─────────────────────────────────────────────────────

/// Discriminates an async-artifact FETCH (signed PDF / audit certificate) from
/// every other signing call, so a `404` (live not-ready codes
/// `1609`/`128301`/`146422`) is only reframed as "not ready yet" where that is
/// genuinely correct (`signing.txt:520`, `:531`). For [`SignOp::General`] —
/// CRUD / list / get / update / source-download — a `404` is a genuine
/// not-found (`signing.txt:591`). Mirrors the CLI's `SignOp`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SignOp {
    /// A CRUD / list / get / update call, or a source-document download.
    General,
    /// A signed-PDF fetch — available once the envelope **completes**, so the
    /// not-ready guidance steers to "poll until it completes".
    SignedFetch,
    /// An audit-certificate fetch — available once the envelope reaches **any
    /// terminal state** (completed OR voided), so the not-ready guidance steers
    /// to "poll until it reaches a terminal state".
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
///   terminal state** (completed OR voided). For [`SignOp::General`] a
///   `404`/`1609` is a genuine not-found and is NOT reframed (`signing.txt:591`).
/// - `10545` (workspace membership) / `115069` (envelope access) override the
///   generic 401 hint; `1680` (workspace permission, kept generic), `1670`
///   (plan signing capability), `9992` (removed/renamed route), `1685`
///   (credits), `1660` (terminal) each get signing-scoped wording.
/// - Unmatched codes append the shared `err.suggestion()`.
fn sign_err_to_result(err: &fastio_cli::error::CliError, ctx: &str, op: SignOp) -> CallToolResult {
    if let fastio_cli::error::CliError::Api(api) = err {
        // Async-artifact "not ready yet" — only for a signed/audit fetch. Code
        // 9992 (router-level "no such route", also HTTP 404) is EXCLUDED so the
        // code-specific match below frames it as a removed/renamed route instead
        // of "poll and retry" (otherwise an agent would poll a dead route forever).
        if op.is_artifact_fetch()
            && api.code != 9992
            && (api.http_status == 404
                || api.code == 1609
                || api.code == 128_301
                || api.code == 146_422)
        {
            // Artifact-appropriate poll target: the signed PDF is available once
            // the envelope COMPLETES; the audit certificate once the envelope
            // reaches any TERMINAL state (completed OR voided).
            let stage = if op == SignOp::AuditFetch {
                "the audit certificate is not generated until the envelope reaches a terminal \
                 state; poll envelope-get and retry once it reaches a terminal state (completed \
                 or voided)."
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
                "the server does not recognize this API path (9992) — the route may have been \
                 removed or renamed. Check for a `fastio` CLI update.",
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
/// `body_json` / `policy_json` are documented as OBJECTS (`signing.txt:291`);
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
                .value_json(value_json))
        })
        .collect()
}

/// The authoritative per-action describe payload for the `sign` tool. Names the
/// CLI-only outward/destructive/terminal actions under `cli_only_actions`.
// The length is a flat action-spec table, not branching logic (mirrors
// `workflow_describe`).
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
             (or body_json), or the simple source_node_id + recipient_email path.",
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
             is a declarative replace (omit to leave unchanged).",
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
    ]
    .iter()
    .map(|s| Value::String((*s).to_owned()))
    .collect();

    let payload = serde_json::json!({
        "tool": "sign",
        "summary": "E-signature SignEnvelopes (workspace-parented) — READ + reversible-DRAFT-drive \
                    only. Drafting and downloads are exposed over MCP; the outward-facing send/void \
                    are CLI-binary-only. Envelopes are voided, not deleted (no delete action).",
        "common_required": ["workspace_id"],
        "destructive_actions": [],
        "side_effects": "envelope-create makes a DRAFT (reversible; no recipient is notified and \
                         no credits are reserved until a CLI `send`). Downloads write files to the \
                         agent's local filesystem. The outward-facing send / void actions are \
                         CLI-binary-only (see cli_only_actions) and are NOT exposed over MCP. \
                         There is no delete — envelopes are voided.",
        "guidance": {
            "workspace": "workspace_id is the 19-digit owner workspace; every envelope is \
                          workspace-parented (the former org surface was removed).",
            "send_void": "To send (emails real recipients) or void an envelope, run the CLI: \
                          `fastio sign envelope send|void …`. These are NOT available over MCP by \
                          design. Envelopes are voided, not deleted — there is no delete action.",
            "cli_only_actions": cli_only,
        },
        "actions": Value::Object(action_map),
    });
    success_json(&payload)
}

/// E-signature tool handler — READ + reversible-DRAFT-drive actions only. The
/// outward-facing / terminal actions (send / void) are CLI-binary-only and are
/// routed to a guidance message BEFORE auth/workspace extraction (mirrors how
/// the `workflow` tool keeps `cancel` CLI-only). Envelopes are voided, not
/// deleted; a `delete` request is reported as unsupported.
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
                     (recipients are a full replacement); name / expires_at / policy_json / \
                     documents_json / fields_json are optional",
                ));
            }
            // An update is a FULL recipient replacement — recipients_json (>=1)
            // is required (F5; mirrors the CLI).
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
    // OBJECT (signing.txt:291); `sign_json_object` rejects a non-object.
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
            Some(node) => vec![
                DocumentSpec::new()
                    .source_node_id(Some(node.to_owned()))
                    .source_version_id(optional_str(args, "source_version_id").map(str::to_owned))
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
/// node id through `/storage/{node}/read/` (`signing.txt:155`).
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
    // where a 404 is a genuine not-found (signing.txt:520/531/591).
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

/// AI instructions tool handler.
#[allow(clippy::too_many_lines)]
async fn handle_instructions(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    match action {
        "get-user" => match api::instructions::get_user_instructions(&client).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },
        "set-user" => {
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::set_user_instructions(&client, content).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "clear-user" => match api::instructions::delete_user_instructions(&client).await {
            Ok(v) => Ok(success_json(&v)),
            Err(e) => Ok(cli_err_to_result(&e)),
        },

        "get-org" => {
            let org_id = match required_str(args, "org_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::get_org_instructions(&client, org_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "set-org" => {
            let org_id = match required_str(args, "org_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::set_org_instructions(&client, org_id, content).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "clear-org" => {
            let org_id = match required_str(args, "org_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::delete_org_instructions(&client, org_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "get-org-user" => {
            let org_id = match required_str(args, "org_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::get_org_user_instructions(&client, org_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "set-org-user" => {
            let org_id = match required_str(args, "org_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::set_org_user_instructions(&client, org_id, content).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "clear-org-user" => {
            let org_id = match required_str(args, "org_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::delete_org_user_instructions(&client, org_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }

        "get-workspace" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::get_workspace_instructions(&client, workspace_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "set-workspace" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::set_workspace_instructions(&client, workspace_id, content)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "clear-workspace" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::delete_workspace_instructions(&client, workspace_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "get-workspace-user" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::get_workspace_user_instructions(&client, workspace_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "set-workspace-user" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::set_workspace_user_instructions(&client, workspace_id, content)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "clear-workspace-user" => {
            let workspace_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::delete_workspace_user_instructions(&client, workspace_id).await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }

        "get-share" => {
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::get_share_instructions(&client, share_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "set-share" => {
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::set_share_instructions(&client, share_id, content).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "clear-share" => {
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::delete_share_instructions(&client, share_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "get-share-user" => {
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::get_share_user_instructions(&client, share_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "set-share-user" => {
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let content = match required_str(args, "content") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::set_share_user_instructions(&client, share_id, content).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "clear-share-user" => {
            let share_id = match required_str(args, "share_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::instructions::delete_share_user_instructions(&client, share_id).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }

        _ => Ok(error_text(&format!(
            "Unknown instructions action: {action}"
        ))),
    }
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
        ToolRouter::new(Arc::new(McpState::new_unauthenticated_for_test(
            "https://api.fast.io/current",
        )))
    }

    /// An authenticated router whose client points at an unroutable base URL.
    /// Lets tests exercise the pre-network input validation (which runs after
    /// `require_auth`) without any real HTTP — a validation error is returned
    /// before any request is attempted.
    async fn authed_router() -> ToolRouter {
        let state = Arc::new(McpState::new_unauthenticated_for_test(
            "https://api.fast.io/current",
        ));
        state.set_token("test-token".to_owned()).await;
        ToolRouter::new(state)
    }

    #[test]
    fn list_tools_advertises_ripley_not_ai() {
        let tools = ToolRouter::list_tools().tools;
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
    fn mcp_search_success_normalizes_files_map_to_array_rows() {
        // The three MCP storage-search handlers (workspace `search`,
        // `handle_files_search`, `handle_ai_search`) emit
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
        let tools = ToolRouter::list_tools().tools;
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
        // ai.txt:265 makes `question` required for create; the handler must
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
        // ai.txt:894 caps `files` at 1-25; the handler must reject >25
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

    #[test]
    fn ripley_tool_advertises_phase2_actions() {
        let tools = ToolRouter::list_tools().tools;
        let ripley = tools
            .iter()
            .find(|t| t.name.as_ref() == "ripley")
            .expect("ripley tool present");
        let schema = serde_json::to_string(&ripley.input_schema).unwrap_or_default();
        for action in ["ask", "memory-get", "memory-set", "memory-delete"] {
            assert!(
                schema.contains(action),
                "ripley schema must advertise the `{action}` action, got: {schema}"
            );
        }
    }

    #[test]
    fn metadata_tool_advertises_extract_and_wait_action() {
        let tools = ToolRouter::list_tools().tools;
        let metadata = tools
            .iter()
            .find(|t| t.name.as_ref() == "metadata")
            .expect("metadata tool present");
        let schema = serde_json::to_string(&metadata.input_schema).unwrap_or_default();
        for action in ["extract", "extract-and-wait", "auto-match", "extract-all"] {
            assert!(
                schema.contains(action),
                "metadata schema must advertise `{action}`, got: {schema}"
            );
        }
        // New params surfaced.
        for param in ["batch_size", "force", "poll_interval"] {
            assert!(
                schema.contains(param),
                "metadata schema must advertise param `{param}`, got: {schema}"
            );
        }
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
        let tools = ToolRouter::list_tools().tools;
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

    #[test]
    fn saved_view_subpaths_are_workspace_level() {
        // FIX 2: saved views are WORKSPACE-level and keyed by template_id.
        // `metadata_api` prepends `/workspace/{id}/`, so the sub-paths must be
        // `metadata/view/` and `metadata/views/` — NOT the old node-scoped
        // `storage/{node}/metadata/view/...` shape.
        assert_eq!(super::METADATA_VIEW_SUBPATH, "metadata/view/");
        assert_eq!(super::METADATA_VIEWS_SUBPATH, "metadata/views/");
        assert!(
            !super::METADATA_VIEW_SUBPATH.contains("storage/"),
            "saved-view path must not be node-scoped"
        );
        assert!(
            !super::METADATA_VIEWS_SUBPATH.contains("storage/"),
            "saved-views list path must not be node-scoped"
        );
    }

    #[test]
    fn workspace_tool_advertises_saved_view_actions_and_config_param() {
        // FIX 2: the workspace tool exposes the workspace-level saved-view
        // actions (incl. the new `metadata-view-get`) and the `config` param
        // (form field for view-save); the retired `view_id` param is gone.
        let tools = ToolRouter::list_tools().tools;
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
                schema.contains(action),
                "workspace schema must advertise `{action}`, got: {schema}"
            );
        }
        assert!(
            schema.contains("config"),
            "workspace schema must advertise the saved-view `config` param"
        );
        assert!(
            !schema.contains("view_id"),
            "retired node-scoped `view_id` param must be gone from the workspace schema"
        );
    }

    #[tokio::test]
    async fn metadata_view_get_routes_to_handler() {
        // Proves `metadata-view-get` is wired (auth gate short-circuit vs the
        // unknown-action arm).
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("metadata-view-get".to_owned()),
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
    async fn memory_get_routes_to_handler() {
        // Unauthenticated → short-circuits at the auth gate inside handle_ai,
        // proving `memory-get` routed (vs the unknown-action arm).
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-get".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(text.contains("Not authenticated"), "got: {text}");
        assert!(!text.contains("Unknown ripley action"), "got: {text}");
    }

    #[tokio::test]
    async fn memory_set_rejects_over_cap_content() {
        // ai.txt: content is capped at 64KB; the api layer validates before
        // the round-trip, so an over-cap write surfaces a Parse error.
        let router = authed_router().await;
        let big = "a".repeat(65_537);
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-set".to_owned()));
        args.insert("context_type".to_owned(), Value::String("org".to_owned()));
        args.insert("context_id".to_owned(), Value::String("o1".to_owned()));
        args.insert("content".to_owned(), Value::String(big));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("65536") || text.contains("at most"),
            "over-cap memory content must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn memory_set_rejects_non_numeric_revision() {
        // A present-but-invalid revision must NOT be silently dropped (which
        // would downgrade a conditional write to last-writer-wins). It must be
        // rejected pre-network, BEFORE api::ai_memory::set runs. We pair it with
        // an over-cap content so that, had the revision been silently dropped,
        // the request would instead surface the 64KB content-cap error — proving
        // the revision check fires first.
        let router = authed_router().await;
        let big = "a".repeat(65_537);
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-set".to_owned()));
        args.insert("context_type".to_owned(), Value::String("org".to_owned()));
        args.insert("context_id".to_owned(), Value::String("o1".to_owned()));
        args.insert("content".to_owned(), Value::String(big));
        args.insert("revision".to_owned(), Value::String("abc".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("revision must be a non-negative integer"),
            "non-numeric revision must be rejected, got: {text}"
        );
        assert!(
            !text.contains("65536") && !text.contains("at most"),
            "revision check must fire before api::ai_memory::set, got: {text}"
        );
    }

    #[tokio::test]
    async fn memory_set_rejects_null_revision() {
        // An explicit `null` revision is PRESENT-but-invalid, not absent. Per the
        // lost-update contract (orgs.txt:2265) only a truly absent key means an
        // unconditional write; a present `null` must be rejected pre-network so a
        // conditional write is never silently downgraded to last-writer-wins. As
        // with the other rejection test we pair it with over-cap content: surfacing
        // the revision error (and NOT the 64KB content-cap error) proves the
        // revision check fires before api::ai_memory::set.
        let router = authed_router().await;
        let big = "a".repeat(65_537);
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-set".to_owned()));
        args.insert("context_type".to_owned(), Value::String("org".to_owned()));
        args.insert("context_id".to_owned(), Value::String("o1".to_owned()));
        args.insert("content".to_owned(), Value::String(big));
        args.insert("revision".to_owned(), Value::Null);
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("revision must be a non-negative integer"),
            "a null revision must be rejected, got: {text}"
        );
        assert!(
            !text.contains("65536") && !text.contains("at most"),
            "revision check must fire before api::ai_memory::set, got: {text}"
        );
    }

    #[tokio::test]
    async fn memory_set_accepts_numeric_string_revision() {
        // A valid numeric-string revision passes the MCP-handler check and
        // proceeds into api::ai_memory::set as a CONDITIONAL write. With over-cap
        // content, reaching the content-cap error (rather than the revision
        // error) proves the revision was accepted and forwarded.
        let router = authed_router().await;
        let big = "a".repeat(65_537);
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-set".to_owned()));
        args.insert("context_type".to_owned(), Value::String("org".to_owned()));
        args.insert("context_id".to_owned(), Value::String("o1".to_owned()));
        args.insert("content".to_owned(), Value::String(big));
        args.insert("revision".to_owned(), Value::String("5".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("revision must be a non-negative integer"),
            "a valid numeric-string revision must be accepted, got: {text}"
        );
        assert!(
            text.contains("65536") || text.contains("at most"),
            "a valid revision should proceed into the write path, got: {text}"
        );
    }

    #[tokio::test]
    async fn memory_set_accepts_number_revision() {
        // Same as above but the revision arrives as a JSON number, not a string.
        let router = authed_router().await;
        let big = "a".repeat(65_537);
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-set".to_owned()));
        args.insert("context_type".to_owned(), Value::String("org".to_owned()));
        args.insert("context_id".to_owned(), Value::String("o1".to_owned()));
        args.insert("content".to_owned(), Value::String(big));
        args.insert("revision".to_owned(), Value::Number(5.into()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("revision must be a non-negative integer"),
            "a valid JSON-number revision must be accepted, got: {text}"
        );
        assert!(
            text.contains("65536") || text.contains("at most"),
            "a valid revision should proceed into the write path, got: {text}"
        );
    }

    #[tokio::test]
    async fn memory_set_omitted_revision_proceeds_unconditionally() {
        // No revision key → unconditional write. The handler must NOT emit the
        // revision-validation error and must proceed into the write path (here
        // surfacing the over-cap content error, proving it got past the
        // revision logic).
        let router = authed_router().await;
        let big = "a".repeat(65_537);
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-set".to_owned()));
        args.insert("context_type".to_owned(), Value::String("org".to_owned()));
        args.insert("context_id".to_owned(), Value::String("o1".to_owned()));
        args.insert("content".to_owned(), Value::String(big));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("revision must be a non-negative integer"),
            "an omitted revision must not trigger the revision error, got: {text}"
        );
        assert!(
            text.contains("65536") || text.contains("at most"),
            "an omitted revision should proceed unconditionally, got: {text}"
        );
    }

    #[tokio::test]
    async fn memory_rejects_share_context_type() {
        // Memory has no share scope; context_type must be org or workspace.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("memory-get".to_owned()));
        args.insert("context_type".to_owned(), Value::String("share".to_owned()));
        args.insert("context_id".to_owned(), Value::String("s1".to_owned()));
        let res = router
            .call_tool("ripley", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("org") && text.contains("workspace"),
            "share context_type must be rejected for memory, got: {text}"
        );
    }

    #[tokio::test]
    async fn ai_autotitle_rejects_workspace_context() {
        // autotitle is SHARE-ONLY (ai.txt:1079-1112). A workspace context must
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
        // transactions is WORKSPACE-ONLY (ai.txt:935-981). A share context must
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
    fn json_value_field_to_string_handles_string_and_numeric() {
        // FIX 4: state normalisation must accept a string OR a numeric value,
        // matching the CLI wait loop's `extract_string_field`.
        let s = json!({"state": "complete"});
        assert_eq!(
            super::json_value_field_to_string(&s, "state").as_deref(),
            Some("complete")
        );
        let n = json!({"state": 3});
        assert_eq!(
            super::json_value_field_to_string(&n, "state").as_deref(),
            Some("3")
        );
        let missing = json!({});
        assert!(super::json_value_field_to_string(&missing, "state").is_none());
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
        for action in [
            "metadata-template-preview-match",
            "metadata-template-suggest-fields",
            "metadata-extract",
            "metadata-extract-and-wait",
        ] {
            assert_spend_rejected_without_confirm("workspace", action).await;
            assert_spend_proceeds_with_confirm("workspace", action).await;
        }
        // Standalone metadata tool surface.
        for action in ["auto-match", "extract-all", "extract", "extract-and-wait"] {
            assert_spend_rejected_without_confirm("metadata", action).await;
            assert_spend_proceeds_with_confirm("metadata", action).await;
        }
    }

    #[tokio::test]
    async fn read_only_metadata_actions_do_not_require_confirm_ai_spend() {
        // A read-only action (metadata-list) must NOT be blocked by the spend
        // guard: with no confirm_ai_spend it still reaches param validation
        // rather than the spend rejection.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("metadata-list".to_owned()),
        );
        let res = router
            .call_tool("workspace", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("spends AI credits"),
            "read-only metadata-list must not be spend-gated, got: {text}"
        );
        assert!(
            text.contains("Missing required parameter"),
            "metadata-list should reach param validation, got: {text}"
        );
    }

    #[test]
    fn workspace_and_metadata_schemas_advertise_confirm_ai_spend() {
        let tools = ToolRouter::list_tools().tools;
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

    #[tokio::test]
    async fn extract_all_rejects_present_but_empty_fields() {
        // confirm_ai_spend=true to pass the spend guard, workspace_id +
        // template_id present, fields="" → present-but-empty rejection BEFORE
        // any network call (would otherwise widen to the full schema).
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("extract-all".to_owned()));
        args.insert("confirm_ai_spend".to_owned(), Value::Bool(true));
        args.insert("workspace_id".to_owned(), Value::String("ws1".to_owned()));
        args.insert("template_id".to_owned(), Value::String("mt_1".to_owned()));
        args.insert("fields".to_owned(), Value::String(String::new()));
        let res = router
            .call_tool("metadata", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("fields is present but empty"),
            "present-but-empty fields must be rejected, got: {text}"
        );
    }

    #[test]
    fn legacy_workflow_tools_are_flagged() {
        // The four legacy workflow primitive tools must advertise `[legacy]`.
        let tools = ToolRouter::list_tools().tools;
        for name in ["task", "worklog", "approval", "todo"] {
            let tool = tools
                .iter()
                .find(|t| t.name.as_ref() == name)
                .unwrap_or_else(|| panic!("{name} tool present"));
            let desc = tool.description.as_deref().unwrap_or_default();
            assert!(
                desc.contains("[legacy]"),
                "{name} tool description must contain [legacy], got: {desc}"
            );
        }
    }

    #[test]
    fn worklog_entity_type_defaults_to_profile() {
        // Regression: the MCP worklog default was `workspace` (invalid); it
        // must default to `profile`.
        let empty = Map::new();
        assert_eq!(super::worklog_entity_type(&empty), "profile");
    }

    #[test]
    fn worklog_entity_type_accepts_node() {
        // `node` is a documented worklog entity type and must pass through.
        let mut args = Map::new();
        args.insert("entity_type".to_owned(), Value::String("node".to_owned()));
        assert_eq!(super::worklog_entity_type(&args), "node");
    }

    #[test]
    fn approval_profile_id_prefers_profile_id_then_workspace_id() {
        let mut with_profile = Map::new();
        with_profile.insert("profile_id".to_owned(), Value::String("p1".to_owned()));
        assert_eq!(super::approval_profile_id(&with_profile).ok(), Some("p1"));

        // Legacy fallback to workspace_id.
        let mut with_ws = Map::new();
        with_ws.insert("workspace_id".to_owned(), Value::String("w1".to_owned()));
        assert_eq!(super::approval_profile_id(&with_ws).ok(), Some("w1"));

        // Neither → error.
        assert!(super::approval_profile_id(&Map::new()).is_err());
    }

    #[tokio::test]
    async fn approval_update_requires_a_field() {
        // The MCP approval `update` action must reject when no mutable field is
        // supplied (mirrors the CLI guard) before any network call.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("update".to_owned()));
        args.insert("profile_id".to_owned(), Value::String("w1".to_owned()));
        args.insert("approval_id".to_owned(), Value::String("a1".to_owned()));
        let res = router
            .call_tool("approval", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("at least one of"),
            "empty approval update must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn approval_details_without_profile_id_uses_legacy_unscoped_route() {
        // FIX 2: per-approval action routes (details/approve/reject/update/
        // delete) no longer hard-require a scope. With no profile_id the legacy
        // unscoped route is used, so the call must NOT short-circuit with a
        // missing-profile_id error — it proceeds past validation (and only the
        // unroutable test network fails it). Backward compat for the historical
        // `approval details <id>` syntax.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("details".to_owned()));
        args.insert("approval_id".to_owned(), Value::String("a1".to_owned()));
        let res = router
            .call_tool("approval", args)
            .await
            .expect("call_tool ok");
        let text = result_to_string(&res);
        assert!(
            !text.contains("Missing required parameter: profile_id"),
            "details without scope must not demand profile_id, got: {text}"
        );
    }

    #[test]
    fn approval_scope_opt_some_when_profile_id_present() {
        let mut args = Map::new();
        args.insert("profile_id".to_owned(), Value::String("p1".to_owned()));
        args.insert("profile_type".to_owned(), Value::String("share".to_owned()));
        assert_eq!(super::approval_scope_opt(&args), Some(("share", "p1")));

        // Default profile_type is workspace; legacy workspace_id is accepted.
        let mut legacy = Map::new();
        legacy.insert("workspace_id".to_owned(), Value::String("w1".to_owned()));
        assert_eq!(
            super::approval_scope_opt(&legacy),
            Some(("workspace", "w1"))
        );
    }

    #[test]
    fn approval_scope_opt_none_when_no_profile() {
        // No profile_id and no workspace_id → legacy unscoped route.
        assert_eq!(super::approval_scope_opt(&Map::new()), None);
    }

    #[test]
    fn approval_properties_parses_object_and_string_and_rejects_scalar() {
        // Object passed directly.
        let mut obj = Map::new();
        obj.insert("properties".to_owned(), json!({"k": "v"}));
        let parsed = super::approval_properties(&obj).expect("object ok");
        assert_eq!(parsed.expect("some")["k"], "v");

        // JSON-object string is parsed.
        let mut s = Map::new();
        s.insert(
            "properties".to_owned(),
            Value::String(r#"{"k":1}"#.to_owned()),
        );
        let parsed = super::approval_properties(&s).expect("string ok");
        assert_eq!(parsed.expect("some")["k"], 1);

        // A non-object scalar is rejected.
        let mut bad = Map::new();
        bad.insert("properties".to_owned(), json!(42));
        assert!(super::approval_properties(&bad).is_err());

        // Absent → None.
        assert!(
            super::approval_properties(&Map::new())
                .expect("absent ok")
                .is_none()
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

    // ─── Workflow Orchestration MCP tool ────────────────────────────────────

    /// The set of action names the `workflow` tool advertises in its registry.
    fn workflow_tool_actions() -> Vec<&'static str> {
        super::TOOL_DEFS
            .iter()
            .find(|d| d.name == "workflow")
            .expect("workflow tool registered")
            .actions
            .to_vec()
    }

    #[test]
    fn workflow_tool_is_registered_and_offload_oriented() {
        let tools = ToolRouter::list_tools().tools;
        let wf = tools
            .iter()
            .find(|t| t.name.as_ref() == "workflow")
            .expect("workflow tool present");
        let desc = wf.description.as_deref().unwrap_or_default();
        // Offload framing + the integrity-vs-authenticity caveat must be present.
        assert!(
            desc.contains("OFFLOAD"),
            "description should steer to offload: {desc}"
        );
        assert!(desc.contains("integrity-only") || desc.contains("integrity"));
    }

    #[test]
    fn workflow_tool_advertises_read_and_drive_actions() {
        let actions = workflow_tool_actions();
        for expected in [
            "describe",
            "state",
            "instantiate",
            "instantiate-and-wait",
            "trigger-fire-and-wait",
            "audit-export-and-download",
            "obligation-resolve",
            "step-output",
        ] {
            assert!(
                actions.contains(&expected),
                "workflow tool must advertise '{expected}'"
            );
        }
    }

    #[test]
    fn workflow_tool_omits_admin_destructive_and_crypto_actions() {
        // These admin/destructive/crypto actions are CLI-binary-only and MUST
        // NOT be reachable through the MCP tool.
        let actions = workflow_tool_actions();
        for forbidden in [
            "cancel",
            "create",
            "update",
            "delete",
            "purge",
            "transfer",
            "rotate-inbound-key",
            "grant-add",
            "grant-revoke",
            "step-cancel",
            "template-create",
            "template-publish",
            "template-withdraw",
            "template-deprecate",
            "trigger-create",
            "trigger-update",
            "trigger-delete",
            "trigger-purge",
            "schema-set",
            "schema-derive",
            "redaction-request",
            "redaction-confirm",
            "outbound-create",
            "outbound-rotate-secret",
            "pool-create",
            "pool-delete",
            "realtime-token",
            "review-decision",
            "audit-check-integrity",
        ] {
            assert!(
                !actions.contains(&forbidden),
                "workflow MCP tool must NOT advertise admin/destructive/crypto action '{forbidden}'"
            );
        }
    }

    #[tokio::test]
    async fn workflow_describe_needs_no_auth_and_lists_actions() {
        // `describe` must work unauthenticated and enumerate every advertised
        // action with required/optional params.
        let router = unauthed_router();
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("describe".to_owned()));
        let res = router.call_tool("workflow", args).await.expect("ok");
        let text = result_to_string(&res);
        // The describe payload (rendered markdown) names key actions and the
        // CLI-only carve-out.
        assert!(
            text.contains("instantiate-and-wait"),
            "describe should list compounds: {text}"
        );
        assert!(
            text.contains("cli_only_actions") || text.contains("cli only"),
            "describe should name CLI-only ops"
        );
        // describe accuracy: every advertised action appears in the payload.
        for action in workflow_tool_actions() {
            assert!(
                text.contains(action),
                "describe payload must document advertised action '{action}'"
            );
        }
    }

    #[tokio::test]
    async fn workflow_unknown_action_points_to_cli_for_admin_ops() {
        let router = authed_router().await;
        let mut args = Map::new();
        // An admin action that is intentionally not handled here.
        args.insert("action".to_owned(), Value::String("create".to_owned()));
        let res = router.call_tool("workflow", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("CLI-only workflow action") && text.contains("CLI-binary-only"),
            "admin actions must be rejected with a CLI pointer, got: {text}"
        );
    }

    #[tokio::test]
    async fn workflow_cancel_is_cli_only() {
        // `cancel` is a terminal lifecycle mutation: it must NOT be reachable
        // over MCP and must route to the CLI-only fallback with a clear pointer.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("cancel".to_owned()));
        args.insert(
            "workflow_id".to_owned(),
            Value::String("4011234567890123456".to_owned()),
        );
        let res = router.call_tool("workflow", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("CLI-only workflow action") && text.contains("fastio workflow cancel"),
            "cancel must be rejected over MCP with a CLI pointer, got: {text}"
        );
        // And cancel must not be advertised as an MCP action.
        assert!(
            !workflow_tool_actions().contains(&"cancel"),
            "cancel must not appear in the workflow MCP action list"
        );
    }

    #[tokio::test]
    async fn workflow_instantiate_requires_idempotency_key() {
        // The MCP surface has NO auto-generate; a missing key is a hard error
        // before any network call.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String("instantiate".to_owned()));
        args.insert(
            "workflow_id".to_owned(),
            Value::String("4011234567890123456".to_owned()),
        );
        let res = router.call_tool("workflow", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("idempotency_key is required"),
            "instantiate without a key must be rejected, got: {text}"
        );
    }

    #[tokio::test]
    async fn workflow_trigger_fire_requires_idempotency_key() {
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("trigger-fire".to_owned()),
        );
        args.insert("trigger_id".to_owned(), Value::String("trabc-1".to_owned()));
        let res = router.call_tool("workflow", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(text.contains("idempotency_key is required"), "got: {text}");
    }

    #[tokio::test]
    async fn workflow_obligation_list_requires_workflow_id_anchor() {
        // workflow_id is the required authz anchor for obligation listing.
        let router = authed_router().await;
        let mut args = Map::new();
        args.insert(
            "action".to_owned(),
            Value::String("obligation-list".to_owned()),
        );
        let res = router.call_tool("workflow", args).await.expect("ok");
        let text = result_to_string(&res);
        assert!(
            text.contains("Missing required parameter: workflow_id"),
            "obligation-list must require workflow_id, got: {text}"
        );
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
        let tools = ToolRouter::list_tools().tools;
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
        ] {
            assert!(
                !actions.contains(&forbidden),
                "sign MCP tool must NOT advertise outward/destructive action '{forbidden}'"
            );
        }
        // The reversible read + draft-drive actions MUST be present.
        for present in [
            "envelope-create",
            "envelope-update",
            "envelope-list",
            "envelope-get",
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
        // parent_type / parent_id params (F11, F35).
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
        // convention for multi-action tools (e.g. `workflow`, `apps`): the
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

    #[tokio::test]
    async fn sign_describe_communicates_workspace_id_requirement() {
        // The describe payload (rendered markdown) is the authoritative
        // per-action requirement source now that the schema marks workspace_id
        // optional (F3). It must still communicate that workspace_id is
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
        // must report that distinctly (F3), not "CLI-binary-only".
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
        // audit-certificate guidance steers to "terminal state" (completed OR
        // voided), mirroring the CLI surface.
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
        // capabilities.signing (F7; mirrors the CLI's map_signing_error).
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
        // signing-scoped notes and must not steer to "auth login" (F7).
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
        assert!(
            route.contains("9992") && route.to_lowercase().contains("recognize"),
            "got: {route}"
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
    fn sign_err_artifact_9992_404_is_removed_route_not_poll() {
        // A router-level 9992 (also HTTP 404) on an artifact fetch must NOT be
        // reframed as "not ready — poll and retry" (an agent would poll a dead
        // route forever); it must surface the removed/renamed-route framing.
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
                m.contains("9992") && m.to_lowercase().contains("recognize"),
                "9992 on {op:?} must flag an unrecognized/removed route: {m}"
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

    // ─── Phase 7 billing: org tool action surface + subscribe onboarding ─────

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
        // The get/thumbnail preauthorize response (storage.txt:2343) carries a
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
}
