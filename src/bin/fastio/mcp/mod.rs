//! MCP (Model Context Protocol) server mode for the Fast.io CLI.
//!
//! Exposes the CLI's existing API layer as MCP tools over stdio transport,
//! allowing AI agents to interact with Fast.io as a local MCP server.

/// Built-in MCP prompt definitions.
pub mod prompts;
/// MCP resource listings and readers.
pub mod resources;
/// MCP tool definitions and dispatch router.
pub mod tools;

use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult, Implementation,
    InitializeResult, ListPromptsResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use tokio::sync::RwLock;

use fastio_cli::auth::token::resolve_token;
use fastio_cli::client::ApiClient;
use fastio_cli::config::{Config, DEFAULT_API_BASE};

use self::tools::ToolRouter;

/// Shared state accessible by all MCP tool handlers.
pub struct McpState {
    /// HTTP client for the Fast.io API.
    client: RwLock<ApiClient>,
    /// API base URL.
    api_base: String,
    /// Whether the user is authenticated.
    authenticated: RwLock<bool>,
}

impl McpState {
    /// Check whether the session is currently authenticated.
    pub async fn is_authenticated(&self) -> bool {
        *self.authenticated.read().await
    }

    /// Get a reference to the API client.
    pub fn client(&self) -> &RwLock<ApiClient> {
        &self.client
    }

    /// Get the API base URL.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Update the in-memory token for this session.
    pub async fn set_token(&self, token: String) {
        self.client.write().await.set_token(token);
        *self.authenticated.write().await = true;
    }

    /// Clear the in-memory token for this session, de-authenticating it.
    ///
    /// Used by the `signout` action to actually drop the live MCP credential —
    /// it does NOT revoke the server-side session or API key (those have their
    /// own revocation paths: `fastio auth signout` and `api-key-delete`).
    pub async fn clear_token(&self) {
        self.client.write().await.clear_token();
        *self.authenticated.write().await = false;
    }

    /// Construct an unauthenticated state for unit tests.
    ///
    /// Used by tool-router tests to exercise dispatch (e.g. the hidden `ai`
    /// alias routing to the ripley handler) without a live session — calls
    /// short-circuit at `require_auth`, which is enough to prove the alias
    /// reached the correct handler rather than the unknown-tool arm.
    #[cfg(test)]
    pub fn new_unauthenticated_for_test(api_base: &str) -> Self {
        let client = ApiClient::new(api_base, None).expect("test client should construct");
        McpState {
            client: RwLock::new(client),
            api_base: api_base.to_owned(),
            authenticated: RwLock::new(false),
        }
    }
}

/// The MCP server handler that routes tool calls, resources, and prompts.
#[derive(Clone)]
pub struct FastioMcpServer {
    /// Shared application state.
    state: Arc<McpState>,
    /// Tool router for dispatching tool calls.
    tool_router: ToolRouter,
}

impl FastioMcpServer {
    /// Create a new MCP server, resolving credentials from the standard chain.
    ///
    /// `tools_filter` is the validated `--tools` allow-list (`None` = all tools),
    /// threaded into the [`ToolRouter`] so `list_tools` / `call_tool` expose only
    /// the allowed set.
    fn new(
        api_base: &str,
        token_override: Option<&str>,
        profile_name: &str,
        config_dir: &std::path::Path,
        tools_filter: Option<std::collections::HashSet<String>>,
    ) -> Result<Self> {
        let token = resolve_token(token_override, profile_name, config_dir)
            .ok()
            .flatten();
        let authenticated = token.is_some();
        let client = ApiClient::new(api_base, token).context("failed to create API client")?;

        let state = Arc::new(McpState {
            client: RwLock::new(client),
            api_base: api_base.to_owned(),
            authenticated: RwLock::new(authenticated),
        });

        Ok(Self {
            state: Arc::clone(&state),
            tool_router: ToolRouter::new(state, tools_filter),
        })
    }

    /// Whether any tool is visible after applying BOTH the E-Sign rule and the
    /// `--tools` allow-list. Delegates to the router; used by `serve` to refuse a
    /// server that would start with an empty effective tool surface.
    fn has_visible_tools(&self) -> bool {
        self.tool_router.has_visible_tools()
    }

    /// Build the server intro `instructions` text.
    ///
    /// The e-signature (`sign`) mention is CONDITIONAL on the E-Sign kill-switch
    /// (read once at router construction). When E-Sign is disabled the `sign`
    /// tool is filtered from `list_tools`, so the intro must not advertise it.
    ///
    /// When a `--tools` allow-list is active the intro enumerates ONLY the
    /// visible tools instead of pitching the full default surface, so a client
    /// that trusts `instructions` never sees a tool it cannot call
    /// (advertised-vs-callable drift). The Ripley-offload nudge and the `sign`
    /// blurb are included only when those tools are actually visible.
    fn instructions_text(&self) -> String {
        // Compute the visible surface ONCE — router state is immutable after
        // construction, so a single snapshot drives the intro pitch, the Ripley
        // nudge, and the `sign` blurb consistently.
        let visible = self.tool_router.visible_tool_names();
        let mut text = if self.tool_router.has_tools_filter() {
            // Filtered server: honestly list only what is callable.
            format!(
                "Fast.io MCP server (via the Fast.io REST API), started with a \
                 restricted tool set. Available tools: {}. ",
                visible.join(", ")
            )
        } else {
            String::from(
                "Fast.io MCP server -- files, workspaces, shares, uploads, downloads, \
                 and AI agent (Ripley) via the Fast.io REST API. ",
            )
        };
        // The Ripley-offload nudge only makes sense when `ripley` is visible.
        if visible.contains(&"ripley") {
            text.push_str(
                "OFFLOAD multi-step work: prefer asking the `ripley` tool \
                 (Fast.io's delegated AI agent, acting on your behalf) to find or \
                 do a task over hand-driving many primitives. ",
            );
        }
        // The `sign` blurb requires `sign` to be visible, which already implies
        // E-Sign is enabled (`tool_visible` hides `sign` when it is off).
        if visible.contains(&"sign") {
            text.push_str(
                "The `sign` tool exposes READ + DRAFT-DRIVE actions only -- \
                 destructive / terminal mutations (`send`/`void` -- envelopes are \
                 voided, not deleted) are CLI-binary-only. ",
            );
        }
        text.push_str(
            "Tool results are rendered as GitHub-flavored Markdown (shape-compatible \
             with the server-side `?output=markdown` contract) for compact, \
             high-signal consumption. Run `fastio auth login` in a terminal first \
             to authenticate.",
        );
        text
    }
}

impl ServerHandler for FastioMcpServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_prompts()
                .enable_resources()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "fastio-cli".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                ..Implementation::default()
            },
            instructions: Some(self.instructions_text()),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(resources::list_resources(&self.state).await)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        resources::read_resource(&self.state, &request.uri).await
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(prompts::list_prompts())
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        prompts::get_prompt(&request.name, request.arguments)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(self.tool_router.list_tools())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.tool_router
            .call_tool(&request.name, request.arguments.unwrap_or_default())
            .await
    }
}

/// Outcome of normalizing a raw `--tools` allow-list against the known tool set.
///
/// `known` is the deduped set of requested names that match a registered tool
/// (the allow-list threaded into the router); `unknown` is the requested names
/// that match nothing (warned about at startup, in first-seen order).
struct NormalizedToolsFilter {
    known: std::collections::HashSet<String>,
    unknown: Vec<String>,
}

/// Normalize a raw `--tools` list into a validated allow-list.
///
/// Trims each entry, drops empties, maps hidden aliases to their canonical name
/// (`ai` → `ripley`, `how-to` → `howto`, via [`tools::canonical_tool_name`]) so
/// either spelling is accepted, dedupes on the canonical name (case-sensitive
/// exact match against the registered tool names), and partitions the requested
/// names into those that match a known tool and those that do not. Pure and
/// env-free so the validation is unit-testable; the caller (`serve`) decides what
/// to do with the two buckets (warn on `unknown`, fail fast if `known` is empty).
/// `unknown` carries the name as the user typed it, so the warning echoes their
/// input.
fn normalize_tools_filter(raw: &[String]) -> NormalizedToolsFilter {
    let known_names: std::collections::HashSet<&'static str> = tools::known_tool_names().collect();
    let mut known = std::collections::HashSet::new();
    let mut unknown = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in raw {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let canonical = tools::canonical_tool_name(trimmed);
        // Dedupe on the CANONICAL name so `ai` and `ripley` (or a repeat) collapse.
        if !seen.insert(canonical.to_owned()) {
            continue;
        }
        if known_names.contains(canonical) {
            known.insert(canonical.to_owned());
        } else {
            // Echo the name the user typed (not the canonical) in the warning.
            unknown.push(trimmed.to_owned());
        }
    }
    NormalizedToolsFilter { known, unknown }
}

/// Start the MCP server over stdio transport.
///
/// This is the main entry point called from `main.rs` when `fastio mcp` runs.
///
/// `tools_filter` is the raw `--tools` value (comma-split by the caller). When
/// present it is normalized into an allow-list: unknown names are warned about
/// on STDERR (stdout stays clean for JSON-RPC), and if NO requested name matches
/// a registered tool the server fails fast rather than starting with an empty
/// surface.
pub async fn serve(
    tools_filter: Option<Vec<String>>,
    api_base_override: Option<&str>,
    token_override: Option<&str>,
    profile_override: Option<&str>,
) -> Result<()> {
    // Validate the `--tools` allow-list BEFORE stdio starts. Warnings go to
    // STDERR (stdout is the JSON-RPC channel); an all-unknown filter is a
    // fail-fast error so the operator learns immediately instead of getting an
    // empty tool surface.
    let allow_list = match &tools_filter {
        None => None,
        Some(raw) => {
            let normalized = normalize_tools_filter(raw);
            for name in &normalized.unknown {
                eprintln!("warning: --tools: '{name}' is not a known tool; ignoring it.");
            }
            if normalized.known.is_empty() {
                let mut known: Vec<&str> = tools::known_tool_names().collect();
                known.sort_unstable();
                anyhow::bail!(
                    "--tools matched no known tools; known: {}",
                    known.join(", ")
                );
            }
            Some(normalized.known)
        }
    };

    let config = Config::load().unwrap_or_default();
    // Honor the global --profile / --api-base / --token overrides, mirroring the
    // non-MCP path in main.rs. Without this, `fastio mcp` resolves its backend
    // and token from stored config only and silently ignores those flags.
    let profile_name = profile_override.unwrap_or(&config.default_profile);
    let api_base = config.api_base(api_base_override, Some(profile_name));
    // Substitute the production default ONLY when nothing was explicitly
    // requested and the resolved base is somehow empty. An explicit
    // `--api-base ""` (e.g. an unset env var in a wrapper) must NOT silently
    // fall back to production — pass it through so the client fails loudly,
    // matching the non-MCP path.
    let api_base_str = if api_base.is_empty() && api_base_override.is_none() {
        DEFAULT_API_BASE.to_owned()
    } else {
        api_base
    };

    let server = FastioMcpServer::new(
        &api_base_str,
        token_override,
        profile_name,
        &config.config_dir,
        allow_list,
    )?;

    // Refuse to start with an empty EFFECTIVE tool surface — BEFORE stdio. This
    // catches the case name-validation alone cannot: `--tools sign` with E-Sign
    // disabled passes the all-unknown check (`sign` is a known name) yet the
    // E-Sign rule hides it, leaving zero visible tools. Failing here (rather than
    // starting a live server that advertises nothing) mirrors the all-unknown
    // fail-fast and gives the operator an actionable hint.
    if tools_filter.is_some() && !server.has_visible_tools() {
        anyhow::bail!(
            "--tools left no tools visible on this server. If you requested only \
             `sign`, set FASTIO_ENABLE_ESIGN=1 (and enable signing for your org); \
             otherwise include at least one enabled tool."
        );
    }

    let service = server
        .serve(stdio())
        .await
        .context("failed to start MCP server on stdio")?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A `--token` override authenticates the MCP server with no credentials
    /// file, and the `api_base` passed through reaches the state — i.e. the
    /// global overrides are honored. Regression guard for the bug where
    /// `fastio mcp` ignored --api-base / --token / --profile (it resolved the
    /// backend and token from stored config only).
    #[tokio::test]
    async fn new_honors_token_override_and_api_base() {
        let server = FastioMcpServer::new(
            "http://example.test/api/current",
            Some("override-token"),
            "default",
            Path::new("/nonexistent-config-dir-for-test"),
            None,
        )
        .expect("server constructs with a token override");
        assert_eq!(server.state.api_base(), "http://example.test/api/current");
        // Assert the RESOLVED token is the override itself — not merely that the
        // server is authenticated. This proves the `--token` precedence path and
        // is NOT satisfied by an ambient FASTIO_TOKEN (which would pass a bare
        // `authenticated == true` check even on the pre-fix env-first code).
        assert_eq!(
            server.state.client().read().await.get_token(),
            Some("override-token")
        );
        assert!(*server.state.authenticated.read().await);
    }

    /// `clear_token` genuinely de-authenticates the live MCP session — it drops
    /// the in-memory bearer token AND flips `authenticated` to false. Regression
    /// guard for the `signout` bug where the in-memory token was never cleared,
    /// so the session stayed authenticated after "signing out".
    #[tokio::test]
    async fn clear_token_deauthenticates_session() {
        let state = McpState::new_unauthenticated_for_test("http://example.test/api/current");
        state.set_token("live-token".to_owned()).await;
        assert!(state.is_authenticated().await);
        assert_eq!(state.client().read().await.get_token(), Some("live-token"));

        state.clear_token().await;
        assert!(
            !state.is_authenticated().await,
            "session must be de-authenticated after clear_token"
        );
        assert_eq!(
            state.client().read().await.get_token(),
            None,
            "in-memory token must be dropped after clear_token"
        );
    }

    /// `normalize_tools_filter` trims, drops empties, dedupes, and partitions
    /// requested names into known (registered) vs unknown (warned + ignored).
    #[test]
    fn normalize_tools_filter_partitions_known_and_unknown() {
        let raw = vec![
            "  files  ".to_owned(),  // trimmed → known
            "org".to_owned(),        // known
            "files".to_owned(),      // dup of the trimmed entry → dropped
            String::new(),           // empty → dropped
            "  ".to_owned(),         // whitespace-only → dropped
            "nosuchtool".to_owned(), // unknown
        ];
        let out = super::normalize_tools_filter(&raw);
        assert!(out.known.contains("files"), "trimmed known name kept");
        assert!(out.known.contains("org"), "known name kept");
        assert_eq!(
            out.known.len(),
            2,
            "dedup + empties dropped: {:?}",
            out.known
        );
        assert_eq!(
            out.unknown,
            vec!["nosuchtool".to_owned()],
            "unknown name surfaced for a startup warning"
        );
    }

    /// A filter of only unknown names yields an empty known set — the signal
    /// `serve` uses to fail fast rather than start with no tools.
    #[test]
    fn normalize_tools_filter_all_unknown_is_empty() {
        let out = super::normalize_tools_filter(&["nope".to_owned(), "zzz".to_owned()]);
        assert!(
            out.known.is_empty(),
            "no known tools when every name is unknown"
        );
        assert_eq!(out.unknown.len(), 2, "both unknown names surfaced");
    }

    /// `sign` is a KNOWN name even though it is gated by the E-Sign kill-switch:
    /// `--tools sign` must not be warned about as unknown (the runtime E-Sign
    /// gate hides/refuses it separately).
    #[test]
    fn normalize_tools_filter_sign_is_known() {
        let out = super::normalize_tools_filter(&["sign".to_owned()]);
        assert!(out.known.contains("sign"), "sign is a registered tool name");
        assert!(out.unknown.is_empty(), "sign must not be flagged unknown");
    }

    /// Hidden aliases in the raw `--tools` list are canonicalized: `ai` →
    /// `ripley`, `how-to` → `howto`, and an alias + its canonical name collapse
    /// to one known entry (deduped on the canonical name).
    #[test]
    fn normalize_tools_filter_canonicalizes_aliases() {
        let out = super::normalize_tools_filter(&[
            "ai".to_owned(),     // → ripley
            "how-to".to_owned(), // → howto
            "ripley".to_owned(), // dup of the `ai` alias → dropped
        ]);
        assert!(out.known.contains("ripley"), "ai canonicalized to ripley");
        assert!(out.known.contains("howto"), "how-to canonicalized to howto");
        assert_eq!(
            out.known.len(),
            2,
            "alias + canonical collapse to one entry: {:?}",
            out.known
        );
        assert!(
            out.unknown.is_empty(),
            "known aliases are not flagged unknown"
        );
    }

    /// The server intro `instructions` enumerate ONLY the visible tools when a
    /// `--tools` filter is active, so a client trusting the intro never sees a
    /// tool it cannot call.
    #[test]
    fn instructions_text_reflects_tools_filter() {
        let state = Arc::new(McpState::new_unauthenticated_for_test(
            "http://example.test/api/current",
        ));
        let filter: std::collections::HashSet<String> =
            ["org", "id"].iter().map(|s| (*s).to_owned()).collect();
        let server = FastioMcpServer {
            state: Arc::clone(&state),
            tool_router: tools::ToolRouter::new_with_filter(state, false, Some(filter)),
        };
        let text = server.instructions_text();
        assert!(
            text.contains("restricted tool set"),
            "filtered intro must state it is restricted: {text}"
        );
        assert!(
            text.contains("org") && text.contains("id"),
            "filtered intro must enumerate the allowed tools: {text}"
        );
        // A tool NOT in the filter must not be pitched — `ripley` is excluded, so
        // the Ripley-offload nudge is absent.
        assert!(
            !text.contains("OFFLOAD multi-step work"),
            "filtered intro must not pitch an excluded tool (ripley): {text}"
        );
    }

    /// The unfiltered intro keeps the full default pitch (Ripley nudge present).
    #[test]
    fn instructions_text_unfiltered_is_full_pitch() {
        let state = Arc::new(McpState::new_unauthenticated_for_test(
            "http://example.test/api/current",
        ));
        let server = FastioMcpServer {
            state: Arc::clone(&state),
            tool_router: tools::ToolRouter::new_with_filter(state, false, None),
        };
        let text = server.instructions_text();
        assert!(
            !text.contains("restricted tool set"),
            "unfiltered intro must not claim restriction: {text}"
        );
        assert!(
            text.contains("OFFLOAD multi-step work"),
            "unfiltered intro pitches the Ripley offload: {text}"
        );
    }
}
