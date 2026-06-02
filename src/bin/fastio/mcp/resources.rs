/// MCP resource definitions for the Fast.io CLI MCP server.
///
/// Exposes session status and the agent skill guide as readable MCP resources.
use rmcp::ErrorData as McpError;
use rmcp::model::{
    AnnotateAble, ListResourcesResult, RawResource, ReadResourceResult, ResourceContents,
};

use super::McpState;

/// The agent skill guide, embedded at compile time from the repo-root
/// `AGENTS.md`.
///
/// This is the SAME source the `fastio skill` CLI command prints (see
/// `main.rs`), so the `skill://guide` MCP resource and `fastio skill` stay in
/// sync from one file. The path is four levels up from this module
/// (`src/bin/fastio/mcp/`) to the repository root.
const SKILL_GUIDE: &str = include_str!("../../../../AGENTS.md");

/// URI of the agent skill-guide resource.
const SKILL_GUIDE_URI: &str = "skill://guide";

/// List available MCP resources.
pub async fn list_resources(state: &McpState) -> ListResourcesResult {
    let authenticated = state.is_authenticated().await;
    let session_description = if authenticated {
        "Current session status (authenticated)"
    } else {
        "Current session status (not authenticated)"
    };
    let session = RawResource::new("session://status", session_description).no_annotation();

    let mut guide = RawResource::new(SKILL_GUIDE_URI, "Fast.io agent skill guide");
    guide.description =
        Some("Agent guide for the Fast.io CLI (same content as `fastio skill`).".to_owned());
    guide.mime_type = Some("text/markdown".to_owned());
    let guide = guide.no_annotation();

    ListResourcesResult {
        resources: vec![session, guide],
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
                    fastio_cli::output::markdown::to_markdown(&status),
                    uri.to_owned(),
                )],
            })
        }
        SKILL_GUIDE_URI => Ok(ReadResourceResult {
            contents: vec![ResourceContents::text(SKILL_GUIDE, uri.to_owned())],
        }),
        _ => Err(McpError::resource_not_found(
            "resource_not_found",
            Some(serde_json::json!({ "uri": uri })),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `list_resources` must advertise the `skill://guide` resource with a
    /// markdown mime type so agents can discover the guide.
    #[tokio::test]
    async fn list_resources_includes_skill_guide() {
        let state = McpState::new_unauthenticated_for_test("https://api.fast.io/current");
        let listed = list_resources(&state).await;

        let guide = listed
            .resources
            .iter()
            .find(|r| r.uri == SKILL_GUIDE_URI)
            .expect("skill://guide must be listed");
        assert_eq!(guide.mime_type.as_deref(), Some("text/markdown"));
    }

    /// `read_resource("skill://guide")` must return non-empty markdown text
    /// sourced from the root `AGENTS.md`.
    #[tokio::test]
    async fn read_resource_skill_guide_returns_markdown() {
        let state = McpState::new_unauthenticated_for_test("https://api.fast.io/current");
        let result = read_resource(&state, SKILL_GUIDE_URI)
            .await
            .expect("skill://guide must be readable");

        let text = match result.contents.first() {
            Some(ResourceContents::TextResourceContents { text, .. }) => text.clone(),
            _ => panic!("skill://guide must return text contents"),
        };
        assert!(!text.trim().is_empty(), "guide text must not be empty");
        assert!(
            text.contains("Fast.io CLI"),
            "guide text should be the AGENTS.md guide"
        );
    }

    /// An unknown resource URI must surface a not-found error rather than the
    /// guide or session content.
    #[tokio::test]
    async fn read_resource_unknown_uri_is_not_found() {
        let state = McpState::new_unauthenticated_for_test("https://api.fast.io/current");
        let err = read_resource(&state, "skill://does-not-exist")
            .await
            .expect_err("unknown resource must error");
        let _ = err;
    }
}
