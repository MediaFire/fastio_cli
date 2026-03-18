/// MCP resource definitions for the Fast.io CLI MCP server.
///
/// Exposes session status as a readable MCP resource.
use rmcp::ErrorData as McpError;
use rmcp::model::{
    AnnotateAble, ListResourcesResult, RawResource, ReadResourceResult, ResourceContents,
};

use super::McpState;

/// List available MCP resources.
pub async fn list_resources(state: &McpState) -> ListResourcesResult {
    let authenticated = state.is_authenticated().await;
    let description = if authenticated {
        "Current session status (authenticated)"
    } else {
        "Current session status (not authenticated)"
    };
    ListResourcesResult {
        resources: vec![RawResource::new("session://status", description).no_annotation()],
        next_cursor: None,
        meta: None,
    }
}

/// Read a specific MCP resource by URI.
pub async fn read_resource(state: &McpState, uri: &str) -> Result<ReadResourceResult, McpError> {
    match uri {
        "session://status" => {
            let authenticated = state.is_authenticated().await;
            let status = if authenticated {
                serde_json::json!({
                    "authenticated": true,
                    "api_base": state.api_base(),
                    "hint": "Session is active. You can call any tool."
                })
            } else {
                serde_json::json!({
                    "authenticated": false,
                    "api_base": state.api_base(),
                    "hint": "Not authenticated. Run `fastio auth login` in a terminal, or use the auth tool with action=signin."
                })
            };
            Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(
                    serde_json::to_string_pretty(&status).unwrap_or_default(),
                    uri.to_owned(),
                )],
            })
        }
        _ => Err(McpError::resource_not_found(
            "resource_not_found",
            Some(serde_json::json!({ "uri": uri })),
        )),
    }
}
