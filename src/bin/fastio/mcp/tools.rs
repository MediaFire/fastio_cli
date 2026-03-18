/// MCP tool definitions and action-based routing for the Fast.io CLI.
///
/// Each tool corresponds to an API domain (auth, org, workspace, etc.)
/// and uses an `action` parameter to select the specific operation.
/// All tool handlers delegate to the existing `src/api/` functions.
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
fn success_json(value: &Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
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
        description: "Authentication: sign in, sign out, check status, manage API keys, 2FA, OAuth sessions, email/password management.",
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
            ("code", "Reset code (password-reset)", false),
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
        description: "User profile: view, update, search users, manage invitations and assets.",
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
        ],
        params: &[
            ("first_name", "First name (update)", false),
            ("last_name", "Last name (update)", false),
            ("display_name", "Display name (update)", false),
            ("query", "Search query (search)", false),
            ("confirmation", "Confirmation string (close)", false),
            ("user_id", "User ID (details, asset-list)", false),
            (
                "invitation_id",
                "Invitation ID (invitations-details)",
                false,
            ),
            ("asset_type", "Asset type name (asset-delete)", false),
        ],
    },
    ToolDef {
        name: "org",
        description: "Organizations: list, create, view, update, delete orgs; billing, members, invitations, transfer tokens, discovery, assets, workspaces, shares.",
        actions: &[
            "list",
            "create",
            "info",
            "update",
            "delete",
            "billing-info",
            "billing-plans",
            "billing-meters",
            "billing-cancel",
            "billing-activate",
            "billing-reset",
            "billing-members",
            "billing-create",
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
            ("email", "Member email (invite)", false),
            ("role", "Member role", false),
            ("member_id", "Member ID", false),
            ("new_owner_id", "New owner user ID (transfer)", false),
            ("meter", "Meter name (billing-meters)", false),
            ("start_time", "Start time (billing-meters)", false),
            ("end_time", "End time (billing-meters)", false),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
            ("plan_id", "Billing plan ID (billing-create)", false),
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
        description: "Workspaces: list, create, view, update, delete, archive/unarchive, members, shares, notes, quickshares, metadata, import/export, workflow.",
        actions: &[
            "list",
            "create",
            "info",
            "update",
            "delete",
            "enable-workflow",
            "disable-workflow",
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
            "metadata-template-delete",
            "metadata-template-list",
            "metadata-template-details",
            "metadata-template-settings",
            "metadata-template-update",
            "metadata-delete",
            "metadata-details",
            "metadata-extract",
            "metadata-list",
            "metadata-template-select",
            "metadata-templates-in-use",
            "metadata-update",
            "metadata-view-save",
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
            ("content", "Note content", false),
            ("template_id", "Metadata template ID", false),
            ("category", "Metadata template category", false),
            ("fields", "JSON-encoded field definitions (metadata)", false),
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
                "view_id",
                "Metadata view ID (view-save, view-delete)",
                false,
            ),
            (
                "filter",
                "Filter: enabled/disabled (metadata-template-list)",
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
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("folder", "Target folder ID (defaults to root)", false),
            ("name", "Filename (text, create-session)", false),
            ("content", "Text content (text upload)", false),
            ("url", "Source URL (url import)", false),
            (
                "upload_key",
                "Upload key/ID (finalize, status, cancel, chunk-status, chunk-delete)",
                false,
            ),
            ("filesize", "File size in bytes (create-session)", false),
            ("chunk_num", "Chunk number (chunk-delete)", false),
            ("upload_id", "Upload ID (web-cancel, web-status)", false),
        ],
    },
    ToolDef {
        name: "download",
        description: "Downloads: get file download URLs, folder ZIP URLs, quickshare details.",
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
            ("download_enabled", "Allow downloads (true/false)", false),
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
        name: "ai",
        description: "AI: chat create/list/details/update/delete/publish, message send/list/details/read, search, share-generate, transactions, autotitle.",
        actions: &[
            "chat-create",
            "chat-list",
            "chat-details",
            "chat-update",
            "chat-delete",
            "chat-publish",
            "message-send",
            "message-list",
            "message-details",
            "message-read",
            "share-generate",
            "transactions",
            "autotitle",
            "search",
        ],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("query_text", "Question or search query", false),
            (
                "type",
                "Chat type: chat or chat_with_files (chat-create)",
                false,
            ),
            ("chat_id", "Chat ID", false),
            ("message_id", "Message ID", false),
            ("name", "New chat name (chat-update)", false),
            (
                "node_ids",
                "Comma-separated node IDs (share-generate workspace)",
                false,
            ),
            (
                "files",
                "Comma-separated file IDs (share-generate share)",
                false,
            ),
            ("personality", "Response style: concise or detailed", false),
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
        description: "Events: search, summarize, details, activity-list, activity-poll.",
        actions: &[
            "search",
            "summarize",
            "details",
            "activity-list",
            "activity-poll",
        ],
        params: &[
            ("workspace_id", "Workspace ID", false),
            ("share_id", "Share ID", false),
            ("user_id", "User ID", false),
            ("org_id", "Organization ID", false),
            ("event_id", "Event ID (details)", false),
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
        description: "Previews: get preview URLs and transform URLs for files.",
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
        description: "Tasks: manage task lists and tasks. list-lists, create-list, list-details, update-list, delete-list, list-tasks, create-task, task-details, update-task, delete-task, change-status, assign-task, bulk-status, move-task, reorder-tasks, reorder-lists.",
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
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "worklog",
        description: "Worklogs: list, append, interject, details, acknowledge, unacknowledged.",
        actions: &[
            "list",
            "append",
            "interject",
            "details",
            "acknowledge",
            "unacknowledged",
        ],
        params: &[
            (
                "entity_type",
                "Entity type: task, task_list, or profile",
                false,
            ),
            ("entity_id", "Entity ID", false),
            ("entry_id", "Worklog entry ID (details, acknowledge)", false),
            ("message", "Worklog content", false),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "approval",
        description: "Approvals: list, request, approve, reject approval workflows.",
        actions: &["list", "request", "approve", "reject"],
        params: &[
            ("workspace_id", "Workspace ID (list)", false),
            ("approval_id", "Approval ID (approve, reject)", false),
            ("entity_type", "Entity type (request)", false),
            ("entity_id", "Entity ID (request)", false),
            ("description", "Description (request)", false),
            ("approver_id", "Approver user ID (request)", false),
            ("comment", "Comment (approve, reject)", false),
            ("status", "Status filter (list)", false),
            ("limit", "Pagination limit", false),
            ("offset", "Pagination offset", false),
        ],
    },
    ToolDef {
        name: "todo",
        description: "Todos: list, create, details, update, delete, toggle, bulk-toggle.",
        actions: &[
            "list",
            "create",
            "details",
            "update",
            "toggle",
            "delete",
            "bulk-toggle",
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
        description: "File locking: acquire, status, release exclusive locks on files in workspaces or shares.",
        actions: &["acquire", "status", "release"],
        params: &[
            ("context_type", "Context: workspace or share", false),
            ("context_id", "Workspace or share ID", false),
            ("node_id", "File node ID", false),
        ],
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
            "ai" => handle_ai(&self.state, action, &args).await,
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
        "billing-info" => handle_org_billing_info(state, args).await,
        "billing-plans" => handle_org_billing_plans(state, args).await,
        "billing-meters" => handle_org_billing_meters(state, args).await,
        "members-list" => handle_org_members_list(state, args).await,
        "members-invite" => handle_org_members_invite(state, args).await,
        "members-remove" => handle_org_members_remove(state, args).await,
        "members-update-role" => handle_org_members_update_role(state, args).await,
        "transfer" => handle_org_transfer(state, args).await,
        "discover" | "discover-available" => handle_org_discover(state, args).await,
        "billing-cancel" => handle_org_billing_cancel(state, args).await,
        "billing-activate" => handle_org_billing_activate(state, args).await,
        "billing-reset" => handle_org_billing_reset(state, args).await,
        "billing-members" => handle_org_billing_members(state, args).await,
        "billing-create" => handle_org_billing_create(state, args).await,
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

async fn handle_org_billing_info(
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
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_billing_plans(
    state: &McpState,
    _args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    match api::org::list_billing_plans(&client).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
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
        org_id,
        meter,
        optional_str(args, "start_time"),
        optional_str(args, "end_time"),
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
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
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::billing_cancel(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_billing_activate(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::billing_activate(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_billing_reset(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::billing_reset(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
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
    match api::org::billing_members(&client, org_id, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
}

async fn handle_org_billing_create(
    state: &McpState,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    let client = state.client().read().await;
    let org_id = match required_str(args, "org_id") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    match api::org::billing_create(&client, org_id, optional_str(args, "plan_id")).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
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
    match api::org::get_limits(&client, org_id).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
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
            match api::workspace::search_workspace(
                &client,
                ws_id,
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
            match api::workspace::create_note(
                &client,
                ws_id,
                parent,
                name,
                optional_str(args, "content"),
            )
            .await
            {
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
            match api::workspace::read_note(&client, ws_id, node_id).await {
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
            let body =
                json!({ "name": name, "description": desc, "category": cat, "fields": fields });
            match api::workspace::metadata_api(
                &client,
                ws_id,
                "metadata/templates/",
                "POST",
                Some(&body),
                None,
            )
            .await
            {
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
            match api::workspace::metadata_api(&client, ws_id, &sub, "DELETE", None, None).await {
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
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None).await {
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
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None).await {
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
            let mut body = serde_json::Map::new();
            if let Some(v) = optional_bool(args, "enabled") {
                body.insert("enabled".to_owned(), Value::Bool(v));
            }
            if let Some(v) = optional_u8(args, "priority") {
                body.insert("priority".to_owned(), Value::Number(v.into()));
            }
            let sub = format!("metadata/templates/{}/settings/", urlencoding::encode(tid));
            match api::workspace::metadata_api(
                &client,
                ws_id,
                &sub,
                "POST",
                Some(&Value::Object(body)),
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
            let mut body = serde_json::Map::new();
            if let Some(v) = optional_str(args, "name") {
                body.insert("name".to_owned(), Value::String(v.to_owned()));
            }
            if let Some(v) = optional_str(args, "description") {
                body.insert("description".to_owned(), Value::String(v.to_owned()));
            }
            if let Some(v) = optional_str(args, "category") {
                body.insert("category".to_owned(), Value::String(v.to_owned()));
            }
            if let Some(v) = optional_str(args, "fields") {
                body.insert("fields".to_owned(), Value::String(v.to_owned()));
            }
            match api::workspace::metadata_api(
                &client,
                ws_id,
                &sub,
                "POST",
                Some(&Value::Object(body)),
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
            let body = optional_str(args, "keys").map(|k| json!({ "keys": k }));
            match api::workspace::metadata_api(&client, ws_id, &sub, "DELETE", body.as_ref(), None)
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
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!("storage/{}/metadata/details/", urlencoding::encode(nid));
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-extract" => {
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
            let sub = format!("storage/{}/metadata/extract/", urlencoding::encode(nid));
            let body = json!({ "template_id": tid });
            match api::workspace::metadata_api(&client, ws_id, &sub, "POST", Some(&body), None)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
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
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, p).await {
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
            match api::workspace::metadata_api(&client, ws_id, &sub, "POST", None, None).await {
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
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, p).await {
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
            let body = json!({ "key_values": kv });
            match api::workspace::metadata_api(&client, ws_id, &sub, "POST", Some(&body), None)
                .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-view-save" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = if let Some(vid) = optional_str(args, "view_id") {
                format!(
                    "storage/{}/metadata/view/{}/",
                    urlencoding::encode(nid),
                    urlencoding::encode(vid)
                )
            } else {
                format!("storage/{}/metadata/view/", urlencoding::encode(nid))
            };
            let mut body = serde_json::Map::new();
            if let Some(v) = optional_str(args, "name") {
                body.insert("name".to_owned(), Value::String(v.to_owned()));
            }
            if let Some(v) = optional_str(args, "template_id") {
                body.insert("template_id".to_owned(), Value::String(v.to_owned()));
            }
            if let Some(v) = optional_str(args, "filters") {
                body.insert("filters".to_owned(), Value::String(v.to_owned()));
            }
            if let Some(v) = optional_str(args, "order_by") {
                body.insert("order_by".to_owned(), Value::String(v.to_owned()));
            }
            if let Some(v) = optional_bool(args, "order_desc") {
                body.insert("order_desc".to_owned(), Value::Bool(v));
            }
            match api::workspace::metadata_api(
                &client,
                ws_id,
                &sub,
                "POST",
                Some(&Value::Object(body)),
                None,
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-view-delete" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let vid = match required_str(args, "view_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!(
                "storage/{}/metadata/view/{}/",
                urlencoding::encode(nid),
                urlencoding::encode(vid)
            );
            match api::workspace::metadata_api(&client, ws_id, &sub, "DELETE", None, None).await {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "metadata-views-list" => {
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let nid = match required_str(args, "node_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            let sub = format!("storage/{}/metadata/views/", urlencoding::encode(nid));
            match api::workspace::metadata_api(&client, ws_id, &sub, "GET", None, None).await {
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
    match api::storage::search_files(&client, ws_id, query, None, None).await {
        Ok(v) => Ok(success_json(&v)),
        Err(e) => Ok(cli_err_to_result(&e)),
    }
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
                    Ok(success_json(&json!({
                        "download_url": url,
                        "download_token": token,
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
        "chat-create" => handle_ai_chat_create(state, args).await,
        "chat-list" => handle_ai_chat_list(state, args).await,
        "chat-details" => handle_ai_chat_details(state, args).await,
        "chat-update" => handle_ai_chat_update(state, args).await,
        "chat-delete" => handle_ai_chat_delete(state, args).await,
        "chat-publish" => handle_ai_chat_publish(state, args).await,
        "message-send" => handle_ai_message_send(state, args).await,
        "message-list" => handle_ai_message_list(state, args).await,
        "message-details" | "message-read" => handle_ai_message_details(state, args).await,
        "share-generate" => handle_ai_share_generate(state, args).await,
        "transactions" => handle_ai_transactions(state, args).await,
        "autotitle" => handle_ai_autotitle(state, args).await,
        "search" => handle_ai_search(state, args).await,
        _ => Ok(error_text(&format!("Unknown ai action: {action}"))),
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
    let mut body = serde_json::json!({
        "type": chat_type,
        "personality": optional_str(args, "personality").unwrap_or("detailed"),
    });
    if let Some(q) = optional_str(args, "query_text") {
        body["question"] = Value::String(q.to_owned());
    }
    if let Some(v) = optional_str(args, "privacy") {
        body["privacy"] = Value::String(v.to_owned());
    }
    if let Some(v) = optional_str(args, "files_scope") {
        body["files_scope"] = Value::String(v.to_owned());
    }
    if let Some(v) = optional_str(args, "folders_scope") {
        body["folders_scope"] = Value::String(v.to_owned());
    }
    if let Some(v) = optional_str(args, "files_attach") {
        body["files_attach"] = Value::String(v.to_owned());
    }
    match api::ai::ai_api(
        &client,
        ctx_type,
        ctx_id,
        "chat/",
        "POST",
        Some(&body),
        None,
    )
    .await
    {
        Ok(v) => Ok(success_json(&v)),
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
    let sub = if optional_bool(args, "include_deleted").unwrap_or(false) && ctx_type == "share" {
        "chat/list/deleted/"
    } else {
        "chat/list/"
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
    let sub = format!("chat/{}/details/", urlencoding::encode(chat_id));
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
    let body = serde_json::json!({ "name": name });
    let sub = format!("chat/{}/update/", urlencoding::encode(chat_id));
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "POST", Some(&body), None).await {
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
    let sub = format!("chat/{}/", urlencoding::encode(chat_id));
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
    let sub = format!("chat/{}/publish/", urlencoding::encode(chat_id));
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "POST", None, None).await {
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
    let mut body = serde_json::json!({ "question": query });
    if let Some(v) = optional_str(args, "personality") {
        body["personality"] = Value::String(v.to_owned());
    }
    if let Some(v) = optional_str(args, "files_scope") {
        body["files_scope"] = Value::String(v.to_owned());
    }
    if let Some(v) = optional_str(args, "folders_scope") {
        body["folders_scope"] = Value::String(v.to_owned());
    }
    if let Some(v) = optional_str(args, "files_attach") {
        body["files_attach"] = Value::String(v.to_owned());
    }
    let sub = format!("chat/{}/message/", urlencoding::encode(chat_id));
    match api::ai::ai_api(&client, ctx_type, ctx_id, &sub, "POST", Some(&body), None).await {
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
    let sub = format!("chat/{}/messages/list/", urlencoding::encode(chat_id));
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
        "chat/{}/message/{}/details/",
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
    let body = if ctx_type == "workspace" {
        let ids_str = match required_str(args, "node_ids") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        serde_json::json!({ "nodes": ids_str })
    } else {
        let ids_str = match required_str(args, "files") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let file_ids: Vec<String> = ids_str.split(',').map(|s| s.trim().to_owned()).collect();
        serde_json::json!({ "files": file_ids })
    };
    match api::ai::ai_api(
        &client,
        ctx_type,
        ctx_id,
        "share/",
        "POST",
        Some(&body),
        None,
    )
    .await
    {
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
    match api::ai::ai_api(
        &client,
        ctx_type,
        ctx_id,
        "transactions/",
        "GET",
        None,
        None,
    )
    .await
    {
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
    let mut body = serde_json::Map::new();
    if let Some(c) = optional_str(args, "context") {
        body.insert("context".to_owned(), Value::String(c.to_owned()));
    }
    match api::ai::ai_api(
        &client,
        ctx_type,
        ctx_id,
        "autotitle/",
        "POST",
        Some(&Value::Object(body)),
        None,
    )
    .await
    {
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
    let query = match required_str(args, "query_text") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let mut params = std::collections::HashMap::new();
    params.insert("question".to_owned(), query.to_owned());
    if let Some(l) = optional_str(args, "limit") {
        params.insert("limit".to_owned(), l.to_owned());
    }
    if let Some(o) = optional_str(args, "offset") {
        params.insert("offset".to_owned(), o.to_owned());
    }
    if let Some(v) = optional_str(args, "files_scope") {
        params.insert("files_scope".to_owned(), v.to_owned());
    }
    if let Some(v) = optional_str(args, "folders_scope") {
        params.insert("folders_scope".to_owned(), v.to_owned());
    }
    match api::ai::ai_api(
        &client,
        ctx_type,
        ctx_id,
        "search/",
        "GET",
        None,
        Some(&params),
    )
    .await
    {
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
                Ok(v) => Ok(success_json(&v)),
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
        _ => Ok(error_text(&format!("Unknown task action: {action}"))),
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

/// Worklog tool handler.
async fn handle_worklog(
    state: &McpState,
    action: &str,
    args: &Map<String, Value>,
) -> Result<CallToolResult, McpError> {
    if let Err(e) = require_auth(state).await {
        return Ok(e);
    }
    let client = state.client().read().await;
    let entity_type = optional_str(args, "entity_type").unwrap_or("workspace");
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
        _ => Ok(error_text(&format!("Unknown worklog action: {action}"))),
    }
}

/// Approval tool handler.
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
            let ws_id = match required_str(args, "workspace_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::create_approval(
                &client,
                entity_type,
                entity_id,
                description,
                ws_id,
                optional_str(args, "approver_id"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "approve" => {
            let approval_id = match required_str(args, "approval_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::resolve_approval(
                &client,
                approval_id,
                "approve",
                optional_str(args, "comment"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        "reject" => {
            let approval_id = match required_str(args, "approval_id") {
                Ok(v) => v,
                Err(e) => return Ok(e),
            };
            match api::workflow::resolve_approval(
                &client,
                approval_id,
                "reject",
                optional_str(args, "comment"),
            )
            .await
            {
                Ok(v) => Ok(success_json(&v)),
                Err(e) => Ok(cli_err_to_result(&e)),
            }
        }
        _ => Ok(error_text(&format!("Unknown approval action: {action}"))),
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
        _ => Ok(error_text(&format!("Unknown lock action: {action}"))),
    }
}
