/// Metadata extraction command implementations for `fastio metadata *`.
///
/// Handles listing eligible files, managing template-file mappings,
/// AI-based matching, and metadata extraction.
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::api::metadata::ExtractJobState;

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
    /// AI-based file matching for a template. Spends AI credits.
    AutoMatch {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// Optional server-clamped batch-size override.
        batch_size: Option<u32>,
        /// AI-spend acknowledgement flag (skips the interactive prompt).
        confirm_ai_spend: bool,
    },
    /// Batch extract metadata for all files in a template. Spends AI credits.
    ExtractAll {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// JSON-encoded array of field names for partial extraction.
        fields: Option<String>,
        /// Re-extract nodes that already have values for this template.
        force: bool,
        /// AI-spend acknowledgement flag (skips the interactive prompt).
        confirm_ai_spend: bool,
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
    /// Enqueue an async metadata extraction for a single file. Spends AI
    /// credits; optionally polls the job to a terminal state.
    Extract {
        /// Workspace ID.
        workspace: String,
        /// Node ID of the file.
        node_id: String,
        /// Template ID to extract against (optional; server defaults to the
        /// first template mapped to the file).
        template_id: Option<String>,
        /// JSON-encoded array of field names for partial extraction.
        fields: Option<String>,
        /// Poll the workspace jobs-status endpoint until the job is
        /// terminal, then report the outcome.
        wait: bool,
        /// Seconds between job-status polls when `wait` is set.
        poll_interval: Option<u64>,
        /// AI-spend acknowledgement flag (skips the interactive prompt).
        confirm_ai_spend: bool,
    },
    /// Preview files that match a proposed template name + description.
    /// Spends AI credits.
    PreviewMatch {
        /// Workspace ID.
        workspace: String,
        /// Proposed template name (1-255 chars).
        name: String,
        /// Natural-language template description.
        description: String,
        /// AI-spend acknowledgement flag (skips the interactive prompt).
        confirm_ai_spend: bool,
    },
    /// Request AI-suggested column definitions for a proposed template.
    /// Spends AI credits.
    SuggestFields {
        /// Workspace ID.
        workspace: String,
        /// JSON-encoded array of 1-25 sample node IDs.
        node_ids: String,
        /// Template description.
        description: String,
        /// Optional short user hint (max 64 chars, letters/numbers/spaces).
        user_context: Option<String>,
        /// AI-spend acknowledgement flag (skips the interactive prompt).
        confirm_ai_spend: bool,
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
    /// Lexical keyword search over workspace metadata field values.
    Search {
        /// Workspace ID.
        workspace: String,
        /// Search keyword(s).
        query: String,
        /// Optional template scope.
        template_id: Option<String>,
        /// Page size (1-100).
        limit: Option<u32>,
        /// Skip-N offset.
        offset: Option<u32>,
    },
    /// Enqueue an async TSV export of the caller's saved view.
    ExportView {
        /// Workspace ID.
        workspace: String,
        /// Template ID.
        template_id: String,
        /// Destination folder (defaults to workspace root).
        parent_node_id: Option<String>,
    },
}

/// Execute a metadata subcommand.
#[allow(clippy::too_many_lines)]
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
            batch_size,
            confirm_ai_spend,
        } => auto_match(ctx, workspace, template_id, *batch_size, *confirm_ai_spend).await,
        MetadataCommand::ExtractAll {
            workspace,
            template_id,
            fields,
            force,
            confirm_ai_spend,
        } => {
            extract_all(
                ctx,
                workspace,
                template_id,
                fields.as_deref(),
                *force,
                *confirm_ai_spend,
            )
            .await
        }
        MetadataCommand::Details {
            workspace,
            node_ids,
        } => details(ctx, workspace, node_ids).await,
        MetadataCommand::Extract {
            workspace,
            node_id,
            template_id,
            fields,
            wait,
            poll_interval,
            confirm_ai_spend,
        } => {
            extract(
                ctx,
                workspace,
                node_id,
                template_id.as_deref(),
                fields.as_deref(),
                *wait,
                *poll_interval,
                *confirm_ai_spend,
            )
            .await
        }
        MetadataCommand::PreviewMatch {
            workspace,
            name,
            description,
            confirm_ai_spend,
        } => preview_match(ctx, workspace, name, description, *confirm_ai_spend).await,
        MetadataCommand::SuggestFields {
            workspace,
            node_ids,
            description,
            user_context,
            confirm_ai_spend,
        } => {
            suggest_fields(
                ctx,
                workspace,
                node_ids,
                description,
                user_context.as_deref(),
                *confirm_ai_spend,
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
        MetadataCommand::Search {
            workspace,
            query,
            template_id,
            limit,
            offset,
        } => {
            search(
                ctx,
                workspace,
                query,
                template_id.as_deref(),
                *limit,
                *offset,
            )
            .await
        }
        MetadataCommand::ExportView {
            workspace,
            template_id,
            parent_node_id,
        } => export_view(ctx, workspace, template_id, parent_node_id.as_deref()).await,
    }
}

/// Maximum accepted length for a node or workspace identifier.
const MAX_ID_LEN: usize = 128;

/// Runtime cap on positional node IDs per `fastio metadata details`
/// invocation. Bounds wall-time and rate-limit footprint.
const DETAILS_MAX_NODE_IDS: usize = 1000;

/// Default seconds between job-status polls when `extract --wait` is set.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 3;
/// Lower bound on the poll interval (avoids hammering the API).
const MIN_POLL_INTERVAL_SECS: u64 = 1;
/// Upper bound on the poll interval.
const MAX_POLL_INTERVAL_SECS: u64 = 60;
/// Hard ceiling on the `extract --wait` poll loop. Sized well under the
/// ~1-hour JWT lifetime so a stuck job surfaces a clear timeout (with a
/// re-auth hint on a 401) rather than hanging indefinitely.
const EXTRACT_WAIT_MAX_SECS: u64 = 600;

/// Gate an AI-credit-spending action behind explicit acknowledgement.
///
/// Returns `Ok(())` when the caller may proceed:
/// - `confirm_ai_spend == true` (the `--confirm-ai-spend` flag was passed), or
/// - stdin AND stderr are both a TTY and the user answers `y`/`yes` to the
///   interactive prompt.
///
/// Otherwise returns an error. Non-interactive callers (pipes, MCP, CI)
/// that omit the flag are blocked deterministically — they never hang on a
/// prompt that has no reader.
fn confirm_ai_spend(action: &str, cost_note: &str, confirm_ai_spend: bool) -> Result<()> {
    use std::io::{self, BufRead, IsTerminal, Write};

    if confirm_ai_spend {
        return Ok(());
    }

    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    if !interactive {
        anyhow::bail!(
            "'{action}' spends AI credits ({cost_note}). Re-run with --confirm-ai-spend to proceed."
        );
    }

    eprint!("'{action}' spends AI credits ({cost_note}). Proceed? [y/N] ");
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("failed to read confirmation from stdin")?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        Ok(())
    } else {
        anyhow::bail!("aborted: AI-spend not confirmed for '{action}'");
    }
}

/// Clamp a user-supplied poll interval into the supported range.
fn clamp_poll_interval(secs: Option<u64>) -> u64 {
    secs.unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
        .clamp(MIN_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS)
}

/// Extract the `job_id` from a single-file extract `202` response body,
/// if present. A full-row call whose effective scope is empty responds
/// successfully without a `job_id`, so this returns `None` in that case.
fn extract_job_id(resp: &Value) -> Option<String> {
    let payload = resp.get("response").unwrap_or(resp);
    payload
        .get("job_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

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

/// AI-based file matching for a template. Spends AI credits.
async fn auto_match(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    batch_size: Option<u32>,
    spend_ack: bool,
) -> Result<()> {
    confirm_ai_spend(
        "metadata auto-match",
        "one AI classification per eligible file scanned",
        spend_ack,
    )?;
    let client = ctx.build_client()?;
    let value = api::metadata::auto_match_template(&client, workspace, template_id, batch_size)
        .await
        .context("failed to auto-match files to template")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Batch extract metadata for all files in a template. Spends AI credits.
async fn extract_all(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    fields: Option<&str>,
    force: bool,
    spend_ack: bool,
) -> Result<()> {
    confirm_ai_spend(
        "metadata extract-all",
        "one AI extraction per mapped file (up to 1,000 files)",
        spend_ack,
    )?;
    let client = ctx.build_client()?;
    let value = api::metadata::extract_all(&client, workspace, template_id, fields, force)
        .await
        .context("failed to batch extract metadata")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Enqueue an async metadata extraction for a single file. Spends AI
/// credits; optionally polls the job to a terminal state.
#[allow(clippy::too_many_arguments)]
async fn extract(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_id: &str,
    template_id: Option<&str>,
    fields: Option<&str>,
    wait: bool,
    poll_interval: Option<u64>,
    spend_ack: bool,
) -> Result<()> {
    confirm_ai_spend(
        "metadata extract",
        "one AI extraction for this file",
        spend_ack,
    )?;
    let client = ctx.build_client()?;
    let value =
        api::metadata::extract_node_metadata(&client, workspace, node_id, template_id, fields)
            .await
            .context("failed to enqueue metadata extraction")?;
    ctx.output.render(&value)?;

    if !wait {
        return Ok(());
    }

    let Some(job_id) = extract_job_id(&value) else {
        if !ctx.output.quiet {
            eprintln!(
                "no job was enqueued (empty effective extraction scope); nothing to wait for."
            );
        }
        return Ok(());
    };

    let interval = clamp_poll_interval(poll_interval);
    wait_for_extract_job(ctx, &client, workspace, node_id, &job_id, interval).await
}

/// Poll the workspace jobs-status endpoint until the single-file extraction
/// job reaches a terminal state, then report the outcome.
///
/// Strategy mirrors `ripley ask --wait`: a bounded loop
/// ([`EXTRACT_WAIT_MAX_SECS`]) so it cannot hang past the ~1-hour JWT
/// lifetime, with a 401 short-circuiting to a clear re-auth hint rather
/// than spinning. Transient (non-401) errors are tolerated and retried on
/// the next tick. A job that is absent from jobs-status (`NotFound`) is NOT
/// treated as success: terminal entries only age out after ~1h, well beyond
/// this bounded window, so within the window a missing entry means the job is
/// not yet visible (or just enqueued) — the loop keeps polling until it
/// observes an EXPLICIT terminal state (`completed`/`errored`) or hits the
/// deadline (which surfaces an indeterminate timeout, never a false success).
async fn wait_for_extract_job(
    ctx: &CommandContext<'_>,
    client: &fastio_cli::client::ApiClient,
    workspace: &str,
    node_id: &str,
    job_id: &str,
    interval_secs: u64,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(EXTRACT_WAIT_MAX_SECS);

    if !ctx.output.quiet {
        eprintln!("waiting for extraction job {job_id} (polling every {interval_secs}s)...");
    }

    loop {
        match api::workspace::jobs_status(client, workspace).await {
            Ok(status) => {
                match api::metadata::classify_single_extract_job(&status, node_id, Some(job_id)) {
                    ExtractJobState::Completed => {
                        if !ctx.output.quiet {
                            eprintln!(
                                "extraction completed; read values via \
                                 'fastio metadata details --workspace {workspace} {node_id}'."
                            );
                        }
                        return Ok(());
                    }
                    ExtractJobState::Errored(msg) => {
                        let detail = msg.unwrap_or_else(|| "no error message provided".to_owned());
                        anyhow::bail!("extraction job {job_id} failed: {detail}");
                    }
                    // `NotFound` is NOT treated as success. The server only
                    // ages out terminal entries after ~1h, far beyond this
                    // bounded `EXTRACT_WAIT_MAX_SECS` window, so within the
                    // window a missing entry means the job is not yet visible
                    // (or just enqueued) — keep polling until we observe an
                    // EXPLICIT terminal state (`completed`/`errored`) or hit
                    // the deadline. Reporting success on `NotFound` here would
                    // risk a false success. `Pending` and the
                    // `#[non_exhaustive]` catch-all also keep us polling.
                    _ => {}
                }
            }
            Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 401 => {
                anyhow::bail!(
                    "authentication expired while waiting for extraction job {job_id}. The job \
                     may still complete server-side; re-authenticate (fastio auth login) and read \
                     values via 'fastio metadata details --workspace {workspace} {node_id}'."
                );
            }
            // Transient errors are tolerated; retry on the next tick.
            Err(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after ~{EXTRACT_WAIT_MAX_SECS}s waiting for extraction job {job_id}. \
                 The job may still complete server-side; poll \
                 'fastio workspace jobs-status --workspace-id {workspace}' or read values via \
                 'fastio metadata details --workspace {workspace} {node_id}'."
            );
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let sleep = remaining.min(Duration::from_secs(interval_secs));
        tokio::time::sleep(sleep).await;

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after ~{EXTRACT_WAIT_MAX_SECS}s waiting for extraction job {job_id}. \
                 The job may still complete server-side; poll \
                 'fastio workspace jobs-status --workspace-id {workspace}' or read values via \
                 'fastio metadata details --workspace {workspace} {node_id}'."
            );
        }
    }
}

/// Preview files that match a proposed template name + description.
///
/// Spends AI credits (ai.txt:2272 — "Requires available AI credits"), so the
/// same `--confirm-ai-spend` gate as the other AI-spending metadata commands
/// applies for consistent credit-spend protection.
async fn preview_match(
    ctx: &CommandContext<'_>,
    workspace: &str,
    name: &str,
    description: &str,
    spend_ack: bool,
) -> Result<()> {
    confirm_ai_spend(
        "metadata preview-match",
        "AI classification over a sample of eligible files",
        spend_ack,
    )?;
    let client = ctx.build_client()?;
    let value = api::metadata::preview_match(&client, workspace, name, description)
        .await
        .context("failed to preview matching files")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Request AI-suggested column definitions for a proposed template. Spends
/// AI credits.
async fn suggest_fields(
    ctx: &CommandContext<'_>,
    workspace: &str,
    node_ids: &str,
    description: &str,
    user_context: Option<&str>,
    spend_ack: bool,
) -> Result<()> {
    confirm_ai_spend(
        "metadata suggest-fields",
        "one AI call over the sampled files",
        spend_ack,
    )?;
    let client = ctx.build_client()?;
    let value =
        api::metadata::suggest_fields(&client, workspace, node_ids, description, user_context)
            .await
            .context("failed to request suggested fields")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Lexical keyword search over workspace metadata field values.
async fn search(
    ctx: &CommandContext<'_>,
    workspace: &str,
    query: &str,
    template_id: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value =
        api::metadata::search_metadata(&client, workspace, query, template_id, limit, offset)
            .await
            .context("failed to search metadata")?;
    ctx.output.render(&value)?;
    Ok(())
}

/// Enqueue an async TSV export of the caller's saved view for a template.
async fn export_view(
    ctx: &CommandContext<'_>,
    workspace: &str,
    template_id: &str,
    parent_node_id: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::export_view(&client, workspace, template_id, parent_node_id)
        .await
        .context("failed to enqueue metadata view export")?;
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
    use super::{
        DEFAULT_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS, MIN_POLL_INTERVAL_SECS,
        aggregate_metadata_chunks, clamp_poll_interval, confirm_ai_spend, extract_job_id,
    };
    use fastio_cli::api::metadata::{
        BulkMetadataDetailsResponse, parse_bulk_metadata_details_response,
    };
    use serde_json::json;

    fn make_chunk(body: &serde_json::Value) -> BulkMetadataDetailsResponse {
        parse_bulk_metadata_details_response(body).expect("test body should parse")
    }

    #[test]
    fn confirm_ai_spend_passes_with_flag() {
        // The acknowledgement flag bypasses any prompt.
        assert!(confirm_ai_spend("metadata extract", "one extraction", true).is_ok());
    }

    #[test]
    fn confirm_ai_spend_blocks_non_interactive_without_flag() {
        // Under `cargo test`, stdin/stderr are not a TTY, so the prompt
        // path is skipped and the spend is blocked deterministically.
        let err = confirm_ai_spend("metadata auto-match", "per-file classification", false)
            .expect_err("spend must be blocked without the flag in a non-TTY context");
        let msg = err.to_string();
        assert!(msg.contains("--confirm-ai-spend"), "message was: {msg}");
        assert!(msg.contains("metadata auto-match"), "message was: {msg}");
    }

    #[test]
    fn preview_match_is_gated_on_ai_spend() {
        // FIX 5: preview-match spends AI credits (ai.txt:2272), so it routes
        // through the same `confirm_ai_spend` gate. With the flag it passes;
        // without it (non-TTY) it is blocked with the standard message.
        assert!(
            confirm_ai_spend(
                "metadata preview-match",
                "AI classification over a sample of eligible files",
                true,
            )
            .is_ok()
        );
        let err = confirm_ai_spend(
            "metadata preview-match",
            "AI classification over a sample of eligible files",
            false,
        )
        .expect_err("preview-match spend must be blocked without the flag in a non-TTY context");
        let msg = err.to_string();
        assert!(msg.contains("--confirm-ai-spend"), "message was: {msg}");
        assert!(msg.contains("metadata preview-match"), "message was: {msg}");
    }

    #[test]
    fn clamp_poll_interval_uses_default_when_absent() {
        assert_eq!(clamp_poll_interval(None), DEFAULT_POLL_INTERVAL_SECS);
    }

    #[test]
    fn clamp_poll_interval_clamps_bounds() {
        assert_eq!(clamp_poll_interval(Some(0)), MIN_POLL_INTERVAL_SECS);
        assert_eq!(clamp_poll_interval(Some(99_999)), MAX_POLL_INTERVAL_SECS);
        assert_eq!(clamp_poll_interval(Some(5)), 5);
    }

    #[test]
    fn extract_job_id_reads_enveloped_and_flat_bodies() {
        // Enveloped 202 body.
        let enveloped = json!({
            "result": "yes",
            "response": { "job_id": "aj_123", "status": "queued" }
        });
        assert_eq!(extract_job_id(&enveloped).as_deref(), Some("aj_123"));
        // Flat body.
        let flat = json!({ "job_id": "aj_456", "status": "queued" });
        assert_eq!(extract_job_id(&flat).as_deref(), Some("aj_456"));
    }

    #[test]
    fn extract_job_id_none_for_empty_scope_response() {
        // A full-row call with an empty effective scope returns no job_id.
        let no_job = json!({ "result": "yes", "response": { "status": "queued" } });
        assert_eq!(extract_job_id(&no_job), None);
        // Empty-string job_id is treated as absent.
        let empty = json!({ "response": { "job_id": "" } });
        assert_eq!(extract_job_id(&empty), None);
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
