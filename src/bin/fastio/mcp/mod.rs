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
    fn new(api_base: &str, config_dir: &std::path::Path) -> Result<Self> {
        let token = resolve_token(None, "default", config_dir).ok().flatten();
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
                "Fast.io MCP server -- manages files, workspaces, shares, uploads, \
                 downloads, AI chat, and workflow primitives via the Fast.io REST API. \
                 Run `fastio auth login` in a terminal first to authenticate."
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
        Ok(ToolRouter::list_tools())
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
pub async fn serve(tools_filter: Option<Vec<String>>) -> Result<()> {
    let _ = tools_filter; // reserved for future --tools filter

    let config = Config::load().unwrap_or_default();
    let api_base = config.api_base(None, Some(&config.default_profile));
    let api_base_str = if api_base.is_empty() {
        DEFAULT_API_BASE.to_owned()
    } else {
        api_base
    };

    let server = FastioMcpServer::new(&api_base_str, &config.config_dir)?;
    let service = server
        .serve(stdio())
        .await
        .context("failed to start MCP server on stdio")?;
    service.waiting().await?;
    Ok(())
}
