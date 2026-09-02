/// Metadata command implementations for `fastio metadata *`.
///
/// Handles listing eligible files, reading a node's metadata details,
/// single-file AI extraction, and lexical search over metadata values.
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::CommandContext;
use super::{PollAction, classify_poll_error};
use fastio_cli::api;
use fastio_cli::api::metadata::ExtractJobState;
use fastio_cli::error::CliError;

/// Metadata subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MetadataCommand {
    /// List files eligible for metadata extraction.
    Eligible {
        /// Workspace ID.
        workspace: String,
        /// Records per page (server-quantized to 25/100/250).
        page_size: Option<u32>,
        /// Opaque cursor from a previous response.
        cursor: Option<String>,
        /// Filter by MIME type.
        mimetype: Option<String>,
        /// Filter by file extension.
        extension: Option<String>,
        /// True when the deprecated, always-ignored `--limit`/`--offset` were
        /// supplied, so the command can say so instead of silently dropping
        /// them as it used to.
        used_offset_pagination: bool,
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
        /// JSON-encoded array of field names, or `None` for a full
        /// extraction. Naming fields makes the request EXCLUSIVE — those
        /// fields alone are written and anything else the model returns is
        /// discarded — though the file is still read in full and a named
        /// field may not come back if the document does not contain it.
        fields: Option<String>,
        /// Poll the workspace jobs-status endpoint until the job is
        /// terminal, then report the outcome.
        wait: bool,
        /// Seconds between job-status polls when `wait` is set.
        poll_interval: Option<u64>,
        /// AI-spend acknowledgement flag (skips the interactive prompt).
        confirm_ai_spend: bool,
    },
    /// Lexical keyword search over workspace metadata field values.
    Search {
        /// Workspace ID.
        workspace: String,
        /// Search keyword(s).
        query: String,
        /// Page size (1-100).
        limit: Option<u32>,
        /// Skip-N offset.
        offset: Option<u32>,
    },
}

/// Execute a metadata subcommand.
#[allow(clippy::too_many_lines)]
pub async fn execute(command: &MetadataCommand, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        MetadataCommand::Eligible {
            workspace,
            page_size,
            cursor,
            mimetype,
            extension,
            used_offset_pagination,
        } => {
            eligible(
                ctx,
                workspace,
                &api::metadata::EligibleParams::new()
                    .page_size(*page_size)
                    .cursor(cursor.as_deref())
                    .mimetype(mimetype.as_deref())
                    .extension(extension.as_deref()),
                *used_offset_pagination,
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
            fields,
            wait,
            poll_interval,
            confirm_ai_spend,
        } => {
            extract(
                ctx,
                workspace,
                node_id,
                fields.as_deref(),
                *wait,
                *poll_interval,
                *confirm_ai_spend,
            )
            .await
        }
        MetadataCommand::Search {
            workspace,
            query,
            limit,
            offset,
        } => search(ctx, workspace, query, *limit, *offset).await,
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

/// Server-side prune window for TERMINAL job entries, confirmed server-side
/// 2026-08-26.
///
/// A `completed` entry DISAPPEARS from `jobs-status` this long after its last
/// update. The poll loop deliberately reads a missing entry as "not yet
/// visible" rather than as success — correct, and the reason this bound is
/// load-bearing: past it, absence stops meaning "too early" and starts meaning
/// "pruned", and the loop would wait for something that will never return.
const SERVER_TERMINAL_JOB_PRUNE_SECS: u64 = 3600;

// The poll deadline MUST stay inside the prune window, or `--wait` can outlive
// the evidence it is waiting for.
//
// This relationship was previously asserted only in a comment ("far beyond this
// bounded window") — a claim about ANOTHER system's constant that nothing here
// could check, so raising `EXTRACT_WAIT_MAX_SECS` would have silently falsified
// it and turned a bounded timeout into a wait for a record that had already
// been deleted. Now it fails the build instead.
//
// Deliberately a strict inequality with headroom, not `<=`: the two clocks are
// independent and the server's threshold is a value we do not control.
const _: () = assert!(
    EXTRACT_WAIT_MAX_SECS * 2 < SERVER_TERMINAL_JOB_PRUNE_SECS,
    "extract --wait deadline must stay well inside the server's terminal-job prune window"
);

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

/// Extract the `job_id` from a single-file extract response body, if present.
///
/// Absence means simply **no job was enqueued**, and this returns `None`.
/// Deliberately cause-free: an `already_extracted` reply is a `200` carrying no
/// job at all, so neither "the effective scope was empty" nor "this is the
/// original `202`" is reliably true — naming one cause would state a diagnosis
/// the response does not support.
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
        let mut value = api::metadata::get_node_metadata_details(&client, workspace, &unique[0])
            .await
            .context("failed to get metadata details")?;
        strip_declared_types_for_humans(ctx, &mut value);
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

/// Drop the template-DECLARED `metadata[].type` before a HUMAN-facing render.
///
/// The declared type is echoed onto every stored value and can disagree with
/// it, so rendering it beside the value asserts something the CLI cannot back.
/// `--format json` is a machine-readable contract and keeps the field verbatim
/// — removing it there would be a breaking change for existing consumers.
fn strip_declared_types_for_humans(ctx: &CommandContext<'_>, value: &mut Value) {
    use fastio_cli::output::OutputFormat;

    if !matches!(ctx.output.format, OutputFormat::Json) {
        api::metadata::strip_declared_metadata_types(value);
    }
}

/// Build the `--format json` envelope for a bulk metadata-details result.
///
/// `objects` is relayed VERBATIM: JSON is the machine-readable contract, so
/// the declared `metadata[].type` stays, exactly as it does on the single-node
/// path. Only human formats go through `strip_declared_types_for_humans`.
fn bulk_details_json_envelope(agg: &BulkMetadataAggregate) -> Value {
    json!({
        "count_total": agg.total,
        "count_succeeded": agg.objects.len(),
        "count_errored": agg.errors.len(),
        "objects": agg.objects,
        "templates": Value::Object(agg.templates.clone()),
        "errors": agg.errors,
    })
}

/// Render the aggregated bulk metadata-details result.
///
/// JSON: emit the full `{count_*, objects, templates, errors}` map.
/// Other formats: render the `objects` array directly so tabular
/// renderers see it as the primary row data; emit per-error summary
/// lines to stderr (suppressed under `--quiet`).
fn render_bulk_details(ctx: &CommandContext<'_>, agg: &BulkMetadataAggregate) -> Result<()> {
    use fastio_cli::output::OutputFormat;

    let errored = agg.errors.len();
    let total = agg.total;

    if matches!(ctx.output.format, OutputFormat::Json) {
        ctx.output.render(&bulk_details_json_envelope(agg))?;
        return Ok(());
    }

    let mut objects = Value::Array(agg.objects.clone());
    strip_declared_types_for_humans(ctx, &mut objects);
    ctx.output.render(&objects)?;
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
///
/// Cursor-paginated. `used_offset_pagination` reports that the caller passed
/// the retired `--limit`/`--offset`; those are announced as ignored rather than
/// dropped in silence, because that silence is the whole defect — the server
/// accepted and ignored them, and the response looked exactly like a page the
/// caller had asked for.
async fn eligible(
    ctx: &CommandContext<'_>,
    workspace: &str,
    params: &api::metadata::EligibleParams<'_>,
    used_offset_pagination: bool,
) -> Result<()> {
    if used_offset_pagination && !ctx.output.quiet {
        eprintln!(
            "[deprecated] `--limit`/`--offset` do nothing on `metadata eligible` and have been \
             IGNORED — this endpoint is cursor-paginated. The listing below is a full page, not \
             the slice you asked for. Use `--page-size` (server-snapped to 25/100/250) and \
             `--cursor` from the previous response."
        );
    }
    let client = ctx.build_client()?;
    let value = api::metadata::list_eligible(&client, workspace, params)
        .await
        .context("failed to list eligible files")?;
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
    let value = api::metadata::extract_node_metadata(&client, workspace, node_id, fields)
        .await
        .context("failed to enqueue metadata extraction")?;
    ctx.output.render(&value)?;

    if !wait {
        return Ok(());
    }

    let Some(job_id) = extract_job_id(&value) else {
        // CAUSE-FREE ON PURPOSE. This used to assert "(empty effective
        // extraction scope)" as THE reason, which is now known to be wrong in a
        // real case: an extraction already claimed at this file's current
        // version and extractor revision also returns success with no job. The
        // two are different situations with different remedies, and naming the
        // wrong one sends the user to re-check a scope that was fine.
        //
        // Same rule as the status hints: a correct DETECTION
        // does not make the ADVICE correct, and where the cause is not
        // established the honest line names the observable fact and an action,
        // never a diagnosis. The rendered response above is the server's own
        // account, so it is pointed at rather than paraphrased here.
        if !ctx.output.quiet {
            eprintln!(
                "no job was enqueued, so there is nothing to wait for — see the `status` in the \
                 response above. Read the current values with `fastio metadata details`."
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
                    // `errored` IS NOT PROOF OF FINALITY, so the message must
                    // not assert that it is.
                    //
                    // Confirmed server-side 2026-08-27, live in production: a
                    // RETRYABLE extraction failure — mutex contention, a
                    // transient Redis fault — is published as `errored` for the
                    // whole backoff while the queue schedules a retry, and may
                    // flip to completed afterwards. A caller treating `errored`
                    // as final gives up on a run that is about to succeed.
                    //
                    // Stopping is still right: the deadline is bounded and this
                    // is the job's last reported state. What must not happen is
                    // the CLAIM of finality — report the observation and an
                    // action, never a verdict the status does not support. This
                    // deliberately does NOT describe the retry mechanism: that
                    // is the server's to change, and consequence-copy survives
                    // its evidence narrowing where mechanism-copy does not.
                    ExtractJobState::Errored(msg) => {
                        let detail = msg.unwrap_or_else(|| "no error message provided".to_owned());
                        anyhow::bail!(
                            "extraction job {job_id} last reported `errored`: {detail}. This is the \
                             job's most recent status, not proof it will not complete — re-read the \
                             values with `fastio metadata details` before concluding the extraction \
                             did not happen."
                        );
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
            Err(CliError::Api(e)) if e.http_status == 401 => {
                anyhow::bail!(
                    "authentication expired while waiting for extraction job {job_id}. The job \
                     may still complete server-side; re-authenticate (fastio auth login) and read \
                     values via 'fastio metadata details --workspace {workspace} {node_id}'."
                );
            }
            // Classify rather than swallow: a transient blip retries on the next
            // tick; a persistent 4xx (403/404/402/parse) is surfaced instead of
            // looping silently to the deadline.
            Err(e) => match classify_poll_error(e) {
                PollAction::RateLimited { retry_after_secs } => {
                    if retry_after_secs > 0 {
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        tokio::time::sleep(remaining.min(Duration::from_secs(retry_after_secs)))
                            .await;
                    }
                }
                PollAction::RetryTransient => {}
                PollAction::Fatal(err) => {
                    return Err(anyhow::Error::new(err).context(format!(
                        "error while waiting for extraction job {job_id}; read values via \
                         'fastio metadata details --workspace {workspace} {node_id}'"
                    )));
                }
            },
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

/// Lexical keyword search over workspace metadata field values.
async fn search(
    ctx: &CommandContext<'_>,
    workspace: &str,
    query: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::metadata::search_metadata(&client, workspace, query, limit, offset)
        .await
        .context("failed to search metadata")?;
    ctx.output.render(&value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CommandContext, DEFAULT_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS, MIN_POLL_INTERVAL_SECS,
        aggregate_metadata_chunks, bulk_details_json_envelope, clamp_poll_interval,
        confirm_ai_spend, extract_job_id, strip_declared_types_for_humans, validate_opaque_id,
    };
    use fastio_cli::api::metadata::{
        BulkMetadataDetailsResponse, parse_bulk_metadata_details_response,
    };
    use serde_json::json;

    fn make_chunk(body: &serde_json::Value) -> BulkMetadataDetailsResponse {
        parse_bulk_metadata_details_response(body).expect("test body should parse")
    }

    #[test]
    fn validate_opaque_id_accepts_29_and_30_char_forms() {
        // Regression guard: OpaqueIds are no longer fixed-length. Workflow-family
        // ids are 30 chars (35 hyphenated); everything else is 29 (34 hyphenated).
        // Both lengths, raw and hyphenated, must validate.
        for id in [
            "f3jm5zqzfxpxdr2dx8z5bvnb3rpjf",       // 29-char raw
            "f3jm5-zqzfx-pxdr2-dx8z5-bvnb3-rpjf",  // 34-char hyphenated
            "wa3jm5zqzfxpxdr2dx8z5bvnb3rpjf",      // 30-char raw (workflow)
            "wa3jm-5zqzf-xpxdr-2dx8z-5bvnb-3rpjf", // 35-char hyphenated
        ] {
            validate_opaque_id(id, "id").unwrap_or_else(|e| panic!("rejected {id:?}: {e}"));
        }
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
        // Labelled with a SURVIVING command. This read `metadata auto-match`
        // until 2026-08-28 — a command since removed. The helper is
        // generic and the test was still valid, but a fixture naming a retired
        // command reads as coverage of something that no longer exists.
        let err = confirm_ai_spend("metadata extract", "one extraction", false)
            .expect_err("spend must be blocked without the flag in a non-TTY context");
        let msg = err.to_string();
        assert!(msg.contains("--confirm-ai-spend"), "message was: {msg}");
        assert!(msg.contains("metadata extract"), "message was: {msg}");
    }

    #[test]
    fn confirm_ai_spend_gate_is_symmetric_in_the_flag() {
        // The same action passes WITH the acknowledgement flag and is blocked
        // WITHOUT it, and the block names both the flag and the action — so the
        // gate turns on the flag alone, not on anything about the caller.
        //
        // Was `preview_match_is_gated_on_ai_spend`, labelled `metadata
        // preview-match`: a command since removed. Renamed and retargeted
        // rather than deleted — what it actually exercises is the generic gate,
        // which is still live and still worth pinning.
        let action = "metadata extract";
        let note = "AI extraction over the file";
        assert!(confirm_ai_spend(action, note, true).is_ok());
        let err = confirm_ai_spend(action, note, false)
            .expect_err("spend must be blocked without the flag in a non-TTY context");
        let msg = err.to_string();
        assert!(msg.contains("--confirm-ai-spend"), "message was: {msg}");
        assert!(msg.contains(action), "message was: {msg}");
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
        // Enveloped extract response body.
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
    fn extract_job_id_none_when_response_has_no_job_id() {
        // A response that enqueued no job carries no job_id.
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

    /// Run the render-boundary strip decision for one `--format` and report
    /// whether the declared `type` survived.
    fn declared_type_survives(format: &str) -> bool {
        use fastio_cli::output::OutputConfig;
        use std::path::Path;

        let output = OutputConfig::from_flags(Some(format), None, true, false);
        let ctx = CommandContext {
            output: &output,
            profile_name: "default",
            api_base: "http://127.0.0.1:1",
            flag_token: None,
            config_dir: Path::new("/nonexistent"),
        };
        // A field DECLARED `int` that actually holds `"abc"` — the exact
        // divergence the declared type cannot be trusted to describe.
        let mut value = json!({
            "node_id": {"id": "abc", "name": "invoice.pdf", "type": "file"},
            "template_metadata": [{"key": "count", "type": "int", "value": "abc"}]
        });
        strip_declared_types_for_humans(&ctx, &mut value);

        // Whatever the format, the STORED value and the storage node's kind
        // are relayed untouched.
        assert_eq!(value["template_metadata"][0]["value"], "abc", "{format}");
        assert_eq!(value["node_id"]["type"], "file", "{format}");

        value["template_metadata"][0].get("type").is_some()
    }

    #[test]
    fn json_keeps_declared_type_and_human_formats_drop_it() {
        // `--format json` is a machine-readable contract: dropping a field
        // there would be a breaking change for existing consumers.
        assert!(
            declared_type_survives("json"),
            "--format json must relay the declared type verbatim"
        );
        // Human formats render it as a column beside the value, which reads
        // as a claim about that value — so it goes.
        for human in ["table", "csv", "markdown", "md"] {
            assert!(
                !declared_type_survives(human),
                "--format {human} must not render the declared type"
            );
        }
    }

    #[test]
    fn bulk_json_envelope_keeps_declared_type() {
        // The test above only exercises `strip_declared_types_for_humans`.
        // `render_bulk_details`' JSON branch never calls that helper — it
        // returns early over the aggregate — so the bulk JSON contract needs
        // its own assertion on the envelope the branch actually emits.
        // Without this, stripping `agg.objects` before building the envelope
        // would break `--format json` with every other test still green.
        let chunk = make_chunk(&json!({
            "format": "multi",
            "objects": [{
                "node_id": {"id": "a", "type": "file"},
                "template_metadata": [{"key": "count", "type": "int", "value": "abc"}]
            }],
            "templates": {"tpl1": {"name": "Invoices"}},
            "errors": []
        }));
        let agg = aggregate_metadata_chunks(1, vec![chunk]);
        let envelope = bulk_details_json_envelope(&agg);

        assert_eq!(
            envelope["objects"][0]["template_metadata"][0]["type"], "int",
            "bulk --format json must relay the declared type verbatim"
        );
        assert_eq!(
            envelope["objects"][0]["template_metadata"][0]["value"],
            "abc"
        );
        assert_eq!(envelope["count_total"], 1);
        assert_eq!(envelope["count_succeeded"], 1);
        assert_eq!(envelope["count_errored"], 0);
    }
}
