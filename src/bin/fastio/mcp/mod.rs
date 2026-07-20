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
    fn new(
        api_base: &str,
        token_override: Option<&str>,
        profile_name: &str,
        config_dir: &std::path::Path,
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
            tool_router: ToolRouter::new(state),
        })
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
            instructions: Some(
                "Fast.io MCP server -- files, workspaces, shares, uploads, downloads, \
                 AI agent (Ripley), Workflow Orchestration, and e-signature via the \
                 Fast.io REST API. OFFLOAD multi-step work: prefer asking the `ripley` \
                 tool (Fast.io's delegated AI agent, acting on your behalf) to find or \
                 do a task over hand-driving many primitives, and prefer the `workflow` \
                 tool's compound `*-and-wait` actions over tight detail-poll loops. \
                 `workflow` and `sign` expose READ + DRIVE actions only -- destructive / \
                 terminal mutations (workflow `cancel`; sign `send`/`void` -- envelopes are \
                 voided, not deleted) are CLI-binary-only. The `task` tool is the Tasks API \
                 (task lists, tasks, comments, attachments); `workflow` is the separate \
                 durable orchestration surface. Tool results are rendered as \
                 GitHub-flavored Markdown (shape-compatible with the server-side \
                 `?output=markdown` contract) for compact, high-signal consumption. Run \
                 `fastio auth login` in a terminal first to authenticate."
                    .to_owned(),
            ),
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

/// Start the MCP server over stdio transport.
///
/// This is the main entry point called from `main.rs` when `fastio mcp` runs.
pub async fn serve(
    tools_filter: Option<Vec<String>>,
    api_base_override: Option<&str>,
    token_override: Option<&str>,
    profile_override: Option<&str>,
) -> Result<()> {
    let _ = tools_filter; // reserved for future --tools filter

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
    )?;
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
}
