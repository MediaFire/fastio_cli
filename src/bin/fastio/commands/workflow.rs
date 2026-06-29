//! Workflow Orchestration (v3.2) command handlers (`fastio workflow`, `wf`).
//!
//! Owner-side surface over [`fastio_cli::api::orchestration`]. These handlers
//! enforce the binding orchestration disciplines:
//!
//! - **Idempotency.** `instantiate` and `trigger fire` REQUIRE an
//!   `--idempotency-key`. The key may be auto-generated only via the explicit
//!   `--generate-idempotency-key` opt-in, which prints a LOUD stderr warning
//!   (silent auto-generation would destroy replay safety).
//! - **CAS.** Step `output` / `advance` are CAS-guarded; a 409 conflict is
//!   surfaced by default and only re-read-then-retried under
//!   `--retry-on-conflict`.
//! - **Secrets.** Outbound webhook secrets and the realtime token are wrapped
//!   in [`secrecy::SecretString`], never printed to stdout, and written to a
//!   0600 `--secret-file` / `--token-file` when requested.
//! - **Audit.** `audit check-integrity` runs the integrity portion of the
//!   verifier recipe (chunk SHA-256 + content-hash chain + completeness) over
//!   a locally-downloaded bundle; the HMAC authenticity `verify` is DEFERRED.
//!   The bundle download streams to disk via
//!   [`fastio_cli::client::ApiClient::download_file_stream`].
//! - **`@file` JSON.** Structurally-nested bodies (template body, event
//!   match, schemas, payloads) accept `@path` to read JSON from a file.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use fastio_cli::api::orchestration::{self, DownloadedChunk};
use fastio_cli::error::CliError;

use crate::cli::{
    WorkflowAgentTemplateCommands, WorkflowAuditCommands, WorkflowAuditExportCommands,
    WorkflowCommands, WorkflowGrantCommands, WorkflowInboxCommands, WorkflowModificationCommands,
    WorkflowObligationCommands, WorkflowOutboundCommands, WorkflowPoolCommands,
    WorkflowRealtimeCommands, WorkflowRedactionCommands, WorkflowReviewCommands,
    WorkflowSchemaCommands, WorkflowStepCommands, WorkflowSubjectCommands,
    WorkflowTemplateCommands, WorkflowTriggerAliasCommands, WorkflowTriggerCommands,
};

use super::CommandContext;
use super::secret_output::{extract_secret, redact_secret_field, write_secret_file};

/// Base seconds between `workflow wait` polls when unspecified.
const DEFAULT_WAIT_INTERVAL_SECS: u64 = 3;
/// Lower bound on the `workflow wait` poll interval.
const MIN_WAIT_INTERVAL_SECS: u64 = 1;
/// Upper bound on the `workflow wait` poll interval.
const MAX_WAIT_INTERVAL_SECS: u64 = 60;
/// Hard ceiling on the `workflow wait` poll loop. Sized well under the ~1-hour
/// JWT lifetime so a stuck runtime surfaces a clear timeout rather than
/// hanging indefinitely.
const WAIT_MAX_SECS: u64 = 600;

// ─── @file JSON resolution ──────────────────────────────────────────────────

/// Resolve a JSON-string argument, supporting an `@path` form that reads the
/// JSON from a file. A leading `@` selects file mode; `@@` is an escape for a
/// literal value beginning with `@`. The resolved text is validated as
/// well-formed JSON so a malformed body is rejected client-side before any
/// credit-spending or state-changing call.
fn resolve_json_arg(raw: &str, label: &str) -> Result<String> {
    let text = if let Some(path) = raw.strip_prefix('@') {
        if let Some(literal) = path.strip_prefix('@') {
            // `@@…` escapes a literal leading `@`.
            literal.to_owned()
        } else {
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {label} from file '{path}'"))?
        }
    } else {
        raw.to_owned()
    };
    serde_json::from_str::<Value>(&text).with_context(|| format!("{label} is not valid JSON"))?;
    Ok(text)
}

/// Resolve an optional JSON-string argument (see [`resolve_json_arg`]).
fn resolve_opt_json_arg(raw: Option<&str>, label: &str) -> Result<Option<String>> {
    match raw {
        Some(v) => Ok(Some(resolve_json_arg(v, label)?)),
        None => Ok(None),
    }
}

/// Resolve an optional JSON ARRAY argument into a [`Value`], supporting the
/// `@file.json` form (see [`resolve_json_arg`]) and rejecting a non-array
/// client-side. Used for the review send-for-review / quick-approval reviewer
/// rosters and asset lists.
fn resolve_opt_json_array_value(raw: Option<&str>, label: &str) -> Result<Option<Value>> {
    match raw {
        None => Ok(None),
        Some(r) => {
            let text = resolve_json_arg(r, label)?;
            let value: Value = serde_json::from_str(&text)
                .with_context(|| format!("{label} is not valid JSON"))?;
            if !value.is_array() {
                anyhow::bail!("{label} must be a JSON array");
            }
            Ok(Some(value))
        }
    }
}

/// Resolve an optional JSON OBJECT argument into a [`Value`], supporting the
/// `@file.json` form and rejecting a non-object client-side. Used for the
/// review-comment `--anchor-json` reference.
fn resolve_opt_json_object_value(raw: Option<&str>, label: &str) -> Result<Option<Value>> {
    match raw {
        None => Ok(None),
        Some(r) => {
            let text = resolve_json_arg(r, label)?;
            let value: Value = serde_json::from_str(&text)
                .with_context(|| format!("{label} is not valid JSON"))?;
            if !value.is_object() {
                anyhow::bail!("{label} must be a JSON object");
            }
            Ok(Some(value))
        }
    }
}

// ─── Idempotency-key gating ───────────────────────────────────────────────────

/// Resolve the idempotency key for `instantiate` / `trigger fire`.
///
/// An explicit `--idempotency-key` is always honored. When it is absent, the
/// caller MUST pass `--generate-idempotency-key` to opt into a random key —
/// which emits a LOUD stderr warning because a generated key cannot be replayed
/// safely. Without either, this is a hard error (never a silent auto-generate).
///
/// The generated-key warning is emitted **even under `--quiet`**: losing the
/// replay-safety caveat would let a silent retry start a duplicate run, so this
/// one diagnostic is deliberately not silenceable. `--quiet` still suppresses
/// ordinary progress chatter elsewhere.
fn resolve_idempotency_key(explicit: Option<&str>, generate: bool, _quiet: bool) -> Result<String> {
    if let Some(key) = explicit {
        let key = key.trim();
        anyhow::ensure!(!key.is_empty(), "--idempotency-key must not be blank");
        return Ok(key.to_owned());
    }
    if generate {
        let key = generate_idempotency_key()?;
        // Always emitted, regardless of --quiet: replay safety is too important
        // to silence (a suppressed warning + a retry = a duplicate run).
        eprintln!(
            "WARNING: auto-generated idempotency key '{key}'. This breaks replay safety — \
             a retry of this command will start a SECOND run instead of returning the \
             existing one. Pass an explicit, caller-stable --idempotency-key for \
             replay-safe behavior. (This warning is shown even under --quiet.)"
        );
        return Ok(key);
    }
    anyhow::bail!(
        "--idempotency-key is required for replay-safe instantiation. Provide a stable key, \
         or pass --generate-idempotency-key to generate one (NOT replay-safe)."
    )
}

/// Generate a random idempotency key (32 hex chars) via the CSPRNG.
fn generate_idempotency_key() -> Result<String> {
    use std::fmt::Write as _;
    let mut buf = [0u8; 16];
    getrandom_crate::getrandom(&mut buf)
        .map_err(|e| anyhow::anyhow!("failed to generate idempotency key: {e}"))?;
    let mut s = String::with_capacity(32);
    for b in buf {
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}

// ─── CAS-409 handling ─────────────────────────────────────────────────────────

/// Run a step-occurrence mutation, surfacing a CAS 409 conflict by default and
/// retrying once (after a fresh read) only when `retry_on_conflict` is set.
///
/// On a 409 with `--retry-on-conflict`, the re-read is **load-bearing**, not
/// best-effort:
///
/// - if the re-read itself fails, that error is surfaced (we do not blind-retry
///   against an endpoint that just failed);
/// - if the re-read shows the occurrence in a terminal / non-mutable `state`
///   (`completed`/`failed`/`skipped`/`cancelled`), the retry is abandoned and
///   the terminal state is surfaced — retrying would only 409 again;
/// - otherwise the occurrence is still mutable, so a single retry is attempted.
///
/// The orchestration step endpoints do **not** accept a client-supplied CAS
/// version/token (the CAS is enforced server-side on the row's internal state;
/// the only client-threaded version in this surface is `link_asset`'s
/// `version_id_pinned`, a different endpoint). So the fresh value cannot be
/// "threaded" into the retry body — the actionable use of the re-read is the
/// mutability gate above, which prevents the previous blind re-loop into a
/// guaranteed second 409.
async fn run_step_mutation_with_cas<F, Fut>(
    ctx: &CommandContext<'_>,
    client: &fastio_cli::client::ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
    retry_on_conflict: bool,
    op: F,
) -> Result<Value>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Value, fastio_cli::error::CliError>>,
{
    match op().await {
        Ok(v) => Ok(v),
        Err(fastio_cli::error::CliError::Api(e)) if e.http_status == 409 => {
            if !retry_on_conflict {
                return Err(
                    anyhow::Error::from(fastio_cli::error::CliError::Api(e)).context(
                        "step mutation hit a CAS conflict (409); the occurrence was modified \
                     concurrently. Re-read it and retry, or pass --retry-on-conflict to \
                     re-read and retry once automatically.",
                    ),
                );
            }
            if !ctx.output.quiet {
                eprintln!("CAS conflict (409); re-reading step occurrence and retrying once...");
            }
            // The re-read must succeed before we retry. If it fails, surface
            // that error rather than blind-retrying.
            let snapshot =
                orchestration::get_step_occurrence(client, workflow_id, step_occurrence_id)
                    .await
                    .context(
                        "CAS conflict (409): re-reading the step occurrence failed, so the \
                         retry was abandoned",
                    )?;
            // If the re-read shows a terminal/non-mutable state, retrying would
            // only 409 again — surface the terminal state instead.
            if let Some(state) = step_occurrence_state(&snapshot)
                && is_terminal_step_state(&state)
            {
                anyhow::bail!(
                    "CAS conflict (409): the step occurrence is now in terminal state '{state}' \
                     and can no longer be mutated; not retrying. Inspect it with \
                     'fastio workflow step get'."
                );
            }
            op().await.map_err(|e| {
                anyhow::Error::from(e)
                    .context("step mutation still conflicted after one retry (CAS 409)")
            })
        }
        Err(e) => Err(anyhow::Error::from(e).context("step mutation failed")),
    }
}

/// Read a step occurrence's lifecycle `state` from a get-occurrence snapshot.
///
/// Tolerates both the enveloped (`response.step_occurrence.state`) and flatter
/// shapes the endpoint may return.
fn step_occurrence_state(snapshot: &Value) -> Option<String> {
    let payload = snapshot.get("response").unwrap_or(snapshot);
    payload
        .get("step_occurrence")
        .and_then(|o| o.get("state"))
        .or_else(|| payload.get("state"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Whether a step-occurrence `state` is terminal (rejects further mutation).
///
/// Per `workflows.txt:155`, the terminal step states are `completed`,
/// `failed`, `skipped`, and `cancelled`.
fn is_terminal_step_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "skipped" | "cancelled")
}

// ─── Validation helpers ───────────────────────────────────────────────────────

/// Confirm a destructive `purge` by matching the re-stated id.
fn confirm_purge(id: &str, confirm: &str, what: &str) -> Result<()> {
    anyhow::ensure!(
        id == confirm,
        "purge not confirmed: --confirm '{confirm}' does not match the {what} id '{id}'"
    );
    Ok(())
}

/// Clamp a user-supplied wait interval into the supported range.
fn clamp_wait_interval(secs: Option<u64>) -> u64 {
    secs.unwrap_or(DEFAULT_WAIT_INTERVAL_SECS)
        .clamp(MIN_WAIT_INTERVAL_SECS, MAX_WAIT_INTERVAL_SECS)
}

/// Add bounded random jitter (0..interval/2 seconds) to a poll interval so
/// concurrent waiters do not synchronize their polls.
///
/// `pub(crate)` so the MCP wait/export poll loops reuse the SAME bounded,
/// CSPRNG-jittered backoff on transient-error retries rather than duplicating
/// the logic. A jitter failure falls back to no jitter (never panics), so the
/// returned duration is always `>= interval_secs` and `<= interval_secs * 1.5`.
pub(crate) fn jittered(interval_secs: u64) -> Duration {
    let span = interval_secs / 2;
    let extra_ms = if span == 0 {
        0
    } else {
        let mut buf = [0u8; 2];
        // A jitter failure is non-fatal: fall back to no jitter.
        if getrandom_crate::getrandom(&mut buf).is_ok() {
            u64::from(u16::from_le_bytes(buf)) % (span * 1000 + 1)
        } else {
            0
        }
    };
    Duration::from_secs(interval_secs) + Duration::from_millis(extra_ms)
}

/// How a poll loop should react to an error from one poll tick.
///
/// Distinguishes the three cases the previous `Err(_) => {}` collapsed into one
/// (silent loop-to-timeout):
/// - [`PollAction::RateLimited`] — honor the server's `retry_after`;
/// - [`PollAction::RetryTransient`] — a 5xx / network / I/O blip; back off and
///   retry on the next tick;
/// - [`PollAction::Fatal`] — a persistent, non-transient error (404 / 403 /
///   400 / parse / a non-rate-limit 4xx). Surface it instead of looping.
///
/// `pub(crate)` so the Ripley `ask`/`chat` and metadata `extract --wait` poll
/// loops (CLI + MCP) reuse the SAME classification rather than each
/// re-collapsing every error into a silent timeout.
pub(crate) enum PollAction {
    /// Server asked us to wait this many seconds before the next request.
    RateLimited { retry_after_secs: u64 },
    /// A transient failure worth one more poll on the regular cadence.
    RetryTransient,
    /// A persistent error the caller should see now (returned, not swallowed).
    Fatal(CliError),
}

/// Classify a poll-tick [`CliError`] into a [`PollAction`].
///
/// The 401 re-auth short-circuit is handled by the caller before this is
/// reached. Rate limits sleep their advertised interval; all 5xx (`500..=599`),
/// request timeouts, transport, and I/O errors are transient; everything else
/// (4xx other than 408/429, parse, config) is fatal so a 404/403 no longer
/// loops silently to the deadline.
///
/// `pub(crate)` so the Ripley/metadata wait loops share this exact policy.
pub(crate) fn classify_poll_error(err: CliError) -> PollAction {
    match err {
        CliError::RateLimit { retry_after_secs } => PollAction::RateLimited { retry_after_secs },
        CliError::Api(ref e) => match e.http_status {
            429 | 408 => PollAction::RateLimited {
                retry_after_secs: 0,
            },
            // All server errors are transient — a 500 during a long-running
            // workflow poll is typically a momentary backend blip, not a
            // permanent condition, so it's worth another tick.
            500..=599 => PollAction::RetryTransient,
            _ => PollAction::Fatal(err),
        },
        // Transport/timeout and transient I/O are worth another tick.
        CliError::Http(_) | CliError::Io(_) => PollAction::RetryTransient,
        // Parse / config / auth(other) — and, conservatively, any future
        // non-exhaustive variant — are surfaced rather than looped.
        _ => PollAction::Fatal(err),
    }
}

/// Read a workflow's lifecycle state from a `state` snapshot, if present.
fn workflow_state(snapshot: &Value) -> Option<String> {
    let payload = snapshot.get("response").unwrap_or(snapshot);
    payload
        .get("workflow")
        .and_then(|w| w.get("state"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Whether a lifecycle state is terminal (stop polling).
fn is_terminal_state(state: &str) -> bool {
    matches!(state, "completed" | "cancelled" | "archived" | "deleted")
}

/// Read the run's lifecycle `status` from a state snapshot.
///
/// Prefers `run_summary.status` (the digest that additionally carries
/// `not_started` for a never-instantiated workflow), falling back to
/// `runtime.status`. Both are the runtime lifecycle enum
/// `queued`/`running`/`completed`/`failed`/`cancelled`.
///
/// Distinct from [`workflow_state`], which reads the PROFILE lifecycle
/// (`workflow.state`: `active`/`completed`/`cancelled`/…). This finer run
/// `status` is what tells an approval-rejected *completion* (`completed`) from a
/// `failed` run — the two cases the terminal notice disambiguates.
fn run_status(payload: &Value) -> Option<&str> {
    payload
        .get("run_summary")
        .and_then(|r| r.get("status"))
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("runtime")
                .and_then(|r| r.get("status"))
                .and_then(Value::as_str)
        })
}

/// Whether a run `status` is terminal (the run has stopped). Per the
/// `GET /state/` wire shape the terminal runtime statuses are `completed`,
/// `failed`, and `cancelled` (`queued`/`running`/`not_started` are in-flight).
fn is_terminal_run_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

/// Whether a `wait` poll loop should stop on this state snapshot.
///
/// Breaks when EITHER the RUN `status` is terminal (`completed`/`failed`/
/// `cancelled`) OR the PROFILE lifecycle `workflow.state` is terminal. The run
/// status is the PRIMARY signal: a FAILED run sets `run_summary`/`runtime`
/// status=`failed` while the profile `workflow.state` stays `active` (the
/// profile lifecycle enum has no `failed`, and the backend does not auto-mirror
/// a run failure onto it). A profile-only predicate would therefore never break
/// on a failed run, so `wait` would hang to its `WAIT_MAX_SECS` timeout AND the
/// run-FAILED / native-review notice would never fire. The profile predicate is
/// retained as the fallback that additionally covers `archived` / `deleted`,
/// which have no run-status counterpart.
///
/// Peeling is consistent with the existing readers: [`run_status`] takes the
/// `response`-peeled payload (mirroring [`emit_terminal_notice`]), while
/// [`workflow_state`] peels the envelope internally.
fn wait_is_terminal(snapshot: &Value) -> bool {
    let payload = snapshot.get("response").unwrap_or(snapshot);
    run_status(payload).is_some_and(is_terminal_run_status)
        || workflow_state(snapshot).is_some_and(|s| is_terminal_state(&s))
}

/// A human-facing label for a step occurrence: its display `step_name`, falling
/// back to the raw `step_id` (then a generic `step`).
fn step_label(step: &Value) -> String {
    step.get("step_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| step.get("step_id").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .unwrap_or("step")
        .to_owned()
}

/// Find the failure reason of a `failed` run from its step occurrences.
///
/// Per the `GET /state/` wire shape a failed run carries NO run-level reason
/// string: the cause lives ONLY as a structured `failure_reason: {code, message}`
/// object on the step occurrence that failed (e.g.
/// `native_review_tier_not_provisioned` on a native `approval` step that could
/// not run). Walks `recent_steps[]` first, then `active_steps[]` defensively,
/// for the LAST occurrence whose `state` is `"failed"` or `"skipped"` (a
/// natural-language-gate skip rides the same field) carrying a `failure_reason`
/// object, returning `(step label, failure_reason.code)`.
fn failed_step_reason(payload: &Value) -> Option<(String, String)> {
    ["recent_steps", "active_steps"].iter().find_map(|key| {
        let steps = payload.get(*key).and_then(Value::as_array)?;
        // Prefer the LAST terminal step that carries a reason (a run typically
        // fails on one step, and the failing step is the tail of the feed).
        steps.iter().rev().find_map(|step| {
            let state = step.get("state").and_then(Value::as_str)?;
            if state != "failed" && state != "skipped" {
                return None;
            }
            let code = step
                .get("failure_reason")
                .and_then(|fr| fr.get("code"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())?;
            Some((step_label(step), code.to_owned()))
        })
    })
}

/// Whether a step `output` envelope carries the backend rejected-outcome token
/// (`"rejected"`). Backend-confirmed: BOTH a resolved manual `approval` step and
/// an `approval_in_place` step write the terminal occurrence output via the
/// shared `ApprovalStepHandler::completedResult()` (`approval_in_place` delegates
/// resolution to a composed `ApprovalStepHandler`), stamping
/// `output.outcome = "approved" | "rejected" | "cancelled"` — so `outcome` is the
/// real key for both. `decision` / `resolved_outcome` are retained as defensive
/// fallbacks for older / alternate native-surface shapes. Matched
/// case-insensitively across those keys.
fn output_is_rejected(output: Option<&Value>) -> bool {
    let Some(output) = output.and_then(Value::as_object) else {
        return false;
    };
    ["decision", "resolved_outcome", "outcome"]
        .iter()
        .filter_map(|k| output.get(*k))
        .filter_map(Value::as_str)
        .any(|v| v.eq_ignore_ascii_case("rejected"))
}

/// Detect, from state alone, that a `completed` run ended on an approval
/// REJECTION.
///
/// The clean `terminal_reason: "approval_rejected"` token is event-only
/// (`workflow.completed`) and is absent from `GET /state/`; an approval-rejected
/// run reports `status: "completed"` with no first-class flag. This is the
/// state-only heuristic: scan `recent_steps[]` (then `active_steps[]`) for an
/// `approval` / `approval_in_place` step in a terminal state whose `output`
/// carries the rejected token. Returns the step label when found.
fn rejected_approval_step(payload: &Value) -> Option<String> {
    ["recent_steps", "active_steps"].iter().find_map(|key| {
        let steps = payload.get(*key).and_then(Value::as_array)?;
        steps.iter().rev().find_map(|step| {
            let step_type = step.get("step_type").and_then(Value::as_str)?;
            if step_type != "approval" && step_type != "approval_in_place" {
                return None;
            }
            let state = step.get("state").and_then(Value::as_str)?;
            if !is_terminal_step_state(state) {
                return None;
            }
            output_is_rejected(step.get("output")).then(|| step_label(step))
        })
    })
}

/// Extract a human-facing completion-reason notice from a TERMINAL state
/// snapshot, or `None` when the run completed plainly (nothing to flag) or is
/// not yet terminal.
///
/// Reads strictly from the real `GET /state/` wire shape (there is NO top-level
/// `terminal_reason` and NO run-level reason string — see
/// `temp/planexecute-cli-api-update-2026-06-23/c4-wire-shape.md`):
///
/// - **Run status** comes from [`run_status`] (`run_summary.status` else
///   `runtime.status`). A non-terminal status emits nothing.
/// - **`failed`** — the cause is per-step: surface `failure_reason.code` from the
///   failing step occurrence (e.g. `native_review_tier_not_provisioned`) via
///   [`failed_step_reason`]; a failure with no per-step reason still flags the
///   failure generically.
/// - **`completed`** — a clean success emits nothing. An approval-rejected
///   completion has no run-level flag in state, so it is detected heuristically
///   from a rejected approval step occurrence ([`rejected_approval_step`]); a
///   fake `terminal_reason` is never synthesized for a normal success.
/// - **`cancelled`** — flagged; the snapshot carries no cancellation reason.
fn terminal_completion_notice(payload: &Value) -> Option<String> {
    let status = run_status(payload)?;
    if !is_terminal_run_status(status) {
        return None;
    }
    match status {
        "failed" => Some(match failed_step_reason(payload) {
            Some((step, code)) => format!("run FAILED: {step} — {code}"),
            None => "run FAILED.".to_owned(),
        }),
        "completed" => rejected_approval_step(payload)
            .map(|_step| "run completed via approval REJECTION (not a normal success).".to_owned()),
        "cancelled" => Some("run cancelled.".to_owned()),
        _ => None,
    }
}

/// Decide the terminal completion-reason notice to emit for a state snapshot,
/// applying every gate of [`emit_terminal_notice`] EXCEPT the final print.
///
/// - Suppressed under `quiet` (returns `None`).
/// - Peels the `response` envelope wrapper (mirroring [`workflow_state`]) so the
///   signal fields are found wherever the snapshot carries them.
/// - Gated strictly on a TERMINAL run status. `workflow state` calls the emitter
///   on EVERY snapshot — including mid-flight reads (`running`/`queued`) — so a
///   terminal completion notice must never fire on a still-running run, even if
///   a stray `recent_steps[]` row looks terminal. (`terminal_completion_notice`
///   self-gates on the same condition; this makes the boundary explicit.)
///
/// Returns the notice TEXT that would be printed, or `None` when nothing should
/// be emitted. Kept separate from the [`eprintln!`] side effect so the
/// quiet/peel/terminal-gate decision contract is directly unit-testable.
fn terminal_notice_for(quiet: bool, snapshot: &Value) -> Option<String> {
    if quiet {
        return None;
    }
    let payload = snapshot.get("response").unwrap_or(snapshot);
    if !run_status(payload).is_some_and(is_terminal_run_status) {
        return None;
    }
    terminal_completion_notice(payload)
}

/// Print the terminal completion-reason notice (if any) to stderr.
///
/// Delegates the decision (quiet gate, `response`-envelope peel, terminal-status
/// gate) to [`terminal_notice_for`], then emits the result to STDERR — never
/// stdout — so structured output (especially `--format json`) on stdout stays
/// byte-clean and machine-parseable.
fn emit_terminal_notice(ctx: &CommandContext<'_>, snapshot: &Value) {
    if let Some(notice) = terminal_notice_for(ctx.output.quiet, snapshot) {
        eprintln!("notice: {notice}");
    }
}

// ─── Dispatch ─────────────────────────────────────────────────────────────────

/// Execute a `fastio workflow` command.
#[allow(clippy::too_many_lines)] // a flat dispatch over the full orchestration surface
pub async fn execute(command: WorkflowCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        WorkflowCommands::Create {
            workspace_id,
            name,
            description,
            template_id,
            definition,
            agent_credit_cap,
            visibility,
        } => {
            // `definition` and `template_id` are mutually exclusive (enforced by
            // clap `conflicts_with`); validate the inline definition is JSON
            // client-side before the create call.
            let definition = resolve_opt_json_arg(definition.as_deref(), "definition")?;
            let client = ctx.build_client()?;
            let params = orchestration::CreateWorkflowParams::new()
                .name(name)
                .description(description)
                .template_id(template_id)
                .definition(definition)
                .agent_credit_cap(agent_credit_cap)
                .visibility(visibility);
            let v = orchestration::create_workflow(&client, &workspace_id, &params)
                .await
                .context("failed to create workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::List {
            workspace_id,
            limit,
            offset,
            template_id,
            state,
            archived,
            created_by_me,
            participant_me,
            include,
            page_size,
            cursor,
            bucket,
        } => {
            let client = ctx.build_client()?;
            let params = orchestration::ListWorkflowsParams::new()
                .limit(limit)
                .offset(offset)
                .template_id(template_id)
                .state(state)
                .archived(archived)
                .created_by_me(created_by_me)
                .participant_me(participant_me)
                .include(include)
                .page_size(page_size)
                .cursor(cursor)
                .bucket(bucket);
            let v = orchestration::list_workflows(&client, &workspace_id, &params)
                .await
                .context("failed to list workflows")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Get { workflow_id } => {
            let client = ctx.build_client()?;
            let v = orchestration::get_workflow(&client, &workflow_id)
                .await
                .context("failed to get workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Update {
            workflow_id,
            name,
            description,
            state,
            agent_credit_cap,
        } => {
            let mut fields = HashMap::new();
            if let Some(n) = name {
                fields.insert("name".to_owned(), n);
            }
            if let Some(d) = description {
                fields.insert("description".to_owned(), d);
            }
            if let Some(s) = state {
                fields.insert("state".to_owned(), s);
            }
            if let Some(c) = agent_credit_cap {
                fields.insert("agent_credit_cap".to_owned(), c.to_string());
            }
            anyhow::ensure!(!fields.is_empty(), "no fields to update were supplied");
            let client = ctx.build_client()?;
            let v = orchestration::update_workflow(&client, &workflow_id, &fields)
                .await
                .context("failed to update workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Delete { workflow_id } => {
            let client = ctx.build_client()?;
            let v = orchestration::delete_workflow(&client, &workflow_id, false)
                .await
                .context("failed to archive workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Purge {
            workflow_id,
            confirm,
        } => {
            confirm_purge(&workflow_id, &confirm, "workflow")?;
            let client = ctx.build_client()?;
            let v = orchestration::delete_workflow(&client, &workflow_id, true)
                .await
                .context("failed to purge workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Transfer {
            workflow_id,
            to_workspace,
        } => {
            let client = ctx.build_client()?;
            let v = orchestration::transfer_workflow(&client, &workflow_id, &to_workspace)
                .await
                .context("failed to transfer workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Instantiate {
            workflow_id,
            idempotency_key,
            generate_idempotency_key,
            trigger_payload,
            external_subject_id,
            pool_key,
            step_seeds,
        } => {
            let key = resolve_idempotency_key(
                idempotency_key.as_deref(),
                generate_idempotency_key,
                ctx.output.quiet,
            )?;
            let payload = resolve_opt_json_arg(trigger_payload.as_deref(), "trigger payload")?;
            let step_seeds = resolve_opt_json_arg(step_seeds.as_deref(), "step seeds")?;
            let client = ctx.build_client()?;
            let params = orchestration::InstantiateParams::new(key)
                .trigger_payload(payload)
                .external_subject_id(external_subject_id)
                .pool_key(pool_key)
                .step_seeds(step_seeds);
            let v = orchestration::instantiate_workflow(&client, &workflow_id, &params)
                .await
                .context("failed to instantiate workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::State { workflow_id } => {
            let client = ctx.build_client()?;
            let v = orchestration::get_workflow_state(&client, &workflow_id)
                .await
                .context("failed to get workflow state")?;
            // Flag a notable terminal reason (approval-rejected completion,
            // native-review fail-closed) for a run that is already terminal, so a
            // direct `state` read surfaces WHY it ended, not just the raw fields.
            emit_terminal_notice(ctx, &v);
            // The state snapshot is a single rich object, not a list; render it
            // faithfully so table/CSV don't collapse to an empty `active_steps`.
            ctx.output.render_state_snapshot(&v)?;
            Ok(())
        }
        WorkflowCommands::Wait {
            workflow_id,
            poll_interval,
        } => wait_for_workflow(ctx, &workflow_id, poll_interval).await,
        WorkflowCommands::Pause { workflow_id } => {
            let client = ctx.build_client()?;
            let v = orchestration::pause_workflow(&client, &workflow_id)
                .await
                .context("failed to pause workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Resume { workflow_id } => {
            let client = ctx.build_client()?;
            let v = orchestration::resume_workflow(&client, &workflow_id)
                .await
                .context("failed to resume workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Cancel {
            workflow_id,
            reason,
        } => {
            let client = ctx.build_client()?;
            let v = orchestration::cancel_workflow(&client, &workflow_id, reason.as_deref())
                .await
                .context("failed to cancel workflow")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::RotateInboundKey { workflow_id } => {
            let client = ctx.build_client()?;
            let v = orchestration::rotate_workflow_inbound_key(&client, &workflow_id)
                .await
                .context("failed to rotate inbound key")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowCommands::Grant(c) => execute_grant(c, ctx).await,
        WorkflowCommands::Step(c) => execute_step(c, ctx).await,
        WorkflowCommands::Modification(c) => execute_modification(c, ctx).await,
        WorkflowCommands::Template(c) => execute_template(c, ctx).await,
        WorkflowCommands::AgentTemplate(c) => execute_agent_template(c, ctx).await,
        WorkflowCommands::Trigger(c) => execute_trigger(c, ctx).await,
        WorkflowCommands::TriggerAlias(c) => execute_trigger_alias(c, ctx).await,
        WorkflowCommands::Obligation(c) => execute_obligation(c, ctx).await,
        WorkflowCommands::Inbox(c) => execute_inbox(c, ctx).await,
        WorkflowCommands::Schema(c) => execute_schema(c, ctx).await,
        WorkflowCommands::Audit(c) => execute_audit(c, ctx).await,
        WorkflowCommands::Outbound(c) => execute_outbound(c, ctx).await,
        WorkflowCommands::Pool(c) => execute_pool(c, ctx).await,
        WorkflowCommands::Subject(c) => execute_subject(c, ctx).await,
        WorkflowCommands::Realtime(c) => execute_realtime(c, ctx).await,
        WorkflowCommands::Review(c) => execute_review(c, ctx).await,
    }
}

/// Poll runtime state until terminal (bounded backoff + jitter).
async fn wait_for_workflow(
    ctx: &CommandContext<'_>,
    workflow_id: &str,
    poll_interval: Option<u64>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let interval = clamp_wait_interval(poll_interval);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(WAIT_MAX_SECS);
    if !ctx.output.quiet {
        eprintln!("waiting for workflow {workflow_id} (polling ~every {interval}s)...");
    }
    loop {
        // Re-check the deadline at the TOP of every iteration, before issuing
        // the next state GET. The sleep at the bottom is clamped to the
        // remaining wait (and a 429 clamp can land exactly on the deadline);
        // without this check a woken iteration would issue one more request
        // that could add the client's request timeout and overrun WAIT_MAX_SECS.
        // Mirrors the MCP `workflow_wait_for_terminal` top-of-loop check.
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after ~{WAIT_MAX_SECS}s waiting for workflow {workflow_id} to reach a \
                 terminal state. It may still be running; poll \
                 'fastio workflow state {workflow_id}'."
            );
        }
        // The sleep applied at the bottom of this tick; rate-limit responses
        // override the default jittered cadence.
        let mut next_sleep = jittered(interval);
        match orchestration::get_workflow_state(&client, workflow_id).await {
            Ok(snapshot) => {
                if wait_is_terminal(&snapshot) {
                    // Surface WHY the run ended (run FAILED + per-step reason,
                    // approval-rejected completion, native-review fail-closed,
                    // or another terminal reason) BEFORE the snapshot render, so
                    // a non-JSON consumer is not left treating a failed or
                    // `approval_rejected` completion as a plain success. A FAILED
                    // run breaks here via the run-status predicate even though
                    // its profile `workflow.state` is still `active`. The
                    // structured snapshot still renders in full.
                    emit_terminal_notice(ctx, &snapshot);
                    // Faithful object render (see `State` handler): the terminal
                    // snapshot's `active_steps` is empty, so flattening would
                    // emit an empty table.
                    ctx.output.render_state_snapshot(&snapshot)?;
                    return Ok(());
                }
            }
            Err(CliError::Api(e)) if e.http_status == 401 => {
                anyhow::bail!(
                    "authentication expired while waiting for workflow {workflow_id}. \
                     Re-authenticate (fastio auth login) and poll \
                     'fastio workflow state {workflow_id}'."
                );
            }
            Err(other) => match classify_poll_error(other) {
                PollAction::RateLimited { retry_after_secs } => {
                    // Honor the server's retry-after, clamped into the poll
                    // bounds (and to the deadline below).
                    if retry_after_secs > 0 {
                        next_sleep = Duration::from_secs(
                            retry_after_secs.clamp(MIN_WAIT_INTERVAL_SECS, MAX_WAIT_INTERVAL_SECS),
                        );
                    }
                }
                PollAction::RetryTransient => {}
                // A persistent, non-transient error: surface it instead of
                // looping silently to the timeout.
                PollAction::Fatal(e) => {
                    return Err(anyhow::Error::from(e)
                        .context(format!("error while waiting for workflow {workflow_id}")));
                }
            },
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after ~{WAIT_MAX_SECS}s waiting for workflow {workflow_id} to reach a \
                 terminal state. It may still be running; poll \
                 'fastio workflow state {workflow_id}'."
            );
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(remaining.min(next_sleep)).await;
    }
}

// ─── Grants ───────────────────────────────────────────────────────────────────

async fn execute_grant(command: WorkflowGrantCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowGrantCommands::Add {
            workflow_id,
            user_id,
            role,
            expires_at,
        } => orchestration::add_grant(
            &client,
            &workflow_id,
            &user_id,
            &role,
            expires_at.as_deref(),
        )
        .await
        .context("failed to add grant")?,
        WorkflowGrantCommands::List {
            workflow_id,
            limit,
            cursor,
        } => orchestration::list_grants(&client, &workflow_id, limit, cursor.as_deref())
            .await
            .context("failed to list grants")?,
        WorkflowGrantCommands::Revoke {
            workflow_id,
            user_id,
        } => orchestration::revoke_grant(&client, &workflow_id, &user_id)
            .await
            .context("failed to revoke grant")?,
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Steps ─────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)] // flat dispatch over the step-occurrence sub-surface
async fn execute_step(command: WorkflowStepCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        WorkflowStepCommands::Get {
            workflow_id,
            step_occurrence_id,
        } => {
            let v = orchestration::get_step_occurrence(&client, &workflow_id, &step_occurrence_id)
                .await
                .context("failed to get step occurrence")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::Advance {
            workflow_id,
            step_occurrence_id,
            output,
            retry_on_conflict,
        } => {
            let output = resolve_opt_json_arg(output.as_deref(), "step output")?;
            let v = run_step_mutation_with_cas(
                ctx,
                &client,
                &workflow_id,
                &step_occurrence_id,
                retry_on_conflict,
                || {
                    orchestration::advance_step(
                        &client,
                        &workflow_id,
                        &step_occurrence_id,
                        output.as_deref(),
                    )
                },
            )
            .await?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::Cancel {
            workflow_id,
            step_occurrence_id,
        } => {
            let v = orchestration::cancel_step(&client, &workflow_id, &step_occurrence_id)
                .await
                .context("failed to cancel step occurrence")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::Output {
            workflow_id,
            step_occurrence_id,
            output,
            retry_on_conflict,
        } => {
            let output = resolve_json_arg(&output, "step output")?;
            let v = run_step_mutation_with_cas(
                ctx,
                &client,
                &workflow_id,
                &step_occurrence_id,
                retry_on_conflict,
                || {
                    orchestration::submit_step_output(
                        &client,
                        &workflow_id,
                        &step_occurrence_id,
                        &output,
                    )
                },
            )
            .await?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::Occurrences {
            workflow_id,
            step_id,
            limit,
            offset,
        } => {
            let v = orchestration::list_step_occurrences(
                &client,
                &workflow_id,
                &step_id,
                limit,
                offset,
            )
            .await
            .context("failed to list step occurrences")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::AgentActivity {
            workflow_id,
            step_occurrence_id,
        } => {
            let v =
                orchestration::get_step_agent_activity(&client, &workflow_id, &step_occurrence_id)
                    .await
                    .context("failed to read step agent activity")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::AgentTrace {
            workflow_id,
            step_occurrence_id,
        } => {
            let v = orchestration::get_step_agent_trace(&client, &workflow_id, &step_occurrence_id)
                .await
                .context("failed to read step agent trace")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::Files {
            workflow_id,
            step_occurrence_id,
            node_ids,
        } => {
            anyhow::ensure!(
                !node_ids.is_empty(),
                "at least one --node-ids value is required"
            );
            let v = orchestration::submit_step_files(
                &client,
                &workflow_id,
                &step_occurrence_id,
                &node_ids,
            )
            .await
            .context("failed to provide files to step")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::Complete {
            workflow_id,
            step_occurrence_id,
        } => {
            let v = orchestration::complete_step(&client, &workflow_id, &step_occurrence_id)
                .await
                .context("failed to complete step")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowStepCommands::Reassign {
            workflow_id,
            step_occurrence_id,
            new_assignee_user_id,
        } => {
            let v = orchestration::reassign_step(
                &client,
                &workflow_id,
                &step_occurrence_id,
                &new_assignee_user_id,
            )
            .await
            .context("failed to reassign step")?;
            ctx.output.render(&v)?;
            Ok(())
        }
    }
}

// ─── Mid-Run Modifications ────────────────────────────────────────────────────────

async fn execute_modification(
    command: WorkflowModificationCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowModificationCommands::Propose {
            workflow_id,
            ops,
            reason,
            expires_in_seconds,
        } => {
            let ops = resolve_json_arg(&ops, "ops")?;
            orchestration::propose_modification(
                &client,
                &workflow_id,
                &ops,
                reason.as_deref(),
                expires_in_seconds,
            )
            .await
            .context("failed to propose modification")?
        }
        WorkflowModificationCommands::List {
            workflow_id,
            status,
        } => orchestration::list_modifications(&client, &workflow_id, status.as_deref())
            .await
            .context("failed to list modifications")?,
        WorkflowModificationCommands::Get {
            workflow_id,
            modification_id,
        } => orchestration::get_modification(&client, &workflow_id, &modification_id)
            .await
            .context("failed to get modification")?,
        WorkflowModificationCommands::Apply {
            workflow_id,
            modification_id,
            apply_change_ids,
            confirm_removes_human_gate,
        } => {
            let apply_change_ids =
                resolve_opt_json_arg(apply_change_ids.as_deref(), "apply change ids")?;
            orchestration::apply_modification(
                &client,
                &workflow_id,
                &modification_id,
                apply_change_ids.as_deref(),
                confirm_removes_human_gate,
            )
            .await
            .context("failed to apply modification")?
        }
        WorkflowModificationCommands::Cancel {
            workflow_id,
            modification_id,
        } => orchestration::cancel_modification(&client, &workflow_id, &modification_id)
            .await
            .context("failed to cancel modification")?,
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Templates ──────────────────────────────────────────────────────────────────

async fn execute_template(
    command: WorkflowTemplateCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowTemplateCommands::Create {
            workspace_id,
            template_body,
            name,
        } => {
            let body = resolve_json_arg(&template_body, "template body")?;
            orchestration::create_template(&client, &workspace_id, &body, name.as_deref())
                .await
                .context("failed to create template revision")?
        }
        WorkflowTemplateCommands::List {
            workspace_id,
            limit,
            offset,
            usage,
        } => orchestration::list_templates(&client, &workspace_id, limit, offset, usage.as_deref())
            .await
            .context("failed to list templates")?,
        WorkflowTemplateCommands::Get {
            template_id,
            include_body,
        } => orchestration::get_template(&client, &template_id, include_body)
            .await
            .context("failed to get template")?,
        WorkflowTemplateCommands::Publish { template_id } => {
            orchestration::publish_template(&client, &template_id)
                .await
                .context("failed to publish template")?
        }
        WorkflowTemplateCommands::Withdraw { template_id } => {
            orchestration::withdraw_template(&client, &template_id)
                .await
                .context("failed to withdraw template")?
        }
        WorkflowTemplateCommands::Deprecate { template_id } => {
            orchestration::deprecate_template(&client, &template_id)
                .await
                .context("failed to deprecate template")?
        }
        WorkflowTemplateCommands::Gallery => orchestration::list_system_templates(&client)
            .await
            .context("failed to list the system template gallery")?,
        WorkflowTemplateCommands::GalleryGet { handle } => {
            orchestration::get_system_template(&client, &handle)
                .await
                .context("failed to get gallery template")?
        }
        WorkflowTemplateCommands::FromSystem {
            workspace_id,
            handle,
            workflow_id,
            create_workflow,
            name,
            description,
            inputs,
            expected_version,
            idempotency_key,
            publish,
            reason,
        } => {
            let inputs = resolve_opt_json_arg(inputs.as_deref(), "inputs")?;
            orchestration::instantiate_system_template(
                &client,
                &orchestration::FromSystemParams {
                    workspace_id: &workspace_id,
                    handle: &handle,
                    workflow_id: workflow_id.as_deref(),
                    create_workflow,
                    name: name.as_deref(),
                    description: description.as_deref(),
                    inputs: inputs.as_deref(),
                    expected_version,
                    idempotency_key: idempotency_key.as_deref(),
                    publish,
                    reason: reason.as_deref(),
                },
            )
            .await
            .context("failed to instantiate gallery template")?
        }
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Agent Templates ──────────────────────────────────────────────────────────────

async fn execute_agent_template(
    command: WorkflowAgentTemplateCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowAgentTemplateCommands::Create {
            workspace_id,
            name,
            instruction_prompt,
            tool_allowlist,
        } => {
            let tool_allowlist = resolve_opt_json_arg(tool_allowlist.as_deref(), "tool allowlist")?;
            orchestration::create_agent_template(
                &client,
                &workspace_id,
                &name,
                &instruction_prompt,
                tool_allowlist.as_deref(),
            )
            .await
            .context("failed to create agent template")?
        }
        WorkflowAgentTemplateCommands::List { workspace_id } => {
            orchestration::list_agent_templates(&client, &workspace_id)
                .await
                .context("failed to list agent templates")?
        }
        WorkflowAgentTemplateCommands::Get {
            workspace_id,
            template_id,
        } => orchestration::get_agent_template(&client, &workspace_id, &template_id)
            .await
            .context("failed to get agent template")?,
        WorkflowAgentTemplateCommands::Update {
            workspace_id,
            template_id,
            name,
            instruction_prompt,
            tool_allowlist,
        } => {
            if name.is_none() && instruction_prompt.is_none() && tool_allowlist.is_none() {
                anyhow::bail!(
                    "at least one update field is required (--name, --instruction-prompt, --tool-allowlist)"
                );
            }
            let tool_allowlist = resolve_opt_json_arg(tool_allowlist.as_deref(), "tool allowlist")?;
            orchestration::update_agent_template(
                &client,
                &workspace_id,
                &template_id,
                name.as_deref(),
                instruction_prompt.as_deref(),
                tool_allowlist.as_deref(),
            )
            .await
            .context("failed to update agent template")?
        }
        WorkflowAgentTemplateCommands::Delete {
            workspace_id,
            template_id,
        } => orchestration::delete_agent_template(&client, &workspace_id, &template_id)
            .await
            .context("failed to delete agent template")?,
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Triggers ───────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)] // flat dispatch over the trigger sub-surface
async fn execute_trigger(command: WorkflowTriggerCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        WorkflowTriggerCommands::Create {
            workspace_id,
            kind,
            target_template_id,
            event_match,
            param_mapping,
            rate_limit_per_hour,
            concurrency_cap,
            dedup_scope,
            idempotency_key_template,
        } => {
            let params = orchestration::CreateTriggerParams::new()
                .kind(kind)
                .target_template_id(target_template_id)
                .event_match(resolve_opt_json_arg(event_match.as_deref(), "event match")?)
                .param_mapping(resolve_opt_json_arg(
                    param_mapping.as_deref(),
                    "param mapping",
                )?)
                .rate_limit_per_hour(rate_limit_per_hour)
                .concurrency_cap(concurrency_cap)
                .dedup_scope(dedup_scope)
                .idempotency_key_template(idempotency_key_template);
            let v = orchestration::create_trigger(&client, &workspace_id, &params)
                .await
                .context("failed to create trigger")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::List {
            workspace_id,
            enabled_filter,
        } => {
            let v = orchestration::list_triggers(&client, &workspace_id, enabled_filter.as_deref())
                .await
                .context("failed to list triggers")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::Get { trigger_id } => {
            let v = orchestration::get_trigger(&client, &trigger_id)
                .await
                .context("failed to get trigger")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::Update {
            trigger_id,
            enabled,
            rate_limit_per_hour,
            concurrency_cap,
        } => {
            let mut fields = HashMap::new();
            if let Some(e) = enabled {
                fields.insert("enabled".to_owned(), e.to_string());
            }
            if let Some(r) = rate_limit_per_hour {
                fields.insert("rate_limit_per_hour".to_owned(), r.to_string());
            }
            if let Some(c) = concurrency_cap {
                fields.insert("concurrency_cap".to_owned(), c.to_string());
            }
            anyhow::ensure!(!fields.is_empty(), "no fields to update were supplied");
            let v = orchestration::update_trigger(&client, &trigger_id, &fields)
                .await
                .context("failed to update trigger")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::Fire {
            trigger_id,
            idempotency_key,
            generate_idempotency_key,
            trigger_payload,
        } => {
            let key = resolve_idempotency_key(
                idempotency_key.as_deref(),
                generate_idempotency_key,
                ctx.output.quiet,
            )?;
            let payload = resolve_opt_json_arg(trigger_payload.as_deref(), "trigger payload")?;
            let v = orchestration::fire_trigger(&client, &trigger_id, &key, payload.as_deref())
                .await
                .context("failed to fire trigger")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::DryRun {
            trigger_id,
            window_days,
            sample_limit,
            apply_guards,
        } => {
            let v = orchestration::dry_run_trigger(
                &client,
                &trigger_id,
                window_days,
                sample_limit,
                apply_guards,
            )
            .await
            .context("failed to dry-run trigger")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::DryRunDraft {
            workspace_id,
            event_match,
            param_mapping,
            target_template_id,
            window_days,
            sample_limit,
        } => {
            let em = resolve_opt_json_arg(event_match.as_deref(), "event match")?;
            let pm = resolve_opt_json_arg(param_mapping.as_deref(), "param mapping")?;
            let v = orchestration::dry_run_trigger_draft(
                &client,
                &workspace_id,
                em.as_deref(),
                pm.as_deref(),
                target_template_id.as_deref(),
                window_days,
                sample_limit,
            )
            .await
            .context("failed to dry-run trigger draft")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::Delete { trigger_id, hard } => {
            let v = orchestration::delete_trigger(&client, &trigger_id, hard)
                .await
                .context("failed to delete trigger")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::Purge {
            trigger_id,
            confirm,
        } => {
            confirm_purge(&trigger_id, &confirm, "trigger")?;
            let v = orchestration::delete_trigger(&client, &trigger_id, true)
                .await
                .context("failed to purge trigger")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerCommands::RotateInboundKey { trigger_id } => {
            let v = orchestration::rotate_trigger_inbound_key(&client, &trigger_id)
                .await
                .context("failed to rotate trigger inbound key")?;
            ctx.output.render(&v)?;
            Ok(())
        }
    }
}

// ─── Trigger aliases ──────────────────────────────────────────────────────────

async fn execute_trigger_alias(
    command: WorkflowTriggerAliasCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        WorkflowTriggerAliasCommands::Get { workspace_id } => {
            let v = orchestration::get_trigger_aliases(&client, &workspace_id)
                .await
                .context("failed to read trigger aliases")?;
            // Project just the alias map for readability.
            let aliases = project_trigger_aliases(&v);
            ctx.output.render(&aliases)?;
            Ok(())
        }
        WorkflowTriggerAliasCommands::Set {
            workspace_id,
            verb,
            template,
        } => {
            let current = orchestration::get_trigger_aliases(&client, &workspace_id)
                .await
                .context("failed to read current trigger aliases")?;
            let mut map = current_alias_map(&current);
            map.insert(verb, Value::String(template));
            let aliases_json = serde_json::to_string(&Value::Object(map))
                .context("failed to serialize alias map")?;
            let v = orchestration::set_trigger_aliases(&client, &workspace_id, &aliases_json)
                .await
                .context("failed to set trigger alias")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerAliasCommands::Remove { workspace_id, verb } => {
            let current = orchestration::get_trigger_aliases(&client, &workspace_id)
                .await
                .context("failed to read current trigger aliases")?;
            let mut map = current_alias_map(&current);
            if map.remove(&verb).is_none() {
                anyhow::bail!("alias verb '{verb}' is not present in the map");
            }
            let aliases_json = serde_json::to_string(&Value::Object(map))
                .context("failed to serialize alias map")?;
            let v = orchestration::set_trigger_aliases(&client, &workspace_id, &aliases_json)
                .await
                .context("failed to remove trigger alias")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowTriggerAliasCommands::Replace {
            workspace_id,
            aliases_json,
        } => {
            // Normalise + validate the supplied map client-side before the
            // network: it MUST be a JSON object (verb→template). Re-serializing
            // the parsed value drops insignificant whitespace and rejects
            // non-object input deterministically rather than letting the server
            // reject a malformed body.
            let normalized = normalize_alias_map_json(&aliases_json)?;
            let v = orchestration::set_trigger_aliases(&client, &workspace_id, &normalized)
                .await
                .context("failed to replace trigger aliases")?;
            ctx.output.render(&v)?;
            Ok(())
        }
    }
}

/// Validate `--aliases-json` is a JSON object and re-serialize it canonically.
///
/// The contract's `workflow_trigger_aliases` field is a verb→template map, so a
/// non-object (array, string, number) is rejected client-side with a clear
/// error rather than forwarded.
fn normalize_alias_map_json(aliases_json: &str) -> Result<String> {
    let parsed: Value =
        serde_json::from_str(aliases_json).context("--aliases-json is not valid JSON")?;
    anyhow::ensure!(
        parsed.is_object(),
        "--aliases-json must be a JSON object mapping verb→template, e.g. \
         '{{\"redact\":\"redact-tpl\"}}'"
    );
    serde_json::to_string(&parsed).context("failed to serialize alias map")
}

/// Extract the `workflow_trigger_aliases` object from a workspace response as a
/// serde map (empty when absent/malformed).
fn current_alias_map(workspace: &Value) -> serde_json::Map<String, Value> {
    let payload = workspace.get("response").unwrap_or(workspace);
    payload
        .get("workflow_trigger_aliases")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Project just the alias map into a small renderable envelope.
fn project_trigger_aliases(workspace: &Value) -> Value {
    serde_json::json!({
        "result": "yes",
        "workflow_trigger_aliases": Value::Object(current_alias_map(workspace)),
    })
}

// ─── Obligations ─────────────────────────────────────────────────────────────

async fn execute_obligation(
    command: WorkflowObligationCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowObligationCommands::List {
            workflow_id,
            status,
            assigned_user_id,
            limit,
            offset,
        } => orchestration::list_obligations(
            &client,
            &workflow_id,
            status.as_deref(),
            assigned_user_id.as_deref(),
            limit,
            offset,
        )
        .await
        .context("failed to list obligations")?,
        WorkflowObligationCommands::Get { obligation_id } => {
            orchestration::get_obligation(&client, &obligation_id)
                .await
                .context("failed to get obligation")?
        }
        WorkflowObligationCommands::Claim { obligation_id } => {
            orchestration::claim_obligation(&client, &obligation_id)
                .await
                .context("failed to claim obligation")?
        }
        WorkflowObligationCommands::Release { obligation_id } => {
            orchestration::release_obligation(&client, &obligation_id)
                .await
                .context("failed to release obligation")?
        }
        WorkflowObligationCommands::Resolve {
            obligation_id,
            resolution_payload,
        } => {
            let payload =
                resolve_opt_json_arg(resolution_payload.as_deref(), "resolution payload")?;
            orchestration::resolve_obligation(&client, &obligation_id, payload.as_deref())
                .await
                .context("failed to resolve obligation")?
        }
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Inbox ─────────────────────────────────────────────────────────────────────

async fn execute_inbox(command: WorkflowInboxCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowInboxCommands::Me => orchestration::inbox(&client)
            .await
            .context("failed to read inbox")?,
        WorkflowInboxCommands::Workspace { workspace_id } => {
            orchestration::inbox_workspace(&client, &workspace_id)
                .await
                .context("failed to read workspace inbox")?
        }
        WorkflowInboxCommands::Pool {
            workspace_id,
            pool_key,
        } => orchestration::inbox_pool(&client, &workspace_id, &pool_key)
            .await
            .context("failed to read pool inbox")?,
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Extraction schema ────────────────────────────────────────────────────────

async fn execute_schema(command: WorkflowSchemaCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        WorkflowSchemaCommands::Get { workflow_id } => {
            let v = orchestration::get_extraction_schema(&client, &workflow_id)
                .await
                .context("failed to get extraction schema")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowSchemaCommands::Set {
            workflow_id,
            extraction_schema,
        } => {
            let schema = resolve_json_arg(&extraction_schema, "extraction schema")?;
            let v = orchestration::set_extraction_schema(&client, &workflow_id, &schema)
                .await
                .context("failed to set extraction schema")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowSchemaCommands::Derive {
            workflow_id,
            node_ids,
            confirm_ai_spend,
        } => {
            confirm_spend(
                "workflow schema derive",
                "one LLM proposal over a sample of the workflow's files",
                confirm_ai_spend,
            )?;
            let node_ids = resolve_opt_json_arg(node_ids.as_deref(), "node ids")?;
            let v =
                orchestration::derive_extraction_schema(&client, &workflow_id, node_ids.as_deref())
                    .await
                    .context("failed to derive extraction schema")?;
            ctx.output.render(&v)?;
            Ok(())
        }
    }
}

/// Gate an AI-credit-spending action behind explicit acknowledgement (mirrors
/// the metadata command's `confirm_ai_spend`). Non-interactive callers that
/// omit the flag are blocked deterministically.
fn confirm_spend(action: &str, cost_note: &str, confirmed: bool) -> Result<()> {
    use std::io::{self, BufRead, IsTerminal, Write};
    if confirmed {
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

// ─── Audit ─────────────────────────────────────────────────────────────────────

async fn execute_audit(command: WorkflowAuditCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        WorkflowAuditCommands::Events {
            workflow_id,
            include_payload,
            limit,
            offset,
        } => {
            let client = ctx.build_client()?;
            let v =
                orchestration::audit_events(&client, &workflow_id, include_payload, limit, offset)
                    .await
                    .context("failed to list audit events")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowAuditCommands::Export(c) => execute_audit_export(c, ctx).await,
        WorkflowAuditCommands::CheckIntegrity { manifest, chunks } => {
            check_integrity(ctx, &manifest, &chunks)
        }
        WorkflowAuditCommands::Redaction(c) => execute_redaction(c, ctx).await,
    }
}

async fn execute_audit_export(
    command: WorkflowAuditExportCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        WorkflowAuditExportCommands::Start {
            workflow_id,
            scope,
            include_overlays,
            redaction_pin_strategy,
        } => {
            let v = orchestration::start_audit_export(
                &client,
                &workflow_id,
                scope.as_deref(),
                include_overlays,
                redaction_pin_strategy.as_deref(),
            )
            .await
            .context("failed to start audit export")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowAuditExportCommands::List {
            workspace_id,
            limit,
            offset,
        } => {
            let v = orchestration::list_audit_export_jobs(&client, &workspace_id, limit, offset)
                .await
                .context("failed to list audit export jobs")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowAuditExportCommands::Get { job_id } => {
            let v = orchestration::get_audit_export_job(&client, &job_id)
                .await
                .context("failed to get audit export job")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowAuditExportCommands::Download {
            job_id,
            chunk,
            output,
        } => {
            // Stream the bundle chunk to disk (NEVER buffer — bundles can be
            // large) via the Phase-0 streaming helper.
            let path = orchestration::audit_bundle_chunk_path(&job_id, &chunk);
            let bytes = client
                .download_file_stream(&path, &output)
                .await
                .context("failed to download audit bundle chunk")?;
            if !ctx.output.quiet {
                eprintln!(
                    "downloaded chunk '{chunk}' ({bytes} bytes) to '{}'. After downloading the \
                     manifest and all chunks, run \
                     'fastio workflow audit check-integrity --manifest <manifest> --chunk <0> …'.",
                    output.display()
                );
            }
            Ok(())
        }
    }
}

/// Derive a chunk's manifest id from its filename.
///
/// Accepts the names the download paths emit — `chunk_0003.jsonl` → `"3"` — as
/// well as a bare integer file stem (`3.jsonl` → `"3"`). The leading-zero pad
/// is stripped so the id matches the manifest's `chunk_hashes` key (which is an
/// unpadded integer string / array index). Returns `None` when no integer can
/// be recovered, so the caller can refuse rather than guess.
fn chunk_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    // Strip an optional `chunk_` (or `chunk-`) prefix, then require the rest to
    // parse as an unsigned integer (this also drops any leading zero pad).
    let digits = stem
        .strip_prefix("chunk_")
        .or_else(|| stem.strip_prefix("chunk-"))
        .unwrap_or(stem);
    digits.parse::<u64>().ok().map(|n| n.to_string())
}

/// Run the integrity portion of the audit-bundle verifier recipe over a
/// locally-downloaded manifest + chunks. Integrity ONLY (chunk hashes +
/// content-hash chain + completeness) — the HMAC authenticity `verify` is
/// deferred.
fn check_integrity(
    ctx: &CommandContext<'_>,
    manifest_path: &Path,
    chunk_paths: &[std::path::PathBuf],
) -> Result<()> {
    let manifest_text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read manifest '{}'", manifest_path.display()))?;
    let manifest: Value = serde_json::from_str(&manifest_text)
        .with_context(|| format!("manifest '{}' is not valid JSON", manifest_path.display()))?;

    let mut chunks = Vec::with_capacity(chunk_paths.len());
    for p in chunk_paths {
        let bytes =
            std::fs::read(p).with_context(|| format!("failed to read chunk '{}'", p.display()))?;
        // Derive the chunk id from the FILENAME, not the positional --chunk
        // order: `chunk_0003.jsonl` → `3`. Trusting argument order would let an
        // out-of-order or omitted --chunk silently mis-map a chunk's bytes onto
        // the wrong manifest hash entry (and pass integrity on tampered data).
        let chunk_id = chunk_id_from_filename(p).with_context(|| {
            format!(
                "cannot derive a chunk id from '{}'. Name chunks like 'chunk_0003.jsonl' \
                 (or pass a bare integer id) so they map to the manifest's chunk_hashes \
                 entries unambiguously.",
                p.display()
            )
        })?;
        chunks.push(DownloadedChunk { chunk_id, bytes });
    }

    let report = orchestration::check_bundle_integrity(&manifest, &chunks);

    // Honesty scope: the completeness gap-check is only RUN (and only counted
    // toward `passed()`) when the manifest claims completeness. When it does
    // not (`includes_completeness_proof=false`), the check is skipped — so the
    // completeness line must NOT appear under `verifies` (that would overstate
    // what was proven); it moves under `does_not_verify` instead, keeping the
    // prose consistent with the structured `completeness_claimed` boolean.
    let mut verifies = vec!["chunk SHA-256 (recomputed over the downloaded chunk bytes)"];
    let mut does_not_verify = vec![
        "per-event content_hash recompute (needs canonical-byte/JCS canonicalization — deferred)",
        "HMAC manifest_signature / authenticity (needs the per-workspace audit key — deferred)",
    ];
    if report.completeness_claimed {
        verifies.push("completeness (chunks cover every event_seq in the manifest range)");
    } else {
        does_not_verify.push(
            "completeness (manifest sets includes_completeness_proof=false — gap-check skipped)",
        );
    }
    verifies
        .push("chain linkage (each event's prior_content_hash == the prior event's content_hash)");

    // A chunk can pass its SHA-256 yet still carry an invalid-UTF-8 region or a
    // malformed JSONL line. That is a hard failure (`workflows.txt:258` — reject
    // the bundle on any failure), so surface it under `does_not_verify` rather
    // than burying it only in `notes`. The owned String is kept alive in
    // `parse_failure_detail` so the &str pushed into `does_not_verify` stays
    // valid for the render below.
    let parse_failure_detail;
    if !report.parse_ok {
        parse_failure_detail = format!(
            "content parsing ({} chunk content failure(s): invalid UTF-8 or malformed JSONL line(s) \
             — the chunk SHA-256 may match but the bundle is NOT trustworthy)",
            report.parse_failures
        );
        does_not_verify.push(parse_failure_detail.as_str());
    }

    let rendered = serde_json::json!({
        "result": if report.passed() { "yes" } else { "no" },
        "integrity_check": {
            "passed": report.passed(),
            // Explicit honesty scope: what this check DOES and DOES NOT prove.
            "verifies": verifies,
            "does_not_verify": does_not_verify,
            "authenticity_checked": false,
            "chunks_ok": report.chunks_ok,
            "chain_ok": report.chain_ok,
            "parse_ok": report.parse_ok,
            "parse_failures": report.parse_failures,
            "events_checked": report.events_checked,
            "completeness_claimed": report.completeness_claimed,
            "completeness_ok": report.completeness_ok,
            "chunk_results": report.chunk_results.iter().map(|(id, expected, ok)| {
                serde_json::json!({"chunk_id": id, "expected_hash": expected, "ok": ok})
            }).collect::<Vec<_>>(),
            "notes": report.notes,
        }
    });
    ctx.output.render(&rendered)?;
    if report.passed() {
        Ok(())
    } else {
        anyhow::bail!(
            "audit bundle integrity check FAILED (this is an integrity check only; HMAC \
             authenticity is not verified). See the notes in the output above."
        )
    }
}

/// Build the `mode=request` form fields for a dual-control redaction.
///
/// Extracted as a pure function so the optional-`redaction_paths` wiring is
/// testable without a network round-trip: the field is inserted only when the
/// caller supplied paths (omitted → the server derives the scope from the
/// target event). `reason`, `target_event_id`, and `target_workflow_id` are
/// always sent.
fn build_redaction_request_fields(
    target_event_id: &str,
    target_workflow_id: &str,
    redaction_paths: Option<&str>,
    reason: &str,
) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    fields.insert("mode".to_owned(), "request".to_owned());
    fields.insert("target_event_id".to_owned(), target_event_id.to_owned());
    fields.insert(
        "target_workflow_id".to_owned(),
        target_workflow_id.to_owned(),
    );
    if let Some(paths) = redaction_paths {
        fields.insert("redaction_paths".to_owned(), paths.to_owned());
    }
    fields.insert("reason".to_owned(), reason.to_owned());
    fields
}

async fn execute_redaction(
    command: WorkflowRedactionCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowRedactionCommands::Request {
            workspace_id,
            target_event_id,
            target_workflow_id,
            redaction_paths,
            reason,
        } => {
            // `redaction_paths` is OPTIONAL: when omitted the server derives the
            // scope from the redactable identifiers present in the target event.
            // Only forward the field when the caller supplied it.
            let paths = resolve_opt_json_arg(redaction_paths.as_deref(), "redaction paths")?;
            let fields = build_redaction_request_fields(
                &target_event_id,
                &target_workflow_id,
                paths.as_deref(),
                &reason,
            );
            orchestration::audit_redaction(&client, &workspace_id, &fields)
                .await
                .context("failed to request redaction")?
        }
        WorkflowRedactionCommands::Confirm {
            workspace_id,
            action_id,
            confirmer_user_id,
        } => {
            let mut fields = HashMap::new();
            fields.insert("mode".to_owned(), "confirm".to_owned());
            fields.insert("action_id".to_owned(), action_id);
            fields.insert("confirmer_user_id".to_owned(), confirmer_user_id);
            orchestration::audit_redaction(&client, &workspace_id, &fields)
                .await
                .context("failed to confirm redaction")?
        }
        WorkflowRedactionCommands::Get {
            workspace_id,
            redaction_id,
        } => orchestration::get_redaction(&client, &workspace_id, &redaction_id)
            .await
            .context("failed to get redaction")?,
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Outbound subscriptions ───────────────────────────────────────────────────

async fn execute_outbound(
    command: WorkflowOutboundCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        WorkflowOutboundCommands::Create {
            workspace_id,
            target_url,
            event_type_subscriptions,
            description,
            rate_limit_per_hour,
            family_allowlist,
            secret_file,
        } => {
            let params = orchestration::CreateSubscriptionParams::new()
                .target_url(Some(target_url))
                .event_type_subscriptions(Some(resolve_json_arg(
                    &event_type_subscriptions,
                    "event type subscriptions",
                )?))
                .description(description)
                .rate_limit_per_hour(rate_limit_per_hour)
                .family_allowlist(resolve_opt_json_arg(
                    family_allowlist.as_deref(),
                    "family allowlist",
                )?);
            let v = orchestration::create_subscription(&client, &workspace_id, &params)
                .await
                .context("failed to create outbound subscription")?;
            handle_secret_response(ctx, v, secret_file.as_deref(), "outbound webhook secret")
        }
        WorkflowOutboundCommands::List { workspace_id } => {
            let v = orchestration::list_subscriptions(&client, &workspace_id)
                .await
                .context("failed to list outbound subscriptions")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowOutboundCommands::Get { subscription_id } => {
            let v = orchestration::get_subscription(&client, &subscription_id)
                .await
                .context("failed to get outbound subscription")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowOutboundCommands::Update {
            subscription_id,
            enabled,
            description,
            rate_limit_per_hour,
            family_allowlist,
        } => {
            let mut fields = HashMap::new();
            if let Some(e) = enabled {
                fields.insert("enabled".to_owned(), e.to_string());
            }
            if let Some(d) = description {
                fields.insert("description".to_owned(), d);
            }
            if let Some(r) = rate_limit_per_hour {
                fields.insert("rate_limit_per_hour".to_owned(), r.to_string());
            }
            if let Some(fa) = family_allowlist {
                fields.insert(
                    "family_allowlist".to_owned(),
                    resolve_json_arg(&fa, "family allowlist")?,
                );
            }
            anyhow::ensure!(!fields.is_empty(), "no fields to update were supplied");
            let v = orchestration::update_subscription(&client, &subscription_id, &fields)
                .await
                .context("failed to update outbound subscription")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowOutboundCommands::Delete { subscription_id } => {
            let v = orchestration::delete_subscription(&client, &subscription_id)
                .await
                .context("failed to delete outbound subscription")?;
            ctx.output.render(&v)?;
            Ok(())
        }
        WorkflowOutboundCommands::RotateSecret {
            subscription_id,
            secret_file,
        } => {
            let v = orchestration::rotate_subscription_secret(&client, &subscription_id)
                .await
                .context("failed to rotate outbound subscription secret")?;
            handle_secret_response(ctx, v, secret_file.as_deref(), "outbound webhook secret")
        }
    }
}

/// Handle a response that carries a one-time HMAC secret: extract it into a
/// [`SecretString`], write it to a 0600 `--secret-file` when supplied, redact
/// it from the rendered output, and warn loudly when no file was given.
fn handle_secret_response(
    ctx: &CommandContext<'_>,
    mut value: Value,
    secret_file: Option<&Path>,
    label: &str,
) -> Result<()> {
    if let Some(secret) = extract_secret(&value, "secret") {
        if let Some(path) = secret_file {
            write_secret_file(path, &secret, label, ctx.output.quiet)?;
        } else if !ctx.output.quiet {
            eprintln!(
                "WARNING: this response contains a ONE-TIME {label} that is not retrievable \
                 later. It is REDACTED from stdout to avoid leaking it into logs. Re-run with \
                 --secret-file <path> to capture it (written 0600), or rotate the secret if lost."
            );
        }
        redact_secret_field(&mut value, "secret", "--secret-file");
    }
    ctx.output.render(&value)?;
    Ok(())
}

// ─── Pools ─────────────────────────────────────────────────────────────────────

async fn execute_pool(command: WorkflowPoolCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowPoolCommands::Create {
            workspace_id,
            pool_key,
            max_concurrent,
            pool_source,
            pool_admission_policy,
        } => orchestration::create_pool(
            &client,
            &workspace_id,
            &pool_key,
            max_concurrent,
            pool_source.as_deref(),
            pool_admission_policy.as_deref(),
        )
        .await
        .context("failed to create pool")?,
        WorkflowPoolCommands::List { workspace_id } => {
            orchestration::list_pools(&client, &workspace_id)
                .await
                .context("failed to list pools")?
        }
        WorkflowPoolCommands::Get {
            workspace_id,
            pool_key,
        } => orchestration::get_pool(&client, &workspace_id, &pool_key)
            .await
            .context("failed to get pool")?,
        WorkflowPoolCommands::Delete {
            workspace_id,
            pool_key,
        } => orchestration::delete_pool(&client, &workspace_id, &pool_key)
            .await
            .context("failed to delete pool")?,
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── External subjects ──────────────────────────────────────────────────────

async fn execute_subject(command: WorkflowSubjectCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowSubjectCommands::Workflows {
            workspace_id,
            subject_id,
        } => orchestration::subject_workflows(&client, &workspace_id, &subject_id)
            .await
            .context("failed to list subject workflows")?,
    };
    ctx.output.render(&v)?;
    Ok(())
}

// ─── Realtime ──────────────────────────────────────────────────────────────────

async fn execute_realtime(
    command: WorkflowRealtimeCommands,
    ctx: &CommandContext<'_>,
) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        WorkflowRealtimeCommands::Token {
            workflow_id,
            token_file,
        } => {
            let mut v = orchestration::realtime_token(&client, &workflow_id)
                .await
                .context("failed to mint realtime token")?;
            // The minted token is a secret: write it to a file (0600) when
            // requested, redact it from stdout otherwise.
            if let Some(token) =
                extract_secret(&v, "token").or_else(|| extract_secret(&v, "auth_token"))
            {
                if let Some(path) = &token_file {
                    write_secret_file(path, &token, "realtime token", ctx.output.quiet)?;
                } else if !ctx.output.quiet {
                    eprintln!(
                        "WARNING: the realtime token is REDACTED from stdout to avoid leaking it \
                         into logs. Re-run with --token-file <path> to capture it (written 0600)."
                    );
                }
                redact_secret_field(&mut v, "token", "--token-file");
                redact_secret_field(&mut v, "auth_token", "--token-file");
            }
            ctx.output.render(&v)?;
            Ok(())
        }
    }
}

// ─── Review (v3.5b) ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)] // a flat dispatch over the Workflow Review surface
async fn execute_review(command: WorkflowReviewCommands, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    let v = match command {
        WorkflowReviewCommands::Create { step_occurrence_id } => {
            orchestration::review_surface_create(&client, &step_occurrence_id)
                .await
                .context("failed to create review surface")?
        }
        WorkflowReviewCommands::Get { surface_id } => {
            orchestration::review_surface_get(&client, &surface_id)
                .await
                .context("failed to get review surface")?
        }
        WorkflowReviewCommands::Asset {
            surface_id,
            asset_id,
        } => orchestration::review_asset_get(&client, &surface_id, &asset_id)
            .await
            .context("failed to get review asset")?,
        WorkflowReviewCommands::Decision {
            surface_id,
            asset_id,
            decision,
            version_id_pinned,
            comment,
        } => {
            // The server requires a non-empty reason for `reject` /
            // `request_changes` (422 `ERR_REASON_REQUIRED`). Guard client-side
            // before the call so the user gets a clear, immediate message.
            let reason_required = matches!(decision.as_str(), "reject" | "request_changes");
            let reason_empty = comment.as_deref().is_none_or(|c| c.trim().is_empty());
            if reason_required && reason_empty {
                anyhow::bail!(
                    "a reason is required for a '{decision}' decision; pass --comment \"<reason>\""
                );
            }
            orchestration::review_decision(
                &client,
                &surface_id,
                &asset_id,
                &decision,
                &version_id_pinned,
                comment.as_deref(),
            )
            .await
            .context("failed to record review decision")?
        }
        WorkflowReviewCommands::AdminResolve {
            surface_id,
            resolution,
        } => orchestration::review_admin_resolve(&client, &surface_id, &resolution)
            .await
            .context("failed to admin-resolve review surface")?,
        WorkflowReviewCommands::Active {
            workspace_id,
            limit,
            offset,
        } => orchestration::review_workspace_active(&client, &workspace_id, limit, offset)
            .await
            .context("failed to list active reviews")?,
        WorkflowReviewCommands::ReviewerAddMember {
            surface_id,
            member_user_id,
            required,
        } => orchestration::review_reviewer_add_member(
            &client,
            &surface_id,
            &member_user_id,
            required,
        )
        .await
        .context("failed to add member reviewer")?,
        WorkflowReviewCommands::ReviewerAddExternal {
            surface_id,
            email,
            name,
            invite_notes,
        } => orchestration::review_reviewer_add_external(
            &client,
            &surface_id,
            &email,
            &name,
            invite_notes.as_deref(),
        )
        .await
        .context("failed to add external reviewer")?,
        WorkflowReviewCommands::ReviewerRemove {
            surface_id,
            reviewer_id,
        } => orchestration::review_reviewer_remove(&client, &surface_id, &reviewer_id)
            .await
            .context("failed to remove reviewer")?,
        WorkflowReviewCommands::ReviewerNotificationOptOut {
            surface_id,
            reviewer_id,
            opt_out,
        } => orchestration::review_reviewer_notification_opt_out(
            &client,
            &surface_id,
            &reviewer_id,
            opt_out,
        )
        .await
        .context("failed to set reviewer notification opt-out")?,
        WorkflowReviewCommands::LinkTokenRevoke {
            link_token_id,
            reason,
        } => orchestration::review_link_token_revoke(&client, &link_token_id, reason.as_deref())
            .await
            .context("failed to revoke link token")?,
        WorkflowReviewCommands::CommentsList {
            asset_id,
            limit,
            offset,
        } => orchestration::review_asset_comments_list(&client, &asset_id, limit, offset)
            .await
            .context("failed to list review comments")?,
        WorkflowReviewCommands::CommentAdd {
            asset_id,
            body,
            anchor_json,
        } => {
            let anchor = resolve_opt_json_object_value(anchor_json.as_deref(), "anchor")?;
            orchestration::review_asset_comment_create(&client, &asset_id, &body, anchor.as_ref())
                .await
                .context("failed to add review comment")?
        }
        WorkflowReviewCommands::CommentUpdate {
            asset_id,
            comment_id,
            body,
        } => orchestration::review_asset_comment_update(&client, &asset_id, &comment_id, &body)
            .await
            .context("failed to update review comment")?,
        WorkflowReviewCommands::CommentDelete {
            asset_id,
            comment_id,
        } => orchestration::review_asset_comment_delete(&client, &asset_id, &comment_id)
            .await
            .context("failed to delete review comment")?,
        WorkflowReviewCommands::SendForReview {
            workspace,
            workflow,
            step_occurrence_id,
            reviewer_user_ids,
            reviewers_json,
            assets_json,
            reviewed_node_ids,
            version_id,
            policy_mode,
            policy_quorum_n,
            deadline_at,
            message,
            external_invite_notes,
        } => {
            if reviewers_json.is_some() && reviewer_user_ids.is_some() {
                anyhow::bail!(
                    "pass only one of --reviewers-json or --reviewer-user-ids (they are mutually exclusive)"
                );
            }
            let reviewers = resolve_opt_json_array_value(reviewers_json.as_deref(), "reviewers")?;
            let assets = resolve_opt_json_array_value(assets_json.as_deref(), "assets")?;
            let params = orchestration::SendForReviewParams {
                workspace_id: &workspace,
                workflow_id: &workflow,
                step_occurrence_id: &step_occurrence_id,
                reviewers: reviewers.as_ref(),
                reviewer_user_ids: reviewer_user_ids.as_deref(),
                assets: assets.as_ref(),
                reviewed_node_ids: reviewed_node_ids.as_deref(),
                version_id: version_id.as_deref(),
                policy_mode: policy_mode.as_deref(),
                policy_quorum_n,
                deadline_at: deadline_at.as_deref(),
                message: message.as_deref(),
                external_invite_notes: external_invite_notes.as_deref(),
            };
            orchestration::review_send_for_review(&client, &params)
                .await
                .context("failed to send for review")?
        }
        WorkflowReviewCommands::QuickApproval {
            workspace,
            node_id,
            idempotency_key,
            reviewer_user_ids,
            reviewers_json,
            policy_mode,
            policy_quorum_n,
            approval_timeout_seconds,
            message,
        } => {
            if reviewers_json.is_some() && reviewer_user_ids.is_some() {
                anyhow::bail!(
                    "pass only one of --reviewers-json or --reviewer-user-ids (they are mutually exclusive)"
                );
            }
            let reviewers = resolve_opt_json_array_value(reviewers_json.as_deref(), "reviewers")?;
            let params = orchestration::QuickApprovalParams {
                workspace_id: &workspace,
                node_id: &node_id,
                idempotency_key: &idempotency_key,
                reviewers: reviewers.as_ref(),
                reviewer_user_ids: reviewer_user_ids.as_deref(),
                policy_mode: policy_mode.as_deref(),
                policy_quorum_n,
                approval_timeout_seconds,
                message: message.as_deref(),
            };
            orchestration::review_quick_approval(&client, &params)
                .await
                .context("failed to request quick approval")?
        }
    };
    ctx.output.render(&v)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_key_required_without_explicit_or_generate() {
        let err = resolve_idempotency_key(None, false, true).unwrap_err();
        assert!(err.to_string().contains("--idempotency-key is required"));
    }

    #[test]
    fn redaction_request_fields_omit_paths_when_none() {
        let fields = build_redaction_request_fields("evt-1", "1234567890123456789", None, "GDPR");
        assert_eq!(fields.get("mode").map(String::as_str), Some("request"));
        assert_eq!(
            fields.get("target_event_id").map(String::as_str),
            Some("evt-1")
        );
        assert_eq!(
            fields.get("target_workflow_id").map(String::as_str),
            Some("1234567890123456789")
        );
        assert_eq!(fields.get("reason").map(String::as_str), Some("GDPR"));
        // Omitted paths must not appear — the server derives the scope.
        assert!(!fields.contains_key("redaction_paths"));
    }

    #[test]
    fn redaction_request_fields_include_paths_when_supplied() {
        let fields = build_redaction_request_fields(
            "evt-1",
            "1234567890123456789",
            Some(r#"["payload.pii.email"]"#),
            "GDPR",
        );
        assert_eq!(
            fields.get("redaction_paths").map(String::as_str),
            Some(r#"["payload.pii.email"]"#)
        );
    }

    #[test]
    fn explicit_idempotency_key_is_honored() {
        let key = resolve_idempotency_key(Some("job-001"), false, true).unwrap();
        assert_eq!(key, "job-001");
    }

    #[test]
    fn blank_explicit_idempotency_key_is_rejected() {
        assert!(resolve_idempotency_key(Some("   "), false, true).is_err());
    }

    #[test]
    fn generate_idempotency_key_opt_in_produces_key() {
        let key = resolve_idempotency_key(None, true, true).unwrap();
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_json_arg_accepts_inline_json() {
        let out = resolve_json_arg(r#"{"a":1}"#, "body").unwrap();
        assert_eq!(out, r#"{"a":1}"#);
    }

    #[test]
    fn resolve_json_arg_rejects_malformed_json() {
        assert!(resolve_json_arg("{not json", "body").is_err());
    }

    #[test]
    fn resolve_json_arg_at_at_escapes_literal_at() {
        // `@@x` should be a literal value `x` (which here is invalid JSON).
        assert!(resolve_json_arg("@@notjson", "body").is_err());
        let out = resolve_json_arg("@@\"x\"", "body").unwrap();
        assert_eq!(out, "\"x\"");
    }

    #[test]
    fn confirm_purge_matches_id() {
        assert!(confirm_purge("abc", "abc", "workflow").is_ok());
        assert!(confirm_purge("abc", "xyz", "workflow").is_err());
    }

    #[test]
    fn chunk_id_from_filename_derives_unpadded_integer() {
        let id = |p: &str| chunk_id_from_filename(Path::new(p));
        // The names the download path emits.
        assert_eq!(id("chunk_0003.jsonl").as_deref(), Some("3"));
        assert_eq!(id("/tmp/bundle/chunk_0000.jsonl").as_deref(), Some("0"));
        assert_eq!(id("chunk-12.jsonl").as_deref(), Some("12"));
        // Bare integer stems also work.
        assert_eq!(id("7.jsonl").as_deref(), Some("7"));
        assert_eq!(id("42").as_deref(), Some("42"));
        // Non-integer names are refused (not silently mapped to a wrong hash).
        assert_eq!(id("manifest.json"), None);
        assert_eq!(id("chunk_abc.jsonl"), None);
        assert_eq!(id("random.txt"), None);
    }

    #[test]
    fn clamp_wait_interval_bounds() {
        assert_eq!(clamp_wait_interval(None), DEFAULT_WAIT_INTERVAL_SECS);
        assert_eq!(clamp_wait_interval(Some(0)), MIN_WAIT_INTERVAL_SECS);
        assert_eq!(clamp_wait_interval(Some(999)), MAX_WAIT_INTERVAL_SECS);
    }

    #[test]
    fn jitter_never_below_base_and_bounded() {
        for _ in 0..50 {
            let d = jittered(4);
            assert!(d >= Duration::from_secs(4));
            // base 4 + up to 2s jitter
            assert!(d <= Duration::from_secs(6) + Duration::from_millis(1));
        }
        // Zero-span (interval 1) adds no jitter.
        assert_eq!(jittered(1), Duration::from_secs(1));
    }

    #[test]
    fn terminal_state_detection() {
        assert!(is_terminal_state("completed"));
        assert!(is_terminal_state("cancelled"));
        assert!(is_terminal_state("archived"));
        assert!(!is_terminal_state("active"));
        assert!(!is_terminal_state("paused"));
    }

    #[test]
    fn step_terminal_state_detection() {
        for s in ["completed", "failed", "skipped", "cancelled"] {
            assert!(is_terminal_step_state(s), "{s} should be terminal");
        }
        for s in ["pending", "in_progress", "waiting", "blocked"] {
            assert!(!is_terminal_step_state(s), "{s} should be mutable");
        }
    }

    #[test]
    fn run_status_prefers_run_summary_then_runtime() {
        // `run_summary.status` wins when both are present (it is the digest that
        // also carries `not_started`).
        let snap = serde_json::json!({
            "runtime": {"status": "running"},
            "run_summary": {"status": "failed"},
        });
        assert_eq!(run_status(&snap), Some("failed"));
        // Falls back to runtime.status when run_summary carries none.
        let snap = serde_json::json!({"runtime": {"status": "completed"}});
        assert_eq!(run_status(&snap), Some("completed"));
        // None when neither carries a status.
        assert_eq!(run_status(&serde_json::json!({})), None);
    }

    #[test]
    fn terminal_run_status_detection() {
        for s in ["completed", "failed", "cancelled"] {
            assert!(is_terminal_run_status(s), "{s} should be terminal");
        }
        for s in ["queued", "running", "not_started"] {
            assert!(!is_terminal_run_status(s), "{s} should be in-flight");
        }
    }

    #[test]
    fn failed_run_surfaces_step_failure_reason_code() {
        // The `GET /state/` wire shape carries NO run-level failure reason: a
        // failed run's cause lives ONLY as a structured `failure_reason {code,
        // message}` object on the failed step occurrence in `recent_steps[]`. The
        // notice must name that code (e.g. native_review_tier_not_provisioned) and
        // flag the run as a failure.
        let snap = serde_json::json!({
            "run_summary": {"status": "failed"},
            "recent_steps": [
                {"step_id": "s1", "step_name": "Collect", "state": "completed"},
                {
                    "step_id": "s2",
                    "step_name": "Native Approval",
                    "step_type": "approval",
                    "state": "failed",
                    "failure_reason": {
                        "code": "native_review_tier_not_provisioned",
                        "message": "native_review_tier_not_provisioned"
                    }
                }
            ],
        });
        let notice = terminal_completion_notice(&snap).expect("notice for failed run");
        assert!(
            notice.contains("native_review_tier_not_provisioned"),
            "must name the failure_reason.code: {notice}"
        );
        assert!(notice.to_uppercase().contains("FAILED"), "{notice}");
        assert!(
            notice.contains("Native Approval"),
            "should name the failing step: {notice}"
        );
    }

    #[test]
    fn failed_run_reads_skipped_step_and_active_steps_fallback() {
        // A natural-language-gate skip rides the same `failure_reason` path on a
        // `skipped` occurrence, and the reader falls back to `active_steps[]` when
        // `recent_steps[]` carries no reason.
        let snap = serde_json::json!({
            "run_summary": {"status": "failed"},
            "recent_steps": [],
            "active_steps": [
                {
                    "step_id": "s9",
                    "step_name": "Gate",
                    "step_type": "agent",
                    "state": "skipped",
                    "failure_reason": {"code": "nl_gate_denied", "message": "nl_gate_denied"}
                }
            ],
        });
        let notice = terminal_completion_notice(&snap).expect("notice for skipped-reason run");
        assert!(notice.contains("nl_gate_denied"), "{notice}");
    }

    #[test]
    fn failed_run_without_step_reason_is_still_flagged() {
        // A failed run whose steps carry no `failure_reason` still surfaces the
        // failure generically so the user is not left thinking it succeeded.
        let snap = serde_json::json!({"run_summary": {"status": "failed"}});
        let notice = terminal_completion_notice(&snap).expect("notice for bare failure");
        assert!(notice.to_uppercase().contains("FAILED"), "{notice}");

        // A failed run with a failed step but no reason object: still generic.
        let snap = serde_json::json!({
            "run_summary": {"status": "failed"},
            "recent_steps": [{"step_id": "s1", "state": "failed"}],
        });
        let notice = terminal_completion_notice(&snap).expect("notice for reasonless failure");
        assert!(notice.to_uppercase().contains("FAILED"), "{notice}");
    }

    #[test]
    fn completed_run_with_rejected_approval_is_flagged() {
        // An approval-rejected run reports status=completed (a rejection is a
        // completed approval, not a failure); the clean `approval_rejected` token
        // is event-only and absent from state. Detection is the state-only
        // heuristic: a terminal approval step whose output is `rejected`.
        let snap = serde_json::json!({
            "run_summary": {"status": "completed"},
            "recent_steps": [
                {
                    "step_id": "a1",
                    "step_name": "Sign-off",
                    "step_type": "approval",
                    "state": "completed",
                    "output": {"decision": "rejected", "reviewer": "42"}
                }
            ],
        });
        let notice = terminal_completion_notice(&snap).expect("notice for rejected approval");
        assert!(
            notice.to_lowercase().contains("rejection")
                || notice.to_lowercase().contains("rejected"),
            "must flag the approval rejection: {notice}"
        );
        assert!(
            notice.to_lowercase().contains("not a normal success"),
            "must distinguish from a normal success: {notice}"
        );

        // `approval_in_place` writes the SAME terminal output key as manual
        // `approval` — it delegates resolution to the composed
        // `ApprovalStepHandler::completedResult()`, which stamps
        // `output.outcome = "rejected"` (backend-confirmed). The fixture uses
        // that real key.
        let in_place = serde_json::json!({
            "run_summary": {"status": "completed"},
            "recent_steps": [
                {
                    "step_id": "a2",
                    "step_type": "approval_in_place",
                    "state": "completed",
                    "output": {"status": "resolved", "surface_id": "sfc-1", "outcome": "rejected"}
                }
            ],
        });
        assert!(
            terminal_completion_notice(&in_place).is_some(),
            "approval_in_place rejection (output.outcome) must be detected"
        );

        // Defensive fallback: a native surface that resolves on the alternate
        // `resolved_outcome` key is still detected.
        let alt_key = serde_json::json!({
            "run_summary": {"status": "completed"},
            "recent_steps": [
                {
                    "step_id": "a3",
                    "step_type": "approval_in_place",
                    "state": "completed",
                    "output": {"resolved_outcome": "rejected"}
                }
            ],
        });
        assert!(
            terminal_completion_notice(&alt_key).is_some(),
            "resolved_outcome fallback must still be detected"
        );
    }

    #[test]
    fn normal_completion_emits_no_notice() {
        // A run that completed plainly (no rejected approval step) must NOT emit a
        // notice — a fake terminal_reason is never synthesized for a success.
        let snap = serde_json::json!({
            "run_summary": {"status": "completed"},
            "recent_steps": [
                {
                    "step_id": "a1",
                    "step_type": "approval",
                    "state": "completed",
                    "output": {"decision": "approved"}
                }
            ],
        });
        assert_eq!(terminal_completion_notice(&snap), None);
    }

    #[test]
    fn cancelled_run_is_flagged() {
        // A cancelled run is flagged; the snapshot carries no cancellation reason.
        let snap = serde_json::json!({"run_summary": {"status": "cancelled"}});
        let notice = terminal_completion_notice(&snap).expect("notice for cancelled run");
        assert!(notice.to_lowercase().contains("cancel"), "{notice}");
    }

    #[test]
    fn non_terminal_run_emits_no_notice_even_with_stray_failed_step() {
        // A mid-flight run (running/queued) must NOT emit a terminal notice — even
        // if a stray `recent_steps[]` row looks failed (the terminal-status gate).
        let snap = serde_json::json!({
            "run_summary": {"status": "running"},
            "recent_steps": [
                {
                    "step_id": "s1",
                    "state": "failed",
                    "failure_reason": {"code": "transient_retO", "message": "x"}
                }
            ],
        });
        assert_eq!(terminal_completion_notice(&snap), None);
        // A never-instantiated run (no status at all) emits nothing.
        assert_eq!(terminal_completion_notice(&serde_json::json!({})), None);
        // `not_started` is explicitly non-terminal.
        let snap = serde_json::json!({"run_summary": {"status": "not_started"}});
        assert_eq!(terminal_completion_notice(&snap), None);
    }

    #[test]
    fn wait_breaks_on_failed_run_even_with_active_profile_state() {
        // FIX C4 wait-arm: a FAILED run sets run_summary/runtime status="failed"
        // while the PROFILE `workflow.state` stays "active" (the profile
        // lifecycle has no `failed` and is not auto-mirrored). The wait loop must
        // still treat this as terminal — otherwise `wait` hangs to its timeout
        // and the FAILED notice never fires. The profile predicate alone (the old
        // break condition) would return false here.
        let failed = serde_json::json!({
            "workflow": {"state": "active"},
            "run_summary": {"status": "failed"},
            "recent_steps": [
                {
                    "step_id": "s2",
                    "step_name": "Native Approval",
                    "step_type": "approval",
                    "state": "failed",
                    "failure_reason": {
                        "code": "native_review_tier_not_provisioned",
                        "message": "native_review_tier_not_provisioned"
                    }
                }
            ],
        });
        // The OLD profile-only predicate would NOT break (state is "active")...
        assert!(!workflow_state(&failed).is_some_and(|s| is_terminal_state(&s)));
        // ...but the wait predicate DOES break on the terminal run status...
        assert!(
            wait_is_terminal(&failed),
            "a failed run with an active profile state must be treated as terminal"
        );
        // ...and the terminal notice that emit_terminal_notice prints fires,
        // naming the failure (so the break is not silent).
        let notice = terminal_notice_for(false, &failed).expect("FAILED notice must fire");
        assert!(notice.to_uppercase().contains("FAILED"), "{notice}");
        assert!(
            notice.contains("native_review_tier_not_provisioned"),
            "must name the per-step failure code: {notice}"
        );
    }

    #[test]
    fn wait_keeps_polling_on_running_run() {
        // A running run with an active profile state is NOT terminal — the loop
        // must keep polling, and no notice fires.
        let running = serde_json::json!({
            "workflow": {"state": "active"},
            "run_summary": {"status": "running"},
        });
        assert!(
            !wait_is_terminal(&running),
            "a running run must NOT be treated as terminal"
        );
        assert_eq!(terminal_notice_for(false, &running), None);
        // A queued run is likewise non-terminal.
        let queued = serde_json::json!({"run_summary": {"status": "queued"}});
        assert!(!wait_is_terminal(&queued));
    }

    #[test]
    fn wait_terminal_peels_response_envelope_and_honors_profile_fallback() {
        // The terminal signal is found whether the snapshot is wrapped in a
        // `response` envelope or flat (run_status takes the peeled payload;
        // workflow_state peels internally).
        let wrapped = serde_json::json!({
            "response": {
                "workflow": {"state": "active"},
                "run_summary": {"status": "completed"},
            }
        });
        assert!(wait_is_terminal(&wrapped), "envelope-wrapped run must peel");

        // Profile-state FALLBACK: a snapshot with NO run status but an
        // `archived`/`deleted` profile state (which has no run-status
        // counterpart) is still terminal via the profile predicate.
        for state in ["archived", "deleted", "completed", "cancelled"] {
            let profile_only = serde_json::json!({"workflow": {"state": state}});
            assert!(
                wait_is_terminal(&profile_only),
                "terminal profile state {state} must break the wait loop"
            );
        }
        // A bare snapshot with neither signal keeps polling.
        assert!(!wait_is_terminal(&serde_json::json!({})));
    }

    #[test]
    fn terminal_notice_for_peels_response_envelope() {
        // FIX 2(a): the terminal fields under a `response` envelope are found and
        // the notice fires.
        let wrapped = serde_json::json!({
            "response": {
                "run_summary": {"status": "cancelled"},
            }
        });
        let notice = terminal_notice_for(false, &wrapped).expect("notice under envelope");
        assert!(notice.to_lowercase().contains("cancel"), "{notice}");
    }

    #[test]
    fn terminal_notice_for_suppressed_under_quiet() {
        // FIX 2(b): `--quiet` suppresses the notice even on a terminal failure.
        let failed = serde_json::json!({"run_summary": {"status": "failed"}});
        assert_eq!(terminal_notice_for(true, &failed), None);
        // Not quiet → the same snapshot DOES produce a notice.
        assert!(terminal_notice_for(false, &failed).is_some());
    }

    #[test]
    fn terminal_notice_for_early_returns_on_non_terminal() {
        // FIX 2(c): a running snapshot returns None (no notice) — even if a stray
        // step row looks failed (the explicit terminal-run-status gate).
        let running = serde_json::json!({
            "run_summary": {"status": "running"},
            "recent_steps": [{"step_id": "s1", "state": "failed"}],
        });
        assert_eq!(terminal_notice_for(false, &running), None);
    }

    #[test]
    fn emit_terminal_notice_threads_ctx_quiet_gate() {
        // FIX 2(d): exercise the real `emit_terminal_notice` entry point through a
        // minimal `CommandContext`, confirming the ctx.output.quiet gate is read
        // correctly and the full path runs without panic over both a terminal and
        // a non-terminal snapshot. The notice can ONLY reach stderr (the sole sink
        // is `eprintln!`), so stdout stays byte-clean by construction; the notice
        // STRING itself is asserted via `terminal_notice_for` above.
        use fastio_cli::output::OutputConfig;
        use std::path::Path;

        let failed = serde_json::json!({"run_summary": {"status": "failed"}});
        let running = serde_json::json!({"run_summary": {"status": "running"}});
        let cfg_dir = Path::new("/nonexistent");

        for quiet in [false, true] {
            let output = OutputConfig::from_flags(Some("json"), None, true, quiet);
            let ctx = CommandContext {
                output: &output,
                profile_name: "test",
                api_base: "https://example.invalid",
                flag_token: None,
                config_dir: cfg_dir,
            };
            // Both a terminal-failed and a still-running snapshot must run the
            // real emitter without panicking, regardless of the quiet flag.
            emit_terminal_notice(&ctx, &failed);
            emit_terminal_notice(&ctx, &running);
        }
    }

    fn api_err(http_status: u16) -> CliError {
        CliError::Api(fastio_cli::error::ApiError::new(
            0,
            None,
            "boom".to_owned(),
            http_status,
        ))
    }

    #[test]
    fn classify_poll_error_rate_limit_uses_retry_after() {
        match classify_poll_error(CliError::RateLimit {
            retry_after_secs: 12,
        }) {
            PollAction::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, 12),
            _ => panic!("rate limit must map to RateLimited"),
        }
        // A 429/408 Api error is also rate-limit-like.
        assert!(matches!(
            classify_poll_error(api_err(429)),
            PollAction::RateLimited { .. }
        ));
        assert!(matches!(
            classify_poll_error(api_err(408)),
            PollAction::RateLimited { .. }
        ));
    }

    #[test]
    fn classify_poll_error_5xx_transient_4xx_fatal() {
        // All 5xx (including 500) are transient — a momentary backend blip.
        for s in [500u16, 502, 503, 504, 599] {
            assert!(
                matches!(classify_poll_error(api_err(s)), PollAction::RetryTransient),
                "{s} should be transient"
            );
        }
        // Persistent client errors must be surfaced, not looped.
        for s in [400u16, 403, 404] {
            assert!(
                matches!(classify_poll_error(api_err(s)), PollAction::Fatal(_)),
                "{s} should be fatal"
            );
        }
        // Parse errors are fatal.
        assert!(matches!(
            classify_poll_error(CliError::Parse("x".to_owned())),
            PollAction::Fatal(_)
        ));
    }

    #[test]
    fn workflow_state_reads_nested_snapshot() {
        let snap = serde_json::json!({
            "response": {"workflow": {"id": "1", "state": "active"}}
        });
        assert_eq!(workflow_state(&snap).as_deref(), Some("active"));
        let flat = serde_json::json!({"workflow": {"state": "completed"}});
        assert_eq!(workflow_state(&flat).as_deref(), Some("completed"));
    }

    #[test]
    fn normalize_alias_map_json_accepts_object_and_rejects_non_object() {
        // FIX M: `replace` validates the full map client-side. A valid object is
        // re-serialized canonically (insignificant whitespace dropped); this is
        // the exact string `set_trigger_aliases` forwards as the
        // `workflow_trigger_aliases` form value.
        let ok = normalize_alias_map_json("{ \"redact\": \"redact-tpl\" }").unwrap();
        let parsed: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(
            parsed.get("redact").and_then(Value::as_str),
            Some("redact-tpl")
        );
        // An empty map is valid (clears all aliases).
        assert_eq!(normalize_alias_map_json("{}").unwrap(), "{}");
        // Non-object inputs are rejected before the network.
        for bad in [r#"["a","b"]"#, "\"str\"", "42", "not json"] {
            assert!(
                normalize_alias_map_json(bad).is_err(),
                "non-object/invalid input must be rejected: {bad}"
            );
        }
    }

    #[test]
    fn alias_map_set_and_remove_round_trip() {
        let ws = serde_json::json!({
            "response": {"workflow_trigger_aliases": {"redact": "redact-tpl"}}
        });
        let mut map = current_alias_map(&ws);
        assert_eq!(
            map.get("redact").and_then(Value::as_str),
            Some("redact-tpl")
        );
        map.insert("summarize".to_owned(), Value::String("sum-tpl".to_owned()));
        assert_eq!(map.len(), 2);
        map.remove("redact");
        assert!(!map.contains_key("redact"));
    }

    #[test]
    fn confirm_spend_blocks_non_interactive_without_flag() {
        // In the test harness stdin/stderr are not TTYs, so omitting the flag
        // must hard-error rather than prompt.
        assert!(confirm_spend("x", "cost", false).is_err());
        assert!(confirm_spend("x", "cost", true).is_ok());
    }
}
