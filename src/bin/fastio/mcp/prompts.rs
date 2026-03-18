/// MCP prompt definitions for the Fast.io CLI MCP server.
///
/// Provides a "get-started" prompt to guide new users.
use rmcp::ErrorData as McpError;
use rmcp::model::{GetPromptResult, ListPromptsResult, Prompt, PromptMessage, PromptMessageRole};
use serde_json::{Map, Value};

/// List available MCP prompts.
pub fn list_prompts() -> ListPromptsResult {
    ListPromptsResult {
        prompts: vec![Prompt::new(
            "get-started",
            Some("Guide for first-time Fast.io MCP server setup"),
            Some(vec![]),
        )],
        next_cursor: None,
        meta: None,
    }
}

/// Get a specific prompt by name.
pub fn get_prompt(
    name: &str,
    _arguments: Option<Map<String, Value>>,
) -> Result<GetPromptResult, McpError> {
    match name {
        "get-started" => Ok(get_started_prompt()),
        _ => Err(McpError::invalid_params(
            "Unknown prompt",
            Some(serde_json::json!({ "name": name })),
        )),
    }
}

/// Build the get-started prompt content.
fn get_started_prompt() -> GetPromptResult {
    let messages = vec![
        PromptMessage::new_text(
            PromptMessageRole::Assistant,
            "I'll help you get started with Fast.io through the CLI's MCP server.",
        ),
        PromptMessage::new_text(PromptMessageRole::User, GET_STARTED_TEXT),
    ];

    GetPromptResult {
        description: Some("Guide for getting started with the Fast.io MCP server".to_owned()),
        messages,
    }
}

/// Static text for the get-started prompt.
const GET_STARTED_TEXT: &str = "\
# Getting Started with Fast.io MCP Server

## Step 1: Authentication
First, check if you're authenticated by reading the `session://status` resource.

If not authenticated, either:
- Run `fastio auth login` in a terminal (recommended for browser-based login)
- Use the `auth` tool with `action: \"signin\"` and provide email/password

## Step 2: Explore Your Organizations
Use the `org` tool with `action: \"list\"` to see your organizations.

## Step 3: List Workspaces
Use the `workspace` tool with `action: \"list\"` and provide your org_id.

## Step 4: Browse Files
Use the `files` tool with `action: \"list\"` and provide your workspace_id.

## Available Tool Domains
- auth, user, org, workspace, files, upload, download, share
- ai, member, comment, event, invitation, preview, asset
- task, worklog, approval, todo

Each tool uses an `action` parameter to select the operation.";
