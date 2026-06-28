/// Workspace command implementations for `fastio workspace *`.
///
/// Handles workspace listing, creation, details, update, deletion,
/// workflow management, search, and limits.
use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::json;

use super::CommandContext;
use fastio_cli::api;

/// Workspace subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum WorkspaceCommand {
    /// List all workspaces.
    List {
        /// Filter by organization ID.
        org_id: Option<String>,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Create a workspace.
    Create {
        /// Organization ID.
        org_id: String,
        /// Workspace display name.
        name: String,
        /// URL-safe folder name.
        folder_name: Option<String>,
        /// Description.
        description: Option<String>,
        /// Enable AI intelligence.
        intelligence: Option<bool>,
    },
    /// Get workspace details.
    Info {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Update workspace settings.
    Update {
        /// Workspace ID.
        workspace_id: String,
        /// New name.
        name: Option<String>,
        /// New description.
        description: Option<String>,
        /// New folder name.
        folder_name: Option<String>,
        /// Toggle AI indexing (intelligence).
        intelligence: Option<bool>,
        /// Who can self-join the workspace (permission phrase).
        perm_join: Option<String>,
        /// Who can manage members (permission phrase).
        perm_member_manage: Option<String>,
        /// AI obligation-summary enrichment toggle.
        nl_summaries_enabled: Option<bool>,
        /// AI enrichment daily cap (0-100000).
        nl_summaries_daily_cap: Option<u32>,
        /// Native workflow-review rollout tier (disabled, mvs, extended).
        workflow_approval_native_enabled: Option<String>,
        /// Brand accent color (JSON-encoded string).
        accent_color: Option<String>,
        /// Primary background color (JSON-encoded string).
        background_color1: Option<String>,
        /// Secondary background color (JSON-encoded string).
        background_color2: Option<String>,
        /// Custom owner-defined properties (JSON-encoded string).
        owner_defined: Option<String>,
    },
    /// Delete a workspace.
    Delete {
        /// Workspace ID.
        workspace_id: String,
        /// Confirmation string.
        confirm: String,
    },
    /// Enable workflow features.
    EnableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Disable workflow features.
    DisableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// List active background jobs (poll after async metadata extract).
    JobsStatus {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Search workspace content.
    Search {
        /// Workspace ID.
        workspace_id: String,
        /// Search query.
        query: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Get workspace limits.
    Limits {
        /// Workspace ID.
        workspace_id: String,
    },
}

/// Execute a workspace subcommand.
pub async fn execute(command: &WorkspaceCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        WorkspaceCommand::List {
            org_id,
            limit,
            offset,
        } => list(ctx, org_id.as_deref(), *limit, *offset).await,
        WorkspaceCommand::Create {
            org_id,
            name,
            folder_name,
            description,
            intelligence,
        } => {
            create(
                ctx,
                org_id,
                name,
                folder_name.as_deref(),
                description.as_deref(),
                *intelligence,
            )
            .await
        }
        WorkspaceCommand::Info { workspace_id } => info(ctx, workspace_id).await,
        WorkspaceCommand::Update {
            workspace_id,
            name,
            description,
            folder_name,
            intelligence,
            perm_join,
            perm_member_manage,
            nl_summaries_enabled,
            nl_summaries_daily_cap,
            workflow_approval_native_enabled,
            accent_color,
            background_color1,
            background_color2,
            owner_defined,
        } => {
            update(
                ctx,
                workspace_id,
                &WorkspaceUpdate {
                    name: name.as_deref(),
                    description: description.as_deref(),
                    folder_name: folder_name.as_deref(),
                    intelligence: *intelligence,
                    perm_join: perm_join.as_deref(),
                    perm_member_manage: perm_member_manage.as_deref(),
                    nl_summaries_enabled: *nl_summaries_enabled,
                    nl_summaries_daily_cap: *nl_summaries_daily_cap,
                    workflow_approval_native_enabled: workflow_approval_native_enabled.as_deref(),
                    accent_color: accent_color.as_deref(),
                    background_color1: background_color1.as_deref(),
                    background_color2: background_color2.as_deref(),
                    owner_defined: owner_defined.as_deref(),
                },
            )
            .await
        }
        WorkspaceCommand::Delete {
            workspace_id,
            confirm,
        } => delete(ctx, workspace_id, confirm).await,
        WorkspaceCommand::EnableWorkflow { workspace_id } => {
            enable_workflow(ctx, workspace_id).await
        }
        WorkspaceCommand::DisableWorkflow { workspace_id } => {
            disable_workflow(ctx, workspace_id).await
        }
        WorkspaceCommand::JobsStatus { workspace_id } => jobs_status(ctx, workspace_id).await,
        WorkspaceCommand::Search {
            workspace_id,
            query,
            limit,
            offset,
        } => search(ctx, workspace_id, query, *limit, *offset).await,
        WorkspaceCommand::Limits { workspace_id } => limits(ctx, workspace_id).await,
    }
}

/// List workspaces.
async fn list(
    ctx: &CommandContext<'_>,
    org_id: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::list_workspaces(&client, org_id, limit, offset)
        .await
        .context("failed to list workspaces")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a workspace.
async fn create(
    ctx: &CommandContext<'_>,
    org_id: &str,
    name: &str,
    folder_name: Option<&str>,
    description: Option<&str>,
    intelligence: Option<bool>,
) -> Result<()> {
    // Use folder_name if provided, otherwise derive from name
    let effective_folder =
        folder_name.map_or_else(|| name.to_lowercase().replace(' ', "-"), String::from);

    let client = ctx.build_client()?;
    let value = api::workspace::create_workspace(
        &client,
        &api::workspace::CreateWorkspaceParams {
            org_id,
            folder_name: &effective_folder,
            name,
            description,
            intelligence,
        },
    )
    .await
    .context("failed to create workspace")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get workspace details.
async fn info(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::get_workspace(&client, workspace_id)
        .await
        .context("failed to get workspace details")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Optional workspace-update fields gathered from the CLI flags.
#[derive(Default)]
struct WorkspaceUpdate<'a> {
    /// New display name.
    name: Option<&'a str>,
    /// New description.
    description: Option<&'a str>,
    /// New URL-safe folder name.
    folder_name: Option<&'a str>,
    /// AI-indexing (intelligence) toggle.
    intelligence: Option<bool>,
    /// Who can self-join the workspace (permission phrase).
    perm_join: Option<&'a str>,
    /// Who can manage members (permission phrase).
    perm_member_manage: Option<&'a str>,
    /// AI obligation-summary enrichment toggle.
    nl_summaries_enabled: Option<bool>,
    /// AI enrichment daily cap (0-100000).
    nl_summaries_daily_cap: Option<u32>,
    /// Native workflow-review rollout tier (disabled, mvs, extended).
    workflow_approval_native_enabled: Option<&'a str>,
    /// Brand accent color (JSON-encoded string).
    accent_color: Option<&'a str>,
    /// Primary background color (JSON-encoded string).
    background_color1: Option<&'a str>,
    /// Secondary background color (JSON-encoded string).
    background_color2: Option<&'a str>,
    /// Custom owner-defined properties (JSON-encoded string).
    owner_defined: Option<&'a str>,
}

impl WorkspaceUpdate<'_> {
    /// Whether any field is set (the server rejects an empty update).
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.folder_name.is_none()
            && self.intelligence.is_none()
            && self.perm_join.is_none()
            && self.perm_member_manage.is_none()
            && self.nl_summaries_enabled.is_none()
            && self.nl_summaries_daily_cap.is_none()
            && self.workflow_approval_native_enabled.is_none()
            && self.accent_color.is_none()
            && self.background_color1.is_none()
            && self.background_color2.is_none()
            && self.owner_defined.is_none()
    }
}

/// Build the form-field map for a workspace update from the provided options.
///
/// Bool toggles (`intelligence`, `nl_summaries_enabled`) are serialized as the
/// string `"true"`/`"false"` because the `/workspace/{id}/update/` endpoint
/// takes them as string form fields (workspaces.txt). The brand-color and
/// `owner_defined` fields are JSON-encoded strings forwarded verbatim.
fn build_workspace_update_fields(u: &WorkspaceUpdate<'_>) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    if let Some(v) = u.name {
        fields.insert("name".to_owned(), v.to_owned());
    }
    if let Some(v) = u.description {
        fields.insert("description".to_owned(), v.to_owned());
    }
    if let Some(v) = u.folder_name {
        fields.insert("folder_name".to_owned(), v.to_owned());
    }
    if let Some(v) = u.intelligence {
        fields.insert("intelligence".to_owned(), v.to_string());
    }
    if let Some(v) = u.perm_join {
        fields.insert("perm_join".to_owned(), v.to_owned());
    }
    if let Some(v) = u.perm_member_manage {
        fields.insert("perm_member_manage".to_owned(), v.to_owned());
    }
    if let Some(v) = u.nl_summaries_enabled {
        fields.insert("nl_summaries_enabled".to_owned(), v.to_string());
    }
    if let Some(v) = u.nl_summaries_daily_cap {
        fields.insert("nl_summaries_daily_cap".to_owned(), v.to_string());
    }
    if let Some(v) = u.workflow_approval_native_enabled {
        fields.insert("workflow_approval_native_enabled".to_owned(), v.to_owned());
    }
    if let Some(v) = u.accent_color {
        fields.insert("accent_color".to_owned(), v.to_owned());
    }
    if let Some(v) = u.background_color1 {
        fields.insert("background_color1".to_owned(), v.to_owned());
    }
    if let Some(v) = u.background_color2 {
        fields.insert("background_color2".to_owned(), v.to_owned());
    }
    if let Some(v) = u.owner_defined {
        fields.insert("owner_defined".to_owned(), v.to_owned());
    }
    fields
}

/// Update workspace settings.
async fn update(
    ctx: &CommandContext<'_>,
    workspace_id: &str,
    u: &WorkspaceUpdate<'_>,
) -> Result<()> {
    anyhow::ensure!(
        !u.is_empty(),
        "at least one update field is required (e.g. --name, --description, --perm-join, --nl-summaries-enabled, --accent-color, …)"
    );

    let fields = build_workspace_update_fields(u);

    let client = ctx.build_client()?;
    let value = api::workspace::update_workspace(&client, workspace_id, &fields)
        .await
        .context("failed to update workspace")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Delete a workspace.
async fn delete(ctx: &CommandContext<'_>, workspace_id: &str, confirm: &str) -> Result<()> {
    let client = ctx.build_client()?;
    api::workspace::delete_workspace(&client, workspace_id, confirm)
        .await
        .context("failed to delete workspace")?;

    let value = json!({
        "status": "deleted",
        "workspace_id": workspace_id,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Enable workflow features.
async fn enable_workflow(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::enable_workflow(&client, workspace_id)
        .await
        .context("failed to enable workflow features")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Disable workflow features.
async fn disable_workflow(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::disable_workflow(&client, workspace_id)
        .await
        .context("failed to disable workflow features")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List active background jobs (poll target after async metadata extract).
async fn jobs_status(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::jobs_status(&client, workspace_id)
        .await
        .context("failed to fetch workspace jobs status")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Search workspace content.
///
/// Re-pointed (Phase 3) onto the unified grouped-bucket search
/// (`api::search::unified_search_workspace`, `/search/`), sharing its
/// implementation with `fastio search workspace`. `limit`/`offset`, when
/// supplied, page the `files` bucket (the primary bucket for this legacy
/// entry point); the response renders as grouped buckets.
async fn search(
    ctx: &CommandContext<'_>,
    workspace_id: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let params = api::search::UnifiedSearchParams::new().files(offset, limit);
    let value = api::search::unified_search_workspace(&client, workspace_id, query, params)
        .await
        .context("failed to search workspace")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Get workspace limits.
async fn limits(ctx: &CommandContext<'_>, workspace_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::workspace::get_workspace_limits(&client, workspace_id)
        .await
        .context("failed to get workspace limits")?;
    ctx.output.render(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{WorkspaceUpdate, build_workspace_update_fields};

    #[test]
    fn update_fields_carry_intelligence_true() {
        // `workspace update --intelligence true` must reach the form body as
        // the string "true" (workspaces.txt: intelligence is a string toggle).
        let fields = build_workspace_update_fields(&WorkspaceUpdate {
            intelligence: Some(true),
            ..WorkspaceUpdate::default()
        });
        assert_eq!(fields.get("intelligence").map(String::as_str), Some("true"));
        assert_eq!(fields.len(), 1);
    }

    #[test]
    fn update_fields_carry_intelligence_false() {
        let fields = build_workspace_update_fields(&WorkspaceUpdate {
            intelligence: Some(false),
            ..WorkspaceUpdate::default()
        });
        assert_eq!(
            fields.get("intelligence").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn update_fields_omit_intelligence_when_unset() {
        // When --intelligence is not passed the toggle must NOT be sent, so an
        // unrelated rename never accidentally flips AI indexing.
        let fields = build_workspace_update_fields(&WorkspaceUpdate {
            name: Some("New Name"),
            ..WorkspaceUpdate::default()
        });
        assert!(!fields.contains_key("intelligence"));
        assert_eq!(fields.get("name").map(String::as_str), Some("New Name"));
    }

    #[test]
    fn update_fields_carry_new_governance_and_branding_keys() {
        let fields = build_workspace_update_fields(&WorkspaceUpdate {
            perm_join: Some("Member or above"),
            perm_member_manage: Some("Admin or above"),
            nl_summaries_enabled: Some(false),
            nl_summaries_daily_cap: Some(250),
            workflow_approval_native_enabled: Some("mvs"),
            accent_color: Some(r#"{"r":1}"#),
            background_color1: Some(r#"{"g":2}"#),
            background_color2: Some(r#"{"b":3}"#),
            owner_defined: Some(r#"{"k":"v"}"#),
            ..WorkspaceUpdate::default()
        });
        assert_eq!(
            fields.get("perm_join").map(String::as_str),
            Some("Member or above")
        );
        assert_eq!(
            fields.get("perm_member_manage").map(String::as_str),
            Some("Admin or above")
        );
        // Bool → string.
        assert_eq!(
            fields.get("nl_summaries_enabled").map(String::as_str),
            Some("false")
        );
        // Integer → string.
        assert_eq!(
            fields.get("nl_summaries_daily_cap").map(String::as_str),
            Some("250")
        );
        assert_eq!(
            fields
                .get("workflow_approval_native_enabled")
                .map(String::as_str),
            Some("mvs")
        );
        assert_eq!(
            fields.get("accent_color").map(String::as_str),
            Some(r#"{"r":1}"#)
        );
        assert_eq!(
            fields.get("background_color1").map(String::as_str),
            Some(r#"{"g":2}"#)
        );
        assert_eq!(
            fields.get("background_color2").map(String::as_str),
            Some(r#"{"b":3}"#)
        );
        assert_eq!(
            fields.get("owner_defined").map(String::as_str),
            Some(r#"{"k":"v"}"#)
        );
    }

    #[test]
    fn update_fields_empty_when_nothing_set() {
        assert!(WorkspaceUpdate::default().is_empty());
        assert!(build_workspace_update_fields(&WorkspaceUpdate::default()).is_empty());
    }
}
