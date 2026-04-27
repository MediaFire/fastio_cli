/// Metadata extraction command implementations for `fastio metadata *`.
///
/// Handles listing eligible files, managing template-file mappings,
/// AI-based matching, and metadata extraction.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::CommandContext;
use fastio_cli::api;

/// Metadata subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MetadataCommand {
    /// List files eligible for metadata extraction.
    Eligible {
        /// Workspace ID.
        workspace: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Add files to a metadata template.
    AddNodes {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// JSON-encoded array of node IDs.
        node_ids: String,
    },
    /// Remove files from a metadata template.
    RemoveNodes {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// JSON-encoded array of node IDs.
        node_ids: String,
    },
    /// List files mapped to a metadata template.
    ListNodes {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
        /// Optional template field name to sort by.
        sort_field: Option<String>,
        /// Sort direction (`asc` or `desc`).
        sort_dir: Option<String>,
    },
    /// AI-based file matching for a template.
    AutoMatch {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
    },
    /// Batch extract metadata for all files in a template.
    ExtractAll {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
    },
    /// Get metadata details for one or more files.
    ///
    /// `node_ids.len() == 1` keeps the single-node endpoint shape;
    /// 2+ ids route to the bulk endpoint and return
    /// `{objects: [...], templates: {...}, errors: [...]}`.
    Details {
        /// Workspace ID.
        workspace: String,
        /// One or more storage node IDs.
        node_ids: Vec<String>,
    },
    /// Enqueue an async metadata extraction for a single file.
    Extract {
        /// Workspace ID.
        workspace: String,
        /// Node ID of the file.
        node_id: String,
        /// Template ID to extract against.
        template_id: String,
        /// JSON-encoded array of field names for partial extraction.
        fields: Option<String>,
    },
    /// Preview files that match a proposed template name + description.
    PreviewMatch {
        /// Workspace ID.
        workspace: String,
        /// Proposed template name (1-255 chars).
        name: String,
        /// Natural-language template description.
        description: String,
    },
    /// Request AI-suggested column definitions for a proposed template.
    SuggestFields {
        /// Workspace ID.
        workspace: String,
        /// JSON-encoded array of 1-25 sample node IDs.
        node_ids: String,
        /// Template description.
        description: String,
        /// Optional short user hint (max 64 chars, letters/numbers/spaces).
        user_context: Option<String>,
    },
    /// Create a metadata template (a.k.a. view).
    CreateTemplate {
        /// Workspace ID.
        workspace: String,
        /// Template name.
        name: String,
        /// Template description.
        description: String,
        /// Template category.
        category: String,
        /// JSON-encoded fields array (suggest-fields output is compatible).
        fields: String,
    },
}

/// Execute a metadata subcommand.
pub async fn execute(command: &MetadataCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        MetadataCommand::Eligible {
            workspace,
            limit,
            offset,
        } => eligible(ctx, workspace, *limit, *offset).await,
        MetadataCommand::AddNodes {
            workspace,
            template_id,
            node_ids,
        } => add_nodes(ctx, workspace, template_id, node_ids).await,
        MetadataCommand::RemoveNodes {
            workspace,
            template_id,
            node_ids,
        } => remove_nodes(ctx, workspace, template_id, node_ids).await,
        MetadataCommand::ListNodes {
            workspace,
            template_id,
            limit,
            offset,
            sort_field,
            sort_dir,
        } => {
            list_nodes(
                ctx,
                workspace,
                template_id,
                *limit,
                *offset,
                sort_field.as_deref(),
                sort_dir.as_deref(),
            )
            .await
        }
        MetadataCommand::AutoMatch {
            workspace,
            template_id,
        } => auto_match(ctx, workspace, template_id).await,
        MetadataCommand::ExtractAll {
            workspace,
            template_id,
        } => extract_all(ctx, workspace, template_id).await,
        MetadataCommand::Details {
            workspace,
            node_ids,
        } => details(ctx, workspace, node_ids).await,
        MetadataCommand::Extract {
            workspace,
            node_id,
            template_id,
            fields,
        } => extract(ctx, workspace, node_id, template_id, fields.as_deref()).await,
        MetadataCommand::PreviewMatch {
            workspace,
            name,
            description,
        } => preview_match(ctx, workspace, name, description).await,
        MetadataCommand::SuggestFields {
            workspace,
            node_ids,
            description,
            user_context,
        } => {
            suggest_fields(
                ctx,
                workspace,
                node_ids,
                description,
                user_context.as_deref(),
            )
            .await
        }
        MetadataCommand::CreateTemplate {
            workspace,
            name,
            description,
            category,
            fields,
        } => create_template(ctx, workspace, name, description, category, fields).await,
    }
}

/// Maximum accepted length for a node or workspace identifier.
const MAX_ID_LEN: usize = 128;

/// Runtime cap on positional node IDs per `fastio metadata details`
/// invocation. Bounds wall-time and rate-limit footprint.
const DETAILS_MAX_NODE_IDS: usize = 1000;

/// Validate that an identifier is non-empty, within length, and uses
/// only the opaque-ID alphabet `[A-Za-z0-9_-]`.
fn validate_opaque_id(id: &str, label: &str) -> Result<()> {
    anyhow::ensure!(!id.is_empty(), "{label} must not be empty");
    anyhow::ensure!(
        id.len() <= MAX_ID_LEN,
        "{label} must be at most {MAX_ID_LEN} characters (got {})",
        id.len()
    );
    anyhow::ensure!(
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{label} must only contain ASCII letters, digits, '-', and '_'"
    );
    Ok(())
}

fn validate_node_id(node_id: &str) -> Result<()> {
    validate_opaque_id(node_id, "node ID")
}

fn validate_workspace_id(workspace: &str) -> Result<()> {
    validate_opaque_id(workspace, "workspace ID")
}

/// Aggregated bulk metadata-details run: per-id success/failure
/// outcome, deduplicated template definitions, and the original
/// requested-input count for the `count_*` fields.
struct BulkMetadataAggregate {
    total: usize,
    objects: Vec<Value>,
    templates: serde_json::Map<String, Value>,
    errors: Vec<Value>,
}

/// Get metadata details for one or more nodes. Single-id requests
/// keep the legacy single-node shape; 2+ ids route through the bulk
/// endpoint with client-side chunking at
/// `api::metadata::BULK_METADATA_DETAILS_MAX_IDS`.
async fn details(ctx: &CommandContext<'_>, workspace: &str, node_ids: &[String]) -> Result<()> {
    use std::collections::HashSet;

    validate_workspace_id(workspace)?;
    anyhow::ensure!(!node_ids.is_empty(), "at least one node ID is required");
    anyhow::ensure!(
        node_ids.len() <= DETAILS_MAX_NODE_IDS,
        "at most {DETAILS_MAX_NODE_IDS} node IDs accepted per call (got {})",
        node_ids.len()
    );
    for id in node_ids {
        validate_node_id(id)?;
    }

    // Dedupe case-insensitively to match server normalization.
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique: Vec<String> = Vec::with_capacity(node_ids.len());
    for id in node_ids {
        if seen.insert(id.to_ascii_lowercase()) {
            unique.push(id.clone());
        }
    }

    let client = ctx.build_client()?;

    if unique.len() == 1 {
        let value = api::metadata::get_node_metadata_details(&client, workspace, &unique[0])
            .await
            .context("failed to get metadata details")?;
        ctx.output.render(&value)?;
        return Ok(());
    }

    let aggregated = run_bulk_details(&client, workspace, &unique).await?;

    let succeeded = aggregated.objects.len();
    let errored = aggregated.errors.len();

    render_bulk_details(ctx, &aggregated)?;

    if succeeded == 0 && errored > 0 {
        anyhow::bail!("all {errored} node id(s) failed; see errors output for details");
    }
    if succeeded == 0 && errored == 0 && aggregated.total > 0 {
        anyhow::bail!(
            "server returned no objects and no errors for {} requested id(s); response was empty",
            aggregated.total
        );
    }
    Ok(())
}

/// Issue chunked bulk metadata-details calls and aggregate per-id
/// outcomes.
async fn run_bulk_details(
    client: &fastio_cli::client::ApiClient,
    workspace: &str,
    unique: &[String],
) -> Result<BulkMetadataAggregate> {
    let chunk_size = api::metadata::BULK_METADATA_DETAILS_MAX_IDS;
    let mut chunks: Vec<api::metadata::BulkMetadataDetailsResponse> = Vec::new();
    for chunk in unique.chunks(chunk_size) {
        let resp = api::metadata::get_bulk_node_metadata_details(client, workspace, chunk)
            .await
            .context("failed to fetch bulk metadata details")?;
        chunks.push(resp);
    }
    Ok(aggregate_metadata_chunks(unique.len(), chunks))
}

/// Aggregate per-chunk responses into a single result.
///
/// Server-returned objects are deduplicated by the same key the
/// metadata API uses (`node_id` first, falling back to `instance_id`,
/// then `object_id`). Per-id errors are deduplicated case-insensitively
/// by the echoed `node_id`. Templates are merged across chunks; if two
/// chunks define the same `template_id`, the later chunk's definition
/// wins (the server returns the same definition either way, so this is
/// a no-op in practice).
fn aggregate_metadata_chunks(
    total: usize,
    chunks: Vec<api::metadata::BulkMetadataDetailsResponse>,
) -> BulkMetadataAggregate {
    use std::collections::HashSet;

    let mut objects: Vec<Value> = Vec::new();
    let mut templates: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut object_keys: HashSet<String> = HashSet::new();
    let mut error_keys: HashSet<String> = HashSet::new();

    for resp in chunks {
        for obj in resp.objects {
            let key = obj
                .get("node_id")
                .or_else(|| obj.get("instance_id"))
                .or_else(|| obj.get("object_id"))
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase);
            if let Some(k) = key
                && !object_keys.insert(k)
            {
                tracing::warn!(object = %obj, "dropping duplicate metadata object from server response");
                continue;
            }
            objects.push(obj);
        }
        for (tid, tpl) in resp.templates {
            templates.insert(tid, tpl);
        }
        for err in resp.errors {
            if error_keys.insert(err.node_id.to_ascii_lowercase()) {
                errors.push(json!({
                    "node_id": err.node_id,
                    "code": err.code,
                    "message": err.message,
                }));
            }
        }
    }

    BulkMetadataAggregate {
        total,
        objects,
        templates,
        errors,
    }
}

/// Render the aggregated bulk metadata-details result.
///
/// JSON: emit the full `{count_*, objects, templates, errors}` map.
/// Other formats: render the `objects` array directly so tabular
/// renderers see it as the primary row data; emit per-error summary
/// lines to stderr (suppressed under `--quiet`).
fn render_bulk_details(ctx: &CommandContext<'_>, agg: &BulkMetadataAggregate) -> Result<()> {
    use fastio_cli::output::OutputFormat;

    let succeeded = agg.objects.len();
    let errored = agg.errors.len();
    let total = agg.total;

    if matches!(ctx.output.format, OutputFormat::Json) {
        let aggregated = json!({
            "count_total": total,
            "count_succeeded": succeeded,
            "count_errored": errored,
            "objects": agg.objects,
            "templates": Value::Object(agg.templates.clone()),
            "errors": agg.errors,
        });
        ctx.output.render(&aggregated)?;
        return Ok(());
    }

    ctx.output.render(&Value::Array(agg.objects.clone()))?;
    if !agg.errors.is_empty() && !ctx.output.quiet {
        eprintln!("--- {errored} of {total} id(s) failed ---");
        for err in &agg.errors {
            let raw_nid = err.get("node_id").and_then(Value::as_str).unwrap_or("");
            let nid = if raw_nid.is_empty() {
                "<no id>"
            } else {
                raw_nid
            };
            let code = err.get("code").and_then(Value::as_u64).unwrap_or(0);
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("");
            eprintln!("  {nid}: [{code}] {msg}");
        }
    }
    Ok(())
}

/// List files eligible for metadata extraction.
async fn eligible(
    ctx: &CommandContext<'_>,
    workspace: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::list_eligible(&client, workspace, limit, offset)
        .await
        .context("failed to list eligible files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Add files to a metadata template.
async fn add_nodes(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::add_nodes_to_template(&client, workspace, template_id, node_ids)
        .await
        .context("failed to add nodes to template")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Remove files from a metadata template.
async fn remove_nodes(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    node_ids: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::remove_nodes_from_template(&client, workspace, template_id, node_ids)
            .await
            .context("failed to remove nodes from template")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// List files mapped to a metadata template.
async fn list_nodes(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    sort_field: Option<&str>,
    sort_dir: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::list_template_nodes(
        &client,
        workspace,
        template_id,
        limit,
        offset,
        sort_field,
        sort_dir,
    )
    .await
    .context("failed to list template nodes")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// AI-based file matching for a template.
async fn auto_match(ctx: &CommandContext<'_>, workspace: &str, template_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::auto_match_template(&client, workspace, template_id)
        .await
        .context("failed to auto-match files to template")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Batch extract metadata for all files in a template.
async fn extract_all(ctx: &CommandContext<'_>, workspace: &str, template_id: &str) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::extract_all(&client, workspace, template_id)
        .await
        .context("failed to batch extract metadata")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Enqueue an async metadata extraction for a single file.
async fn extract(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    template_id: &str,
    fields: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::extract_node_metadata(&client, workspace, node_id, template_id, fields)
            .await
            .context("failed to enqueue metadata extraction")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Preview files that match a proposed template name + description.
async fn preview_match(
    ctx: &CommandContext<'_>,
    workspace: &str,
    name: &str,
    description: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::preview_match(&client, workspace, name, description)
        .await
        .context("failed to preview matching files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Request AI-suggested column definitions for a proposed template.
async fn suggest_fields(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_ids: &str,
    description: &str,
    user_context: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::suggest_fields(&client, workspace, node_ids, description, user_context)
            .await
            .context("failed to request suggested fields")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Create a metadata template (a.k.a. view) with the given column definitions.
async fn create_template(
    ctx: &CommandContext<'_>,
    workspace: &str,
    name: &str,
    description: &str,
    category: &str,
    fields: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::create_template(&client, workspace, name, description, category, fields)
            .await
            .context("failed to create metadata template")?;
    ctx.output.render(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::aggregate_metadata_chunks;
    use fastio_cli::api::metadata::{
        BulkMetadataDetailsResponse, parse_bulk_metadata_details_response,
    };
    use serde_json::json;

    fn make_chunk(body: &serde_json::Value) -> BulkMetadataDetailsResponse {
        parse_bulk_metadata_details_response(body).expect("test body should parse")
    }

    #[test]
    fn aggregate_metadata_chunks_dedupes_repeated_node_ids() {
        let chunk_a = make_chunk(&json!({
            "format": "multi",
            "objects": [{"node_id": "ABC", "template_id": "tpl1"}],
            "templates": {"tpl1": {"name": "Photos"}},
            "errors": []
        }));
        let chunk_b = make_chunk(&json!({
            "format": "multi",
            "objects": [{"node_id": "abc", "template_id": "tpl1"}],
            "templates": {"tpl1": {"name": "Photos"}},
            "errors": []
        }));
        let agg = aggregate_metadata_chunks(2, vec![chunk_a, chunk_b]);
        assert_eq!(agg.objects.len(), 1);
        assert_eq!(agg.templates.len(), 1);
        assert!(agg.errors.is_empty());
    }

    #[test]
    fn aggregate_metadata_chunks_dedupes_repeated_error_node_ids() {
        let chunk = make_chunk(&json!({
            "format": "multi",
            "objects": [],
            "errors": [
                {"node_id": "X", "code": 191_049, "message": "not found"},
                {"node_id": "x", "code": 191_049, "message": "not found"}
            ]
        }));
        let agg = aggregate_metadata_chunks(1, vec![chunk]);
        assert!(agg.objects.is_empty());
        assert_eq!(agg.errors.len(), 1);
    }

    #[test]
    fn aggregate_metadata_chunks_partial_success_yields_both_lists() {
        let chunk = make_chunk(&json!({
            "format": "multi",
            "objects": [{"node_id": "ok", "template_id": "tpl1"}],
            "templates": {"tpl1": {"name": "T"}},
            "errors": [{"node_id": "missing", "code": 191_049, "message": "not found"}]
        }));
        let agg = aggregate_metadata_chunks(2, vec![chunk]);
        assert_eq!(agg.objects.len(), 1);
        assert_eq!(agg.errors.len(), 1);
        assert_eq!(agg.total, 2);
        assert_eq!(agg.templates.len(), 1);
    }

    #[test]
    fn aggregate_metadata_chunks_merges_templates_across_chunks() {
        // Two chunks each carry their own templates; merge keeps both.
        let chunk_a = make_chunk(&json!({
            "format": "multi",
            "objects": [{"node_id": "a", "template_id": "tpl1"}],
            "templates": {"tpl1": {"name": "Photos"}},
            "errors": []
        }));
        let chunk_b = make_chunk(&json!({
            "format": "multi",
            "objects": [{"node_id": "b", "template_id": "tpl2"}],
            "templates": {"tpl2": {"name": "Receipts"}},
            "errors": []
        }));
        let agg = aggregate_metadata_chunks(2, vec![chunk_a, chunk_b]);
        assert_eq!(agg.objects.len(), 2);
        assert_eq!(agg.templates.len(), 2);
        assert!(agg.templates.contains_key("tpl1"));
        assert!(agg.templates.contains_key("tpl2"));
    }

    #[test]
    fn aggregate_metadata_chunks_all_errored() {
        let chunk = make_chunk(&json!({
            "format": "multi",
            "objects": [],
            "errors": [
                {"node_id": "a", "code": 191_049, "message": "not found"},
                {"node_id": "b", "code": 147_196, "message": "invalid id"}
            ]
        }));
        let agg = aggregate_metadata_chunks(2, vec![chunk]);
        assert!(agg.objects.is_empty());
        assert_eq!(agg.errors.len(), 2);
    }
}
