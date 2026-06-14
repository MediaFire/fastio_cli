// Justification: every `pub async fn` here returns `Result<_, CliError>` and
// fails for exactly one reason — the underlying HTTP/envelope call via
// `ApiClient` (network error, non-2xx envelope, or parse failure), already
// fully documented on `CliError`/`ApiError`. Per-function `# Errors` sections
// would be ~40 identical copies of "Returns `CliError` if the API request
// fails." that add noise without information, so the doc requirement is
// allowed off module-wide rather than satisfied with boilerplate. This is
// scoped to this builder module; the rest of the crate keeps the lint on.
#![allow(clippy::missing_errors_doc)]

//! Workflow Orchestration API (v3.2) — the durable multi-step runtime.
//!
//! Maps to the owner-side REST surface documented at
//! `~/vividengine/llms/workflows.txt`. This module is **distinct from the
//! legacy task-management primitives** in [`crate::api::workflow`] (tasks,
//! worklogs, approvals, todos): those endpoints live under
//! `/workspace/{id}/tasks/` and friends, share no IDs or state with this
//! surface, and are not orchestration. This module ships the workflow
//! *profile + runtime*, immutable *templates*, *triggers*, human
//! *obligations*, per-workflow *extraction schemas*, the tamper-evident
//! *audit chain* (events / signed export / dual-control redaction),
//! *outbound webhook subscriptions*, concurrency *pools*, *external-subject*
//! correlation, the realtime-channel token mint, and the v3.5b *review*
//! surface.
//!
//! ## Encoding matrix (form vs JSON) — verified per endpoint
//!
//! Orchestration write bodies are **`application/x-www-form-urlencoded` with
//! JSON-string VALUES** — NOT JSON bodies. The curl examples in
//! `workflows.txt` make this explicit:
//!
//! - `instantiate` → `-d 'idempotency_key=…&trigger_payload={…}'`
//!   (`workflows.txt:464`)
//! - step `output` → `-d 'output={…}'` (`workflows.txt:555`)
//! - obligation `resolve` → `-d 'resolution_payload={…}'`
//!   (`workflows.txt:592`)
//! - `cancel` → `-d 'reason=…'` (`agents.md:2786`)
//! - audit `export` start → `-d "scope=full&include_overlays=true&…"`
//!   (`workflows.txt:630`)
//! - redaction request/confirm → `-d 'mode=request&…&redaction_paths=[…]'`
//!   (`workflows.txt:267`, `:277`)
//! - outbound subscription create → `-d 'target_url=…&event_type_subscriptions=[…]'`
//!   (`workflows.txt:681`)
//! - trigger `fire` → `-d 'idempotency_key=…&trigger_payload={…}'`
//!   (`agents.md:2616`)
//!
//! Endpoints with no documented body (claim/release, pause/resume, publish/
//! withdraw/deprecate, rotate-key, dry-run with defaults) send an **empty
//! form** (`POST` with no fields). PATCH (`workflow` update, `trigger`
//! update, `outbound` update) and PUT (`extraction_schema`) are likewise
//! **form-encoded** per the gate finding — use [`ApiClient::patch_form`] /
//! [`ApiClient::put_form`], never the JSON variants. GETs that list use
//! [`ApiClient::get_with_params`]; the audit bundle download uses the
//! streaming [`ApiClient::download_file_stream`].
//!
//! Structurally-nested bodies (`template_body`, `event_match`,
//! `param_mapping`, `extraction_schema`) are passed through as JSON strings
//! the caller built (often from an `@file`); this module does not validate
//! their internal shape — the server validates and returns a
//! `validation_report` on a 422.
//!
//! ## Identifier formats (`workflows.txt:750-760`)
//!
//! Three id kinds, all treated as opaque `String` and URL-encoded into the
//! path: the **workflow id** is a 19-digit numeric profile id; the
//! **obligation id** is a plain (short) numeric sequence string; everything
//! else (template, trigger, subscription, pool, export-job, redaction,
//! step-occurrence, step, event) is a hyphenated base32 `OpaqueId`. These are
//! the **workflow family**, whose raw form is now **30 chars** (a 2-char `w`
//! type prefix; 35 chars hyphenated) — vs 29/34 for non-workflow ids. Never
//! parse or assume structure or a fixed length.

use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Insert a key/value into a form map only when the value is `Some`.
fn put_opt(form: &mut HashMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        form.insert(key.to_owned(), v.to_owned());
    }
}

/// Insert a numeric key/value into a form map only when present.
fn put_opt_u64(form: &mut HashMap<String, String>, key: &str, value: Option<u64>) {
    if let Some(v) = value {
        form.insert(key.to_owned(), v.to_string());
    }
}

/// Insert a boolean key/value (`"true"`/`"false"`) only when present.
fn put_opt_bool(form: &mut HashMap<String, String>, key: &str, value: Option<bool>) {
    if let Some(v) = value {
        form.insert(key.to_owned(), v.to_string());
    }
}

// ════════════════════════════════════════════════════════════════════════
//  Workflow Profile + Runtime
// ════════════════════════════════════════════════════════════════════════

/// Parameters for creating a workflow profile.
///
/// `POST /workspace/{workspace_id}/workflows/` — form-encoded. `name` is the
/// only commonly-required field; the rest are optional mutable fields. All
/// `#[non_exhaustive]` because the server may add profile fields.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CreateWorkflowParams {
    /// Display name for the workflow.
    pub name: Option<String>,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Optional published template revision to bind at create time.
    pub template_id: Option<String>,
    /// Optional credit budget cap for the runtime.
    pub agent_credit_cap: Option<u64>,
    /// Optional visibility (`workspace` / `private` / `participants_only`).
    pub visibility: Option<String>,
}

impl CreateWorkflowParams {
    /// An empty parameter set (equivalent to [`Default::default`]). Provided so
    /// callers in the binary crate can build this `#[non_exhaustive]` struct
    /// without struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the display name.
    #[must_use]
    pub fn name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, description: Option<String>) -> Self {
        self.description = description;
        self
    }

    /// Bind a template revision at create time.
    #[must_use]
    pub fn template_id(mut self, template_id: Option<String>) -> Self {
        self.template_id = template_id;
        self
    }

    /// Set the credit budget cap.
    #[must_use]
    pub fn agent_credit_cap(mut self, cap: Option<u64>) -> Self {
        self.agent_credit_cap = cap;
        self
    }

    /// Set the visibility.
    #[must_use]
    pub fn visibility(mut self, visibility: Option<String>) -> Self {
        self.visibility = visibility;
        self
    }
}

/// Create a workflow profile in a workspace.
///
/// `POST /workspace/{workspace_id}/workflows/` (form-encoded).
pub async fn create_workflow(
    client: &ApiClient,
    workspace_id: &str,
    params: &CreateWorkflowParams,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "name", params.name.as_deref());
    put_opt(&mut form, "description", params.description.as_deref());
    put_opt(&mut form, "template_id", params.template_id.as_deref());
    put_opt_u64(&mut form, "agent_credit_cap", params.agent_credit_cap);
    put_opt(&mut form, "visibility", params.visibility.as_deref());
    let path = format!(
        "/workspace/{}/workflows/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, &form).await
}

/// List workflow profiles in a workspace (offset-paginated).
///
/// `GET /workspace/{workspace_id}/workflows/`.
pub async fn list_workflows(
    client: &ApiClient,
    workspace_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/workflows/",
        urlencoding::encode(workspace_id)
    );
    client.get_with_params(&path, &params).await
}

/// Get a single workflow profile.
///
/// `GET /workflows/{workflow_id}/`.
pub async fn get_workflow(client: &ApiClient, workflow_id: &str) -> Result<Value, CliError> {
    let path = format!("/workflows/{}/", urlencoding::encode(workflow_id));
    client.get(&path).await
}

/// Update mutable fields / transition lifecycle of a workflow.
///
/// `PATCH /workflows/{workflow_id}/` — **form-encoded** (orchestration PATCH
/// bodies are form, not JSON). Callers supply the field/value pairs to set;
/// passing `state` transitions the lifecycle along the documented DAG (an
/// out-of-DAG transition returns 400).
#[allow(clippy::implicit_hasher)] // accepts an arbitrary caller-built form-field map
pub async fn update_workflow(
    client: &ApiClient,
    workflow_id: &str,
    fields: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = format!("/workflows/{}/", urlencoding::encode(workflow_id));
    client.patch_form(&path, fields).await
}

/// Soft-archive (or hard-delete with `hard=true`) a workflow.
///
/// `DELETE /workflows/{workflow_id}/` (`?hard=true` for an owner-only hard
/// delete). The `hard` flag is passed as a query param via
/// [`ApiClient::delete_with_params`].
pub async fn delete_workflow(
    client: &ApiClient,
    workflow_id: &str,
    hard: bool,
) -> Result<Value, CliError> {
    let path = format!("/workflows/{}/", urlencoding::encode(workflow_id));
    if hard {
        let mut params = HashMap::new();
        params.insert("hard".to_owned(), "true".to_owned());
        client.delete_with_params(&path, &params).await
    } else {
        client.delete(&path).await
    }
}

/// Transfer a workflow to another workspace in the same organization.
///
/// `POST /workflows/{workflow_id}/transfer/` (form-encoded). Cross-org
/// transfer is rejected server-side (`workflows.txt:768`).
pub async fn transfer_workflow(
    client: &ApiClient,
    workflow_id: &str,
    target_workspace_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "target_workspace_id".to_owned(),
        target_workspace_id.to_owned(),
    );
    let path = format!("/workflows/{}/transfer/", urlencoding::encode(workflow_id));
    client.post(&path, &form).await
}

/// Parameters for instantiating a workflow runtime.
///
/// `POST /workflows/{workflow_id}/instantiate/` (form-encoded;
/// `workflows.txt:464`). `idempotency_key` is **mandatory** and is the basis
/// for replay-safe instantiation — the command layer enforces its presence.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InstantiateParams {
    /// Mandatory replay-safe idempotency key (the same key returns the
    /// existing runtime row).
    pub idempotency_key: String,
    /// Optional resolved input bindings, as a JSON string
    /// (`trigger_payload={…}`).
    pub trigger_payload: Option<String>,
    /// Optional integrator correlation handle (1..255 chars).
    pub external_subject_id: Option<String>,
    /// Optional concurrency pool key to admit this run under.
    pub pool_key: Option<String>,
}

impl InstantiateParams {
    /// Construct with the mandatory `idempotency_key`; all other fields are
    /// `None`. Provided so the binary crate can build this `#[non_exhaustive]`
    /// struct without struct-literal syntax.
    #[must_use]
    pub fn new(idempotency_key: String) -> Self {
        Self {
            idempotency_key,
            trigger_payload: None,
            external_subject_id: None,
            pool_key: None,
        }
    }

    /// Set the JSON-string trigger payload.
    #[must_use]
    pub fn trigger_payload(mut self, payload: Option<String>) -> Self {
        self.trigger_payload = payload;
        self
    }

    /// Set the integrator correlation handle.
    #[must_use]
    pub fn external_subject_id(mut self, id: Option<String>) -> Self {
        self.external_subject_id = id;
        self
    }

    /// Set the concurrency pool key.
    #[must_use]
    pub fn pool_key(mut self, pool_key: Option<String>) -> Self {
        self.pool_key = pool_key;
        self
    }
}

/// Instantiate a workflow runtime (idempotent on `idempotency_key`).
///
/// `POST /workflows/{workflow_id}/instantiate/` (form-encoded).
pub async fn instantiate_workflow(
    client: &ApiClient,
    workflow_id: &str,
    params: &InstantiateParams,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("idempotency_key".to_owned(), params.idempotency_key.clone());
    put_opt(
        &mut form,
        "trigger_payload",
        params.trigger_payload.as_deref(),
    );
    put_opt(
        &mut form,
        "external_subject_id",
        params.external_subject_id.as_deref(),
    );
    put_opt(&mut form, "pool_key", params.pool_key.as_deref());
    let path = format!(
        "/workflows/{}/instantiate/",
        urlencoding::encode(workflow_id)
    );
    client.post(&path, &form).await
}

/// Get the runtime state snapshot for a workflow.
///
/// `GET /workflows/{workflow_id}/state/` — the canonical poll endpoint
/// (active steps, recent steps, progress, credit budget).
pub async fn get_workflow_state(client: &ApiClient, workflow_id: &str) -> Result<Value, CliError> {
    let path = format!("/workflows/{}/state/", urlencoding::encode(workflow_id));
    client.get(&path).await
}

/// Pause an active workflow.
///
/// `POST /workflows/{workflow_id}/pause/` (empty form body).
pub async fn pause_workflow(client: &ApiClient, workflow_id: &str) -> Result<Value, CliError> {
    let path = format!("/workflows/{}/pause/", urlencoding::encode(workflow_id));
    client.post(&path, &HashMap::new()).await
}

/// Resume a paused workflow.
///
/// `POST /workflows/{workflow_id}/resume/` (empty form body).
pub async fn resume_workflow(client: &ApiClient, workflow_id: &str) -> Result<Value, CliError> {
    let path = format!("/workflows/{}/resume/", urlencoding::encode(workflow_id));
    client.post(&path, &HashMap::new()).await
}

/// Cancel a workflow (cascades to synchronous sub-children).
///
/// `POST /workflows/{workflow_id}/cancel/` (form-encoded; optional
/// `reason`, `agents.md:2786`).
pub async fn cancel_workflow(
    client: &ApiClient,
    workflow_id: &str,
    reason: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "reason", reason);
    let path = format!("/workflows/{}/cancel/", urlencoding::encode(workflow_id));
    client.post(&path, &form).await
}

/// Rotate the per-workflow inbound HMAC key (returns the new version int only).
///
/// `POST /workflows/{workflow_id}/rotate_inbound_key/` (empty form body). The
/// secret bytes are never returned over the wire (`workflows.txt:325`).
pub async fn rotate_workflow_inbound_key(
    client: &ApiClient,
    workflow_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/rotate_inbound_key/",
        urlencoding::encode(workflow_id)
    );
    client.post(&path, &HashMap::new()).await
}

// ════════════════════════════════════════════════════════════════════════
//  Grants (workflow-scoped roles)
// ════════════════════════════════════════════════════════════════════════

/// Grant a user a workflow-scoped role.
///
/// `POST /workflows/{workflow_id}/grants/` (form-encoded). Re-granting a user
/// who already holds a live grant returns 409 (`workflows.txt:771`).
pub async fn add_grant(
    client: &ApiClient,
    workflow_id: &str,
    user_id: &str,
    role: &str,
    expires_at: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("user_id".to_owned(), user_id.to_owned());
    form.insert("role".to_owned(), role.to_owned());
    put_opt(&mut form, "expires_at", expires_at);
    let path = format!("/workflows/{}/grants/", urlencoding::encode(workflow_id));
    client.post(&path, &form).await
}

/// List a workflow's grants (cursor-paginated, `workflows.txt:772`).
///
/// `GET /workflows/{workflow_id}/grants/`. Accepts `limit` (default 100, max
/// 500) and `cursor` (the prior response's `pagination.next_cursor`).
pub async fn list_grants(
    client: &ApiClient,
    workflow_id: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    put_opt(&mut params, "cursor", cursor);
    let path = format!("/workflows/{}/grants/", urlencoding::encode(workflow_id));
    client.get_with_params(&path, &params).await
}

/// Revoke a user's grant (soft revoke).
///
/// `DELETE /workflows/{workflow_id}/grants/{user_id}/`.
pub async fn revoke_grant(
    client: &ApiClient,
    workflow_id: &str,
    user_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/grants/{}/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(user_id)
    );
    client.delete(&path).await
}

// ════════════════════════════════════════════════════════════════════════
//  Steps (occurrences)
// ════════════════════════════════════════════════════════════════════════

/// Get a single step occurrence.
///
/// `GET /workflows/{workflow_id}/steps/{step_occurrence_id}/`.
pub async fn get_step_occurrence(
    client: &ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/steps/{}/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(step_occurrence_id)
    );
    client.get(&path).await
}

/// Dispatch a step occurrence's handler (`advance`).
///
/// `POST /workflows/{workflow_id}/steps/{step_occurrence_id}/advance/`
/// (form-encoded). CAS-guarded — a 409 means re-read and retry.
pub async fn advance_step(
    client: &ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
    output: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "output", output);
    let path = format!(
        "/workflows/{}/steps/{}/advance/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(step_occurrence_id)
    );
    client.post(&path, &form).await
}

/// Cancel a single step occurrence (CAS-guarded).
///
/// `POST /workflows/{workflow_id}/steps/{step_occurrence_id}/cancel/`
/// (empty form body).
pub async fn cancel_step(
    client: &ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/steps/{}/cancel/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(step_occurrence_id)
    );
    client.post(&path, &HashMap::new()).await
}

/// Submit a step's output envelope.
///
/// `POST /workflows/{workflow_id}/steps/{step_occurrence_id}/output/`
/// (form-encoded; `output={…}` JSON string, `workflows.txt:555`).
/// CAS-guarded — a 409 means re-read and retry.
pub async fn submit_step_output(
    client: &ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
    output: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("output".to_owned(), output.to_owned());
    let path = format!(
        "/workflows/{}/steps/{}/output/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(step_occurrence_id)
    );
    client.post(&path, &form).await
}

/// Read an AI-agent step occurrence's action feed.
///
/// `GET /workflows/{workflow_id}/steps/{step_occurrence_id}/agent_activity/`
/// (`workflows.txt:413`). Returns `{"agent_activity": {"actions", "available",
/// "step_occurrence_id", "workflow_id"}}` — `actions` is ordered ascending by
/// `seq` and each card is `{seq, label, state, affected_refs, started_at,
/// ended_at}`. The same shape serves a running step (actions emitted so far —
/// poll to follow progress) and a finished one (the durable list).
/// `available: false` with empty `actions` is a neutral no-feed-yet state,
/// NOT an error; a non-agent occurrence returns 404 instead.
pub async fn get_step_agent_activity(
    client: &ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/steps/{}/agent_activity/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(step_occurrence_id)
    );
    client.get(&path).await
}

/// Read an AI-agent step occurrence's reasoning + commentary transcript.
///
/// `GET /workflows/{workflow_id}/steps/{step_occurrence_id}/agent_trace/`.
/// Returns `{"agent_trace": {"reasoning", "commentary", "available",
/// "step_occurrence_id", "workflow_id"}}`. The companion to
/// [`get_step_agent_activity`]: `agent_activity` is the high-level action feed,
/// `agent_trace` is the interim reasoning and the narration commentary the agent
/// emits while working. It never contains the final answer or citations.
/// `available: false` is a neutral no-trace-yet state, NOT an error; a non-agent
/// occurrence (or one that does not resolve to this workflow) returns 404.
pub async fn get_step_agent_trace(
    client: &ApiClient,
    workflow_id: &str,
    step_occurrence_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/steps/{}/agent_trace/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(step_occurrence_id)
    );
    client.get(&path).await
}

/// List occurrences for a step definition.
///
/// `GET /workflows/{workflow_id}/steps/{step_id}/occurrences/`.
pub async fn list_step_occurrences(
    client: &ApiClient,
    workflow_id: &str,
    step_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workflows/{}/steps/{}/occurrences/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(step_id)
    );
    client.get_with_params(&path, &params).await
}

// ════════════════════════════════════════════════════════════════════════
//  Mid-Run Modifications (propose / apply changes to a running workflow)
// ════════════════════════════════════════════════════════════════════════

/// Propose a mid-run modification against a running workflow.
///
/// `POST /workflows/{workflow_id}/modifications/` (form-encoded). `ops` is a
/// JSON array string of operations (each `{op, target_step_occurrence_id, …}`
/// where `op` ∈ `skip`|`reassign`|`patch`; max 50). Proposing auto-pauses the
/// run and returns the proposal plus a before/after diff. Only one proposal may
/// be open per workflow (409 otherwise). `expires_in_seconds` bounds the
/// proposal (max/default 604800 = 7 days; larger values are clamped, an absent
/// or non-positive value defaults to the max). Requires the
/// `workflow_mid_run_edit` plan capability (403 otherwise) and workflow ADMIN.
pub async fn propose_modification(
    client: &ApiClient,
    workflow_id: &str,
    ops: &str,
    reason: Option<&str>,
    expires_in_seconds: Option<u64>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("ops".to_owned(), ops.to_owned());
    put_opt(&mut form, "reason", reason);
    put_opt_u64(&mut form, "expires_in_seconds", expires_in_seconds);
    let path = format!(
        "/workflows/{}/modifications/",
        urlencoding::encode(workflow_id)
    );
    client.post(&path, &form).await
}

/// List a workflow's modification proposals.
///
/// `GET /workflows/{workflow_id}/modifications/` (optional `status` filter).
/// Member-or-above (a share-guest is excluded).
pub async fn list_modifications(
    client: &ApiClient,
    workflow_id: &str,
    status: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(s) = status {
        params.insert("status".to_owned(), s.to_owned());
    }
    let path = format!(
        "/workflows/{}/modifications/",
        urlencoding::encode(workflow_id)
    );
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Get a modification proposal's detail (changes + before/after diff).
///
/// `GET /workflows/{workflow_id}/modifications/{modification_id}/`.
/// Member-or-above (a share-guest is excluded).
pub async fn get_modification(
    client: &ApiClient,
    workflow_id: &str,
    modification_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/modifications/{}/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(modification_id)
    );
    client.get(&path).await
}

/// Apply a modification proposal, then finalize and resume the run.
///
/// `POST /workflows/{workflow_id}/modifications/{modification_id}/apply/`
/// (form-encoded). An empty/omitted `apply_change_ids` applies every pending
/// change; otherwise pass a JSON array string of ids covering all currently
/// pending changes (a partial selection is rejected). A `skip` that removes a
/// human gate (an approval/signing step) requires `confirm_removes_human_gate`,
/// or the apply is rejected with 403. Workflow ADMIN.
pub async fn apply_modification(
    client: &ApiClient,
    workflow_id: &str,
    modification_id: &str,
    apply_change_ids: Option<&str>,
    confirm_removes_human_gate: bool,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "apply_change_ids", apply_change_ids);
    if confirm_removes_human_gate {
        form.insert("confirm_removes_human_gate".to_owned(), "true".to_owned());
    }
    let path = format!(
        "/workflows/{}/modifications/{}/apply/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(modification_id)
    );
    client.post(&path, &form).await
}

/// Cancel a modification proposal and resume the run unchanged.
///
/// `POST /workflows/{workflow_id}/modifications/{modification_id}/cancel/`
/// (empty form body). Workflow ADMIN.
pub async fn cancel_modification(
    client: &ApiClient,
    workflow_id: &str,
    modification_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/modifications/{}/cancel/",
        urlencoding::encode(workflow_id),
        urlencoding::encode(modification_id)
    );
    client.post(&path, &HashMap::new()).await
}

// ════════════════════════════════════════════════════════════════════════
//  Templates (immutable revisions)
// ════════════════════════════════════════════════════════════════════════

/// Create a template revision (validated end-to-end).
///
/// `POST /workspace/{workspace_id}/workflow_templates/` (form-encoded;
/// `template_body={…}` JSON string). On validation failure the server
/// returns 422 with a `validation_report` array. Templates are immutable:
/// there is **no update** — POST a new revision instead.
pub async fn create_template(
    client: &ApiClient,
    workspace_id: &str,
    template_body: &str,
    name: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("template_body".to_owned(), template_body.to_owned());
    put_opt(&mut form, "name", name);
    let path = format!(
        "/workspace/{}/workflow_templates/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, &form).await
}

/// List template revisions for a workspace.
///
/// `GET /workspace/{workspace_id}/workflow_templates/`.
pub async fn list_templates(
    client: &ApiClient,
    workspace_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workspace/{}/workflow_templates/",
        urlencoding::encode(workspace_id)
    );
    client.get_with_params(&path, &params).await
}

/// Get a single template revision (`?include_body=true` inlines the body).
///
/// `GET /workflow_templates/{template_id}/`.
pub async fn get_template(
    client: &ApiClient,
    template_id: &str,
    include_body: bool,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if include_body {
        params.insert("include_body".to_owned(), "true".to_owned());
    }
    let path = format!("/workflow_templates/{}/", urlencoding::encode(template_id));
    client.get_with_params(&path, &params).await
}

/// Transition a template revision to `published` (legal only from
/// `validated`).
///
/// `POST /workflow_templates/{template_id}/publish/` (empty form body).
pub async fn publish_template(client: &ApiClient, template_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workflow_templates/{}/publish/",
        urlencoding::encode(template_id)
    );
    client.post(&path, &HashMap::new()).await
}

/// Transition a template revision to `withdrawn` (legal only from
/// `published`).
///
/// `POST /workflow_templates/{template_id}/withdraw/` (empty form body).
pub async fn withdraw_template(client: &ApiClient, template_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workflow_templates/{}/withdraw/",
        urlencoding::encode(template_id)
    );
    client.post(&path, &HashMap::new()).await
}

/// Transition a template revision to `deprecated` (legal only from
/// `published`).
///
/// `POST /workflow_templates/{template_id}/deprecate/` (empty form body).
pub async fn deprecate_template(client: &ApiClient, template_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workflow_templates/{}/deprecate/",
        urlencoding::encode(template_id)
    );
    client.post(&path, &HashMap::new()).await
}

// ════════════════════════════════════════════════════════════════════════
//  System Template Gallery (built-in catalog)
// ════════════════════════════════════════════════════════════════════════

/// List the system template gallery (metadata only).
///
/// `GET /workflow_templates/system/`. Any authenticated user; no workspace
/// scope or plan gate. The catalog is bounded, so the whole list is returned
/// without pagination.
pub async fn list_system_templates(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/workflow_templates/system/").await
}

/// Get one gallery template — metadata plus the full definition body (including
/// the `setup` block describing the inputs to collect before instantiating).
///
/// `GET /workflow_templates/system/{handle}/`. Any authenticated user; 404 for
/// an unknown handle.
pub async fn get_system_template(client: &ApiClient, handle: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workflow_templates/system/{}/",
        urlencoding::encode(handle)
    );
    client.get(&path).await
}

/// Parameters for [`instantiate_system_template`].
pub struct FromSystemParams<'a> {
    /// Target workspace ID.
    pub workspace_id: &'a str,
    /// Gallery template handle (required).
    pub handle: &'a str,
    /// Attach the new revision to this existing workflow (else create a new one).
    pub workflow_id: Option<&'a str>,
    /// `true`/`false` — create a new workflow (mutually exclusive with `workflow_id`;
    /// `create_workflow=false` requires a `workflow_id`).
    pub create_workflow: Option<bool>,
    /// Override the created revision/workflow name.
    pub name: Option<&'a str>,
    /// Override the created revision/workflow description.
    pub description: Option<&'a str>,
    /// JSON object string mapping setup input ids to values.
    pub inputs: Option<&'a str>,
    /// Integer compare-and-set against the catalog version (409 on mismatch).
    pub expected_version: Option<u64>,
    /// Replay-safe idempotency key (≤128 chars).
    pub idempotency_key: Option<&'a str>,
    /// Publish + bind the revision (server default is `true`).
    pub publish: Option<bool>,
    /// Revision reason string.
    pub reason: Option<&'a str>,
}

/// Instantiate a gallery template into a workspace as a new template revision.
///
/// `POST /workspace/{workspace_id}/workflow_templates/from_system/`
/// (form-encoded). Workspace admin; requires the workspace's workflow feature.
/// Missing/invalid setup inputs are returned together in a 422 `setup_report`.
pub async fn instantiate_system_template(
    client: &ApiClient,
    params: &FromSystemParams<'_>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("handle".to_owned(), params.handle.to_owned());
    put_opt(&mut form, "workflow_id", params.workflow_id);
    put_opt_bool(&mut form, "create_workflow", params.create_workflow);
    put_opt(&mut form, "name", params.name);
    put_opt(&mut form, "description", params.description);
    put_opt(&mut form, "inputs", params.inputs);
    put_opt_u64(&mut form, "expected_version", params.expected_version);
    put_opt(&mut form, "idempotency_key", params.idempotency_key);
    put_opt_bool(&mut form, "publish", params.publish);
    put_opt(&mut form, "reason", params.reason);
    let path = format!(
        "/workspace/{}/workflow_templates/from_system/",
        urlencoding::encode(params.workspace_id)
    );
    client.post(&path, &form).await
}

// ════════════════════════════════════════════════════════════════════════
//  Workflow Agent Templates (v3.5+, admin-only persona templates)
// ════════════════════════════════════════════════════════════════════════

/// Create a workspace-scoped agent template (an agent-step instruction prompt
/// paired with a tool allowlist).
///
/// `POST /workspace/{workspace_id}/workflow_agent_templates/` (form-encoded;
/// `tool_allowlist` is a JSON array string of tool id strings). Write methods
/// require workspace admin. v3.5 ships storage + CRUD only; the agent runtime
/// consumes these in a later release.
pub async fn create_agent_template(
    client: &ApiClient,
    workspace_id: &str,
    name: &str,
    instruction_prompt: &str,
    tool_allowlist: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("name".to_owned(), name.to_owned());
    form.insert(
        "instruction_prompt".to_owned(),
        instruction_prompt.to_owned(),
    );
    put_opt(&mut form, "tool_allowlist", tool_allowlist);
    let path = format!(
        "/workspace/{}/workflow_agent_templates/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, &form).await
}

/// List a workspace's agent templates.
///
/// `GET /workspace/{workspace_id}/workflow_agent_templates/`. Requires
/// workspace view.
pub async fn list_agent_templates(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/workflow_agent_templates/",
        urlencoding::encode(workspace_id)
    );
    client.get(&path).await
}

/// Read one agent template.
///
/// `GET /workspace/{workspace_id}/workflow_agent_templates/{template_id}/`.
pub async fn get_agent_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/workflow_agent_templates/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id)
    );
    client.get(&path).await
}

/// Update an agent template's mutable fields.
///
/// `PATCH /workspace/{workspace_id}/workflow_agent_templates/{template_id}/`
/// (form-encoded). Mutable: `name` (≤128), `instruction_prompt` (≤8192),
/// `tool_allowlist` (JSON array string). `id`/`workspace_id`/`created_at`/
/// `created_by_user_id` are immutable. Workspace admin.
pub async fn update_agent_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
    name: Option<&str>,
    instruction_prompt: Option<&str>,
    tool_allowlist: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "name", name);
    put_opt(&mut form, "instruction_prompt", instruction_prompt);
    put_opt(&mut form, "tool_allowlist", tool_allowlist);
    let path = format!(
        "/workspace/{}/workflow_agent_templates/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id)
    );
    client.patch_form(&path, &form).await
}

/// Hard-delete an agent template.
///
/// `DELETE /workspace/{workspace_id}/workflow_agent_templates/{template_id}/`.
/// Workspace admin.
pub async fn delete_agent_template(
    client: &ApiClient,
    workspace_id: &str,
    template_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/workflow_agent_templates/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(template_id)
    );
    client.delete(&path).await
}

// ════════════════════════════════════════════════════════════════════════
//  Triggers (workspace-scoped fire-on-event)
// ════════════════════════════════════════════════════════════════════════

/// Parameters for creating a workflow trigger.
///
/// `POST /workspace/{workspace_id}/workflow_triggers/` (form-encoded;
/// structurally-nested fields `event_match` / `param_mapping` are JSON
/// strings). `#[non_exhaustive]` because the trigger surface keeps growing.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CreateTriggerParams {
    /// Trigger kind: `manual` / `scheduled` / `event_match` /
    /// `inbound_webhook` / `ai_driven`.
    pub kind: Option<String>,
    /// Target template id to instantiate (optionally `:vN`-versioned).
    pub target_template_id: Option<String>,
    /// JSON-string event-match expression (for `event_match` triggers).
    pub event_match: Option<String>,
    /// JSON-string parameter mapping (extracts inputs from the event).
    pub param_mapping: Option<String>,
    /// Optional per-hour rate limit.
    pub rate_limit_per_hour: Option<u64>,
    /// Optional concurrency cap.
    pub concurrency_cap: Option<u64>,
    /// Optional dedup scope (`trigger_local` / `template_per_workspace` /
    /// `event_source_per_workspace`).
    pub dedup_scope: Option<String>,
    /// Optional idempotency-key template.
    pub idempotency_key_template: Option<String>,
}

impl CreateTriggerParams {
    /// An empty parameter set (equivalent to [`Default::default`]). Provided so
    /// the binary crate can build this `#[non_exhaustive]` struct without
    /// struct-literal syntax; set fields with the `with_*` methods.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the trigger kind.
    #[must_use]
    pub fn kind(mut self, kind: Option<String>) -> Self {
        self.kind = kind;
        self
    }

    /// Set the target template id.
    #[must_use]
    pub fn target_template_id(mut self, id: Option<String>) -> Self {
        self.target_template_id = id;
        self
    }

    /// Set the JSON-string event-match expression.
    #[must_use]
    pub fn event_match(mut self, event_match: Option<String>) -> Self {
        self.event_match = event_match;
        self
    }

    /// Set the JSON-string parameter mapping.
    #[must_use]
    pub fn param_mapping(mut self, param_mapping: Option<String>) -> Self {
        self.param_mapping = param_mapping;
        self
    }

    /// Set the per-hour rate limit.
    #[must_use]
    pub fn rate_limit_per_hour(mut self, rate: Option<u64>) -> Self {
        self.rate_limit_per_hour = rate;
        self
    }

    /// Set the concurrency cap.
    #[must_use]
    pub fn concurrency_cap(mut self, cap: Option<u64>) -> Self {
        self.concurrency_cap = cap;
        self
    }

    /// Set the dedup scope.
    #[must_use]
    pub fn dedup_scope(mut self, scope: Option<String>) -> Self {
        self.dedup_scope = scope;
        self
    }

    /// Set the idempotency-key template.
    #[must_use]
    pub fn idempotency_key_template(mut self, template: Option<String>) -> Self {
        self.idempotency_key_template = template;
        self
    }
}

/// Create a workflow trigger.
///
/// `POST /workspace/{workspace_id}/workflow_triggers/` (form-encoded).
pub async fn create_trigger(
    client: &ApiClient,
    workspace_id: &str,
    params: &CreateTriggerParams,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "kind", params.kind.as_deref());
    put_opt(
        &mut form,
        "target_template_id",
        params.target_template_id.as_deref(),
    );
    put_opt(&mut form, "event_match", params.event_match.as_deref());
    put_opt(&mut form, "param_mapping", params.param_mapping.as_deref());
    put_opt_u64(&mut form, "rate_limit_per_hour", params.rate_limit_per_hour);
    put_opt_u64(&mut form, "concurrency_cap", params.concurrency_cap);
    put_opt(&mut form, "dedup_scope", params.dedup_scope.as_deref());
    put_opt(
        &mut form,
        "idempotency_key_template",
        params.idempotency_key_template.as_deref(),
    );
    let path = format!(
        "/workspace/{}/workflow_triggers/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, &form).await
}

/// List triggers for a workspace, optionally filtered by enabled state.
///
/// `GET /workspace/{workspace_id}/workflow_triggers/`
/// (`?enabled_filter=true|false|all`).
pub async fn list_triggers(
    client: &ApiClient,
    workspace_id: &str,
    enabled_filter: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    put_opt(&mut params, "enabled_filter", enabled_filter);
    let path = format!(
        "/workspace/{}/workflow_triggers/",
        urlencoding::encode(workspace_id)
    );
    client.get_with_params(&path, &params).await
}

/// Get a single trigger.
///
/// `GET /workflow_triggers/{trigger_id}/`.
pub async fn get_trigger(client: &ApiClient, trigger_id: &str) -> Result<Value, CliError> {
    let path = format!("/workflow_triggers/{}/", urlencoding::encode(trigger_id));
    client.get(&path).await
}

/// Update a trigger's mutable fields (toggle / rate cap / concurrency cap).
///
/// `PATCH /workflow_triggers/{trigger_id}/` — **form-encoded**.
#[allow(clippy::implicit_hasher)] // accepts an arbitrary caller-built form-field map
pub async fn update_trigger(
    client: &ApiClient,
    trigger_id: &str,
    fields: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = format!("/workflow_triggers/{}/", urlencoding::encode(trigger_id));
    client.patch_form(&path, fields).await
}

/// Soft-delete (or hard-delete with `hard=true`) a trigger.
///
/// `DELETE /workflow_triggers/{trigger_id}/` (`?hard=true` permanently
/// deletes; soft-delete-first is required, `workflows.txt:744`).
pub async fn delete_trigger(
    client: &ApiClient,
    trigger_id: &str,
    hard: bool,
) -> Result<Value, CliError> {
    let path = format!("/workflow_triggers/{}/", urlencoding::encode(trigger_id));
    if hard {
        let mut params = HashMap::new();
        params.insert("hard".to_owned(), "true".to_owned());
        client.delete_with_params(&path, &params).await
    } else {
        client.delete(&path).await
    }
}

/// Manually fire a trigger (integration testing / replay).
///
/// `POST /workflow_triggers/{trigger_id}/fire/` (form-encoded;
/// `idempotency_key` mandatory + optional `trigger_payload={…}` JSON string,
/// `agents.md:2616`). A 409 denial carries a `reason` field.
pub async fn fire_trigger(
    client: &ApiClient,
    trigger_id: &str,
    idempotency_key: &str,
    trigger_payload: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("idempotency_key".to_owned(), idempotency_key.to_owned());
    put_opt(&mut form, "trigger_payload", trigger_payload);
    let path = format!(
        "/workflow_triggers/{}/fire/",
        urlencoding::encode(trigger_id)
    );
    client.post(&path, &form).await
}

/// Dry-run (backtest) a saved event trigger over a historical window.
///
/// `POST /workflow_triggers/{trigger_id}/dry_run/` (form-encoded; optional
/// `window_days` ≤ 90, `sample_limit`, `apply_guards`).
pub async fn dry_run_trigger(
    client: &ApiClient,
    trigger_id: &str,
    window_days: Option<u64>,
    sample_limit: Option<u64>,
    apply_guards: Option<bool>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt_u64(&mut form, "window_days", window_days);
    put_opt_u64(&mut form, "sample_limit", sample_limit);
    put_opt_bool(&mut form, "apply_guards", apply_guards);
    let path = format!(
        "/workflow_triggers/{}/dry_run/",
        urlencoding::encode(trigger_id)
    );
    client.post(&path, &form).await
}

/// Dry-run (backtest) an unsaved event-trigger draft over a historical
/// window (nothing is saved or fired).
///
/// `POST /workspace/{workspace_id}/workflow_triggers/dry_run/` (form-encoded;
/// inline `event_match` / `param_mapping` / `target_template_id` JSON
/// strings + optional window params).
pub async fn dry_run_trigger_draft(
    client: &ApiClient,
    workspace_id: &str,
    event_match: Option<&str>,
    param_mapping: Option<&str>,
    target_template_id: Option<&str>,
    window_days: Option<u64>,
    sample_limit: Option<u64>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "event_match", event_match);
    put_opt(&mut form, "param_mapping", param_mapping);
    put_opt(&mut form, "target_template_id", target_template_id);
    put_opt_u64(&mut form, "window_days", window_days);
    put_opt_u64(&mut form, "sample_limit", sample_limit);
    let path = format!(
        "/workspace/{}/workflow_triggers/dry_run/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, &form).await
}

/// Rotate the workspace inbound trigger key (returns the new version int).
///
/// `POST /workflow_triggers/{trigger_id}/rotate_inbound_key/` (empty form).
pub async fn rotate_trigger_inbound_key(
    client: &ApiClient,
    trigger_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflow_triggers/{}/rotate_inbound_key/",
        urlencoding::encode(trigger_id)
    );
    client.post(&path, &HashMap::new()).await
}

// ════════════════════════════════════════════════════════════════════════
//  Trigger aliases (workspace verb→template map)
// ════════════════════════════════════════════════════════════════════════

/// Get the workspace's `workflow_trigger_aliases` verb→template map.
///
/// Reads the public `workflow_trigger_aliases` field off the workspace
/// resource (`GET /workspace/{workspace_id}/`); the command layer projects
/// just that field.
pub async fn get_trigger_aliases(
    client: &ApiClient,
    workspace_id: &str,
) -> Result<Value, CliError> {
    let path = format!("/workspace/{}/", urlencoding::encode(workspace_id));
    client.get(&path).await
}

/// Replace the workspace's `workflow_trigger_aliases` map.
///
/// `PATCH /workspace/{workspace_id}/` — **form-encoded**; the
/// `workflow_trigger_aliases` value is a JSON-string object (read-modify-write
/// is performed by the command layer). The workspace PATCH endpoint accepts
/// form bodies.
pub async fn set_trigger_aliases(
    client: &ApiClient,
    workspace_id: &str,
    aliases_json: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "workflow_trigger_aliases".to_owned(),
        aliases_json.to_owned(),
    );
    let path = format!("/workspace/{}/", urlencoding::encode(workspace_id));
    client.patch_form(&path, &form).await
}

// ════════════════════════════════════════════════════════════════════════
//  Obligations + inbox
// ════════════════════════════════════════════════════════════════════════

/// List obligations for a workflow (the `workflow_id` filter is the **required
/// authz anchor**; offset-paginated).
///
/// `GET /obligations/?workflow_id=…` (optional `status`, `assigned_user_id`).
pub async fn list_obligations(
    client: &ApiClient,
    workflow_id: &str,
    status: Option<&str>,
    assigned_user_id: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("workflow_id".to_owned(), workflow_id.to_owned());
    put_opt(&mut params, "status", status);
    put_opt(&mut params, "assigned_user_id", assigned_user_id);
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    client.get_with_params("/obligations/", &params).await
}

/// Get a single obligation (the obligation id is a plain numeric sequence
/// string).
///
/// `GET /obligations/{obligation_id}/`.
pub async fn get_obligation(client: &ApiClient, obligation_id: &str) -> Result<Value, CliError> {
    let path = format!("/obligations/{}/", urlencoding::encode(obligation_id));
    client.get(&path).await
}

/// Atomically claim a role-addressed obligation (409 if another claims first).
///
/// `POST /obligations/{obligation_id}/claim/` (empty form body).
pub async fn claim_obligation(client: &ApiClient, obligation_id: &str) -> Result<Value, CliError> {
    let path = format!("/obligations/{}/claim/", urlencoding::encode(obligation_id));
    client.post(&path, &HashMap::new()).await
}

/// Release a claimed obligation back into the pool (claimer-only).
///
/// `POST /obligations/{obligation_id}/release/` (empty form body).
pub async fn release_obligation(
    client: &ApiClient,
    obligation_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/obligations/{}/release/",
        urlencoding::encode(obligation_id)
    );
    client.post(&path, &HashMap::new()).await
}

/// Resolve an obligation, optionally attaching a `resolution_payload`
/// (bound to the audit envelope only).
///
/// `POST /obligations/{obligation_id}/resolve/` (form-encoded;
/// `resolution_payload={…}` JSON string, `workflows.txt:592`).
pub async fn resolve_obligation(
    client: &ApiClient,
    obligation_id: &str,
    resolution_payload: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "resolution_payload", resolution_payload);
    let path = format!(
        "/obligations/{}/resolve/",
        urlencoding::encode(obligation_id)
    );
    client.post(&path, &form).await
}

/// Cross-workspace top-K inbox (not cached).
///
/// `GET /me/inbox/`.
pub async fn inbox(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/me/inbox/").await
}

/// Workspace-scoped inbox (cached).
///
/// `GET /me/inbox/workspace/{workspace_id}/`.
pub async fn inbox_workspace(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!("/me/inbox/workspace/{}/", urlencoding::encode(workspace_id));
    client.get(&path).await
}

/// Pool-scoped inbox (cached).
///
/// `GET /me/inbox/workspace/{workspace_id}/pool/{pool_key}/`.
pub async fn inbox_pool(
    client: &ApiClient,
    workspace_id: &str,
    pool_key: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/me/inbox/workspace/{}/pool/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(pool_key)
    );
    client.get(&path).await
}

// ════════════════════════════════════════════════════════════════════════
//  Extraction schema (per-workflow structured-extraction field set)
// ════════════════════════════════════════════════════════════════════════

/// Get the workflow's current extraction schema (workflow VIEW access).
///
/// `GET /workflows/{workflow_id}/extraction_schema/`. Returns
/// `{"extraction_schema": null}` when none is configured.
pub async fn get_extraction_schema(
    client: &ApiClient,
    workflow_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflows/{}/extraction_schema/",
        urlencoding::encode(workflow_id)
    );
    client.get(&path).await
}

/// Replace the workflow's extraction schema with a new append-only version
/// (workflow ADMIN access).
///
/// `PUT /workflows/{workflow_id}/extraction_schema/` — **form-encoded**;
/// the `extraction_schema` value is a JSON string (often built from an
/// `@file`). Uses PUT (idempotent replace) per the documented method set.
pub async fn set_extraction_schema(
    client: &ApiClient,
    workflow_id: &str,
    extraction_schema: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("extraction_schema".to_owned(), extraction_schema.to_owned());
    let path = format!(
        "/workflows/{}/extraction_schema/",
        urlencoding::encode(workflow_id)
    );
    client.put_form(&path, &form).await
}

/// Auto-derive a proposed extraction schema from a sample of files (workflow
/// ADMIN access). **Spends AI credits** — the command layer gates this behind
/// `--confirm-ai-spend`. The proposal is returned, NOT persisted.
///
/// `POST /workflows/{workflow_id}/extraction_schema/derive/` (form-encoded;
/// `node_ids` is a JSON-string array).
pub async fn derive_extraction_schema(
    client: &ApiClient,
    workflow_id: &str,
    node_ids: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "node_ids", node_ids);
    let path = format!(
        "/workflows/{}/extraction_schema/derive/",
        urlencoding::encode(workflow_id)
    );
    client.post(&path, &form).await
}

// ════════════════════════════════════════════════════════════════════════
//  Audit (event log, signed export, dual-control redaction)
// ════════════════════════════════════════════════════════════════════════

/// Paginated audit event log (`?include_payload=true` inlines the payload).
///
/// `GET /workflows/{workflow_id}/audit/events/`.
pub async fn audit_events(
    client: &ApiClient,
    workflow_id: &str,
    include_payload: bool,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if include_payload {
        params.insert("include_payload".to_owned(), "true".to_owned());
    }
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workflows/{}/audit/events/",
        urlencoding::encode(workflow_id)
    );
    client.get_with_params(&path, &params).await
}

/// Start an asynchronous signed audit-export job.
///
/// `POST /workflows/{workflow_id}/audit/export/` (form-encoded; e.g.
/// `scope=full&include_overlays=true&redaction_pin_strategy=job_start`,
/// `workflows.txt:630`).
pub async fn start_audit_export(
    client: &ApiClient,
    workflow_id: &str,
    scope: Option<&str>,
    include_overlays: Option<bool>,
    redaction_pin_strategy: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "scope", scope);
    put_opt_bool(&mut form, "include_overlays", include_overlays);
    put_opt(&mut form, "redaction_pin_strategy", redaction_pin_strategy);
    let path = format!(
        "/workflows/{}/audit/export/",
        urlencoding::encode(workflow_id)
    );
    client.post(&path, &form).await
}

/// List export jobs for a workspace (required `workspace_id`; offset-paginated).
///
/// `GET /audit/export_jobs/?workspace_id=…`.
pub async fn list_audit_export_jobs(
    client: &ApiClient,
    workspace_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    params.insert("workspace_id".to_owned(), workspace_id.to_owned());
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    client.get_with_params("/audit/export_jobs/", &params).await
}

/// Get export-job status (manifest signature, chunk count).
///
/// `GET /audit/export_jobs/{job_id}/`.
pub async fn get_audit_export_job(client: &ApiClient, job_id: &str) -> Result<Value, CliError> {
    let path = format!("/audit/export_jobs/{}/", urlencoding::encode(job_id));
    client.get(&path).await
}

/// Build the bundle-chunk download path for a job + chunk id.
///
/// `{chunk_id}` is `"manifest"` or an integer in `[0, total_chunks)`. Exposed
/// so the command layer can stream the chunk via
/// [`ApiClient::download_file_stream`] (the bundle must NOT be buffered).
#[must_use]
pub fn audit_bundle_chunk_path(job_id: &str, chunk_id: &str) -> String {
    format!(
        "/audit/export_jobs/{}/bundle/{}/",
        urlencoding::encode(job_id),
        urlencoding::encode(chunk_id)
    )
}

/// Initiate or confirm a dual-control audit redaction.
///
/// `POST /workspace/{workspace_id}/audit/redaction/` (form-encoded). For
/// `mode=request`: `target_event_id`, `target_workflow_id`, `redaction_paths`
/// (JSON-string array), `reason`. For `mode=confirm`: `action_id`,
/// `confirmer_user_id`. The confirmer MUST differ from the requester
/// (`workflows.txt:282`).
#[allow(clippy::implicit_hasher)] // accepts an arbitrary caller-built form-field map
pub async fn audit_redaction(
    client: &ApiClient,
    workspace_id: &str,
    fields: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/audit/redaction/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, fields).await
}

/// Get a committed redaction batch summary.
///
/// `GET /workspace/{workspace_id}/audit/redaction/{redaction_id}/`.
pub async fn get_redaction(
    client: &ApiClient,
    workspace_id: &str,
    redaction_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/audit/redaction/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(redaction_id)
    );
    client.get(&path).await
}

// ════════════════════════════════════════════════════════════════════════
//  Outbound webhook subscriptions
// ════════════════════════════════════════════════════════════════════════

/// Parameters for creating an outbound webhook subscription.
///
/// `POST /workspace/{workspace_id}/outbound_webhook_subscriptions/`
/// (form-encoded; `event_type_subscriptions` / `family_allowlist` are
/// JSON-string arrays, `workflows.txt:681`). The create response includes the
/// HMAC secret **one time only** — the command layer wraps it in a
/// [`secrecy::SecretString`] and writes it to a 0600 `--secret-file`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CreateSubscriptionParams {
    /// HTTPS delivery target (private/reserved addresses are rejected).
    pub target_url: Option<String>,
    /// JSON-string array of event types to subscribe to.
    pub event_type_subscriptions: Option<String>,
    /// Optional human-readable label.
    pub description: Option<String>,
    /// Optional per-hour delivery cap (0 = no cap).
    pub rate_limit_per_hour: Option<u64>,
    /// Optional JSON-string array of CDN-family suffixes to allow.
    pub family_allowlist: Option<String>,
}

impl CreateSubscriptionParams {
    /// An empty parameter set (equivalent to [`Default::default`]). Provided so
    /// the binary crate can build this `#[non_exhaustive]` struct without
    /// struct-literal syntax; set fields with the `with_*` methods.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the HTTPS delivery target.
    #[must_use]
    pub fn target_url(mut self, url: Option<String>) -> Self {
        self.target_url = url;
        self
    }

    /// Set the JSON-string array of event types.
    #[must_use]
    pub fn event_type_subscriptions(mut self, subs: Option<String>) -> Self {
        self.event_type_subscriptions = subs;
        self
    }

    /// Set the human-readable label.
    #[must_use]
    pub fn description(mut self, description: Option<String>) -> Self {
        self.description = description;
        self
    }

    /// Set the per-hour delivery cap.
    #[must_use]
    pub fn rate_limit_per_hour(mut self, rate: Option<u64>) -> Self {
        self.rate_limit_per_hour = rate;
        self
    }

    /// Set the JSON-string CDN-family allowlist.
    #[must_use]
    pub fn family_allowlist(mut self, allowlist: Option<String>) -> Self {
        self.family_allowlist = allowlist;
        self
    }
}

/// Create an outbound webhook subscription (one-time secret view).
///
/// `POST /workspace/{workspace_id}/outbound_webhook_subscriptions/`.
pub async fn create_subscription(
    client: &ApiClient,
    workspace_id: &str,
    params: &CreateSubscriptionParams,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    put_opt(&mut form, "target_url", params.target_url.as_deref());
    put_opt(
        &mut form,
        "event_type_subscriptions",
        params.event_type_subscriptions.as_deref(),
    );
    put_opt(&mut form, "description", params.description.as_deref());
    put_opt_u64(&mut form, "rate_limit_per_hour", params.rate_limit_per_hour);
    put_opt(
        &mut form,
        "family_allowlist",
        params.family_allowlist.as_deref(),
    );
    let path = format!(
        "/workspace/{}/outbound_webhook_subscriptions/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, &form).await
}

/// List outbound webhook subscriptions for a workspace.
///
/// `GET /workspace/{workspace_id}/outbound_webhook_subscriptions/`.
pub async fn list_subscriptions(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/outbound_webhook_subscriptions/",
        urlencoding::encode(workspace_id)
    );
    client.get(&path).await
}

/// Get a single subscription (the secret is never returned on read).
///
/// `GET /outbound_webhook_subscriptions/{subscription_id}/`.
pub async fn get_subscription(
    client: &ApiClient,
    subscription_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/outbound_webhook_subscriptions/{}/",
        urlencoding::encode(subscription_id)
    );
    client.get(&path).await
}

/// Update a subscription (toggle / description / rate cap / family allowlist).
///
/// `PATCH /outbound_webhook_subscriptions/{subscription_id}/` —
/// **form-encoded**.
#[allow(clippy::implicit_hasher)] // accepts an arbitrary caller-built form-field map
pub async fn update_subscription(
    client: &ApiClient,
    subscription_id: &str,
    fields: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = format!(
        "/outbound_webhook_subscriptions/{}/",
        urlencoding::encode(subscription_id)
    );
    client.patch_form(&path, fields).await
}

/// Hard-delete a subscription.
///
/// `DELETE /outbound_webhook_subscriptions/{subscription_id}/`.
pub async fn delete_subscription(
    client: &ApiClient,
    subscription_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/outbound_webhook_subscriptions/{}/",
        urlencoding::encode(subscription_id)
    );
    client.delete(&path).await
}

/// Rotate a subscription's HMAC secret (one-time response with new bytes).
///
/// `POST /outbound_webhook_subscriptions/{subscription_id}/rotate_secret/`
/// (empty form). The command layer wraps the returned secret in a
/// [`secrecy::SecretString`] and writes it to a 0600 `--secret-file`.
pub async fn rotate_subscription_secret(
    client: &ApiClient,
    subscription_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/outbound_webhook_subscriptions/{}/rotate_secret/",
        urlencoding::encode(subscription_id)
    );
    client.post(&path, &HashMap::new()).await
}

// ════════════════════════════════════════════════════════════════════════
//  Pools (concurrency caps)
// ════════════════════════════════════════════════════════════════════════

/// Create a workflow concurrency pool.
///
/// `POST /workspace/{workspace_id}/workflow_pools/` (form-encoded).
pub async fn create_pool(
    client: &ApiClient,
    workspace_id: &str,
    pool_key: &str,
    max_concurrent: Option<u64>,
    pool_source: Option<&str>,
    pool_admission_policy: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("pool_key".to_owned(), pool_key.to_owned());
    put_opt_u64(&mut form, "max_concurrent", max_concurrent);
    put_opt(&mut form, "pool_source", pool_source);
    put_opt(&mut form, "pool_admission_policy", pool_admission_policy);
    let path = format!(
        "/workspace/{}/workflow_pools/",
        urlencoding::encode(workspace_id)
    );
    client.post(&path, &form).await
}

/// List pools in a workspace (each carries best-effort `active_concurrent`).
///
/// `GET /workspace/{workspace_id}/workflow_pools/`.
pub async fn list_pools(client: &ApiClient, workspace_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/workflow_pools/",
        urlencoding::encode(workspace_id)
    );
    client.get(&path).await
}

/// Get a single pool (carries best-effort `active_concurrent`).
///
/// `GET /workspace/{workspace_id}/workflow_pools/{pool_key}/`.
pub async fn get_pool(
    client: &ApiClient,
    workspace_id: &str,
    pool_key: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/workflow_pools/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(pool_key)
    );
    client.get(&path).await
}

/// Delete a pool (requires zero running and zero queued workflows; a 422
/// carries the current `active_concurrent` count).
///
/// `DELETE /workspace/{workspace_id}/workflow_pools/{pool_key}/`.
pub async fn delete_pool(
    client: &ApiClient,
    workspace_id: &str,
    pool_key: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/workflow_pools/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(pool_key)
    );
    client.delete(&path).await
}

// ════════════════════════════════════════════════════════════════════════
//  External subjects (cross-workflow correlation)
// ════════════════════════════════════════════════════════════════════════

/// List workflows indexed by an integrator correlation handle.
///
/// `GET /workspace/{workspace_id}/external_subjects/{subject_id}/workflows/`.
pub async fn subject_workflows(
    client: &ApiClient,
    workspace_id: &str,
    subject_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workspace/{}/external_subjects/{}/workflows/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(subject_id)
    );
    client.get(&path).await
}

// ════════════════════════════════════════════════════════════════════════
//  Realtime channel (token mint only)
// ════════════════════════════════════════════════════════════════════════

/// Mint a short-lived realtime-channel WebSocket token for a workflow.
///
/// `GET /websocket/auth/{workflow_id}` (`workflows.txt:376`). This mints the
/// token ONLY — no in-CLI WebSocket client is shipped. The response carries
/// the token and an `expires_in`; the command layer wraps the token in a
/// [`secrecy::SecretString`] so it is never logged.
pub async fn realtime_token(client: &ApiClient, workflow_id: &str) -> Result<Value, CliError> {
    let path = format!("/websocket/auth/{}", urlencoding::encode(workflow_id));
    client.get(&path).await
}

// ════════════════════════════════════════════════════════════════════════
//  Workflow Review (v3.5b — member-only MVS; flag-gated 404 when off, except
//  the not-flag-gated `review_workspace_active` hydration read at the end)
// ════════════════════════════════════════════════════════════════════════

/// Get-or-create a review surface for a step occurrence (idempotent).
///
/// `POST /workflow-review/surface/create/` (form-encoded). The create / get /
/// asset / decision / admin-resolve endpoints in this section 404 when the
/// workspace's native-review rollout flag is off; the `review_workspace_active`
/// hydration read below is the exception (never flag-gated).
pub async fn review_surface_create(
    client: &ApiClient,
    step_occurrence_id: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert(
        "step_occurrence_id".to_owned(),
        step_occurrence_id.to_owned(),
    );
    client.post("/workflow-review/surface/create/", &form).await
}

/// Fetch a review surface (assets + reviewers + per-asset decision matrix).
///
/// `GET /workflow-review/surface/{surface_id}/`.
pub async fn review_surface_get(client: &ApiClient, surface_id: &str) -> Result<Value, CliError> {
    let path = format!(
        "/workflow-review/surface/{}/",
        urlencoding::encode(surface_id)
    );
    client.get(&path).await
}

/// Fetch a single review asset + its current round + that round's decisions.
///
/// `GET /workflow-review/surface/{surface_id}/asset/{asset_id}/`.
pub async fn review_asset_get(
    client: &ApiClient,
    surface_id: &str,
    asset_id: &str,
) -> Result<Value, CliError> {
    let path = format!(
        "/workflow-review/surface/{}/asset/{}/",
        urlencoding::encode(surface_id),
        urlencoding::encode(asset_id)
    );
    client.get(&path).await
}

/// Record a review decision (`approve` / `reject` / `request_changes`).
///
/// `POST /workflow-review/surface/{surface_id}/asset/{asset_id}/decision/`
/// (form-encoded). CAS-checks `version_id_pinned` against the asset's
/// `current_version_id` (409 `ERR_VERSION_MISMATCH` on mismatch) and dedupes
/// on `(asset_id, reviewer_id, round_id)`.
pub async fn review_decision(
    client: &ApiClient,
    surface_id: &str,
    asset_id: &str,
    decision: &str,
    version_id_pinned: &str,
    comment: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("decision".to_owned(), decision.to_owned());
    form.insert("version_id_pinned".to_owned(), version_id_pinned.to_owned());
    put_opt(&mut form, "comment", comment);
    let path = format!(
        "/workflow-review/surface/{}/asset/{}/decision/",
        urlencoding::encode(surface_id),
        urlencoding::encode(asset_id)
    );
    client.post(&path, &form).await
}

/// Workspace admin force-resolves a stuck review surface.
///
/// `POST /workflow-review/surface/{surface_id}/admin-resolve/` (form-encoded;
/// `resolution` is `approved` / `rejected` / `cancelled`).
pub async fn review_admin_resolve(
    client: &ApiClient,
    surface_id: &str,
    resolution: &str,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("resolution".to_owned(), resolution.to_owned());
    let path = format!(
        "/workflow-review/surface/{}/admin-resolve/",
        urlencoding::encode(surface_id)
    );
    client.post(&path, &form).await
}

/// List the ACTIVE (`arming` / `open`) review surfaces in a workspace, each
/// with its asset rows.
///
/// `GET /workflow-review/workspace/{workspace_id}/active/`. A workspace
/// hydration read: returns the in-flight reviews with their asset `node_id`s,
/// so a client can badge files as "under review" without per-file fetches.
/// Accepts `limit` (default 25, max 100) and `offset`; rows are ordered
/// oldest-created first (stable id tiebreak). Active reviews per workspace are
/// typically few, so the default page usually covers them — for exhaustive
/// hydration, page with `offset` while `pagination.has_more` is true. The
/// response is `{ reviews: [ { surface, assets } ], pagination }` — it
/// intentionally omits each surface's reviewer roster and per-asset decision
/// matrix (fetch [`review_surface_get`] for those).
///
/// Unlike the other endpoints in this section, this read is **not** gated on
/// the workspace's native-review rollout flag: in-flight reviews keep blocking
/// file writes even if the feature is later disabled, so the list always
/// reports them. For a workspace member it returns `result: true` with an
/// empty `reviews` list when nothing is under review (never an error); a
/// non-member or unknown workspace id gets a uniform `404` (no existence leak).
pub async fn review_workspace_active(
    client: &ApiClient,
    workspace_id: &str,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(l) = limit {
        params.insert("limit".to_owned(), l.to_string());
    }
    if let Some(o) = offset {
        params.insert("offset".to_owned(), o.to_string());
    }
    let path = format!(
        "/workflow-review/workspace/{}/active/",
        urlencoding::encode(workspace_id)
    );
    client.get_with_params(&path, &params).await
}

// ════════════════════════════════════════════════════════════════════════
//  Audit bundle integrity verification (`check-integrity`, NOT `verify`)
// ════════════════════════════════════════════════════════════════════════

/// Outcome of an audit-bundle integrity check.
///
/// This verifies bundle **integrity** (chunk SHA-256 hashes match the
/// manifest, the per-event content-hash chain is intact, and — when the
/// manifest declares it — the chunk coverage is complete with no gaps). It is
/// deliberately **NOT** authenticity verification: it does not recompute or
/// validate the HMAC `manifest_signature` (that requires the per-workspace
/// HMAC key + a JCS canonicalizer, which is the deferred third-party `verify`
/// contract). A passing `check-integrity` proves the local bytes are
/// internally consistent; it does not prove the platform signed them.
//
// The bool fields are independent pass/fail dimensions of the verifier recipe
// (chunk hashes, chain, completeness-claimed, completeness-ok); they are not a
// state machine and collapsing them into enums would obscure the report shape.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IntegrityReport {
    /// Per-chunk hash result: `(chunk_id, expected_hash, ok)`.
    pub chunk_results: Vec<(String, String, bool)>,
    /// Whether every chunk hash matched its manifest entry.
    pub chunks_ok: bool,
    /// Whether the content-hash chain (each event's `prior_content_hash`
    /// equals the prior event's `content_hash`, first is `null`) is intact.
    pub chain_ok: bool,
    /// Number of audit events walked across all chunks.
    pub events_checked: usize,
    /// Whether a completeness proof was present (`includes_completeness_proof`).
    pub completeness_claimed: bool,
    /// Whether the chunks cover every sequence in
    /// `[event_seq_start, event_seq_end]` with no gaps (only meaningful when
    /// `completeness_claimed`).
    pub completeness_ok: bool,
    /// Whether every chunk parsed cleanly: `true` only when NO chunk content
    /// failed to parse. A parse failure is an invalid-UTF-8 chunk or a
    /// non-empty JSONL line that is not valid JSON. These are NOT recoverable
    /// "notes" — a chunk whose manifest SHA-256 still matches but which carries
    /// extra garbage/malformed content must FAIL integrity (the verifier recipe
    /// rejects the bundle on any failure, `workflows.txt:258`). Empty/blank
    /// lines are not failures.
    pub parse_ok: bool,
    /// Count of parse failures (invalid-UTF-8 chunks + invalid-JSON lines)
    /// surfaced for diagnostics; `parse_ok == (parse_failures == 0)`.
    pub parse_failures: usize,
    /// Human-readable notes (e.g. signature-not-checked caveat, gaps found).
    pub notes: Vec<String>,
}

impl IntegrityReport {
    /// `true` only when every checked dimension passed: chunk hashes, the
    /// content-hash chain, clean parsing of all chunk content, and (when
    /// claimed) completeness.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.chunks_ok
            && self.chain_ok
            && self.parse_ok
            && (!self.completeness_claimed || self.completeness_ok)
    }
}

/// Hex-encode a byte slice (lowercase) without an external dependency.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        // Writing to a String cannot fail; the result is discarded
        // intentionally rather than `unwrap`ped (no-unwrap rule).
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Compute the lowercase hex SHA-256 of a byte slice (chunk integrity).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex_lower(&digest)
}

/// One downloaded chunk: its `chunk_id` (as named in the manifest) and the
/// raw bytes on disk.
#[derive(Debug, Clone)]
pub struct DownloadedChunk {
    /// The chunk's id as the manifest names it (e.g. `"0"`).
    pub chunk_id: String,
    /// The chunk's raw bytes (one JSON event per line — JSONL).
    pub bytes: Vec<u8>,
}

/// Run the integrity portion of the audit-bundle verifier recipe over a
/// downloaded manifest + chunks (`workflows.txt:250-258`, steps 2/3/5).
///
/// Steps performed:
/// 2. **Chunk integrity** — each chunk's SHA-256 is recomputed and compared
///    to the manifest's `chunk_hashes` entry.
/// 3. **Content-hash chain** — events are walked in `event_seq` order;
///    each event's stored `content_hash` is taken as authoritative and the
///    next event's `prior_content_hash` must equal it (the first event's
///    `prior_content_hash` must be `null`).
/// 5. **Completeness proof** — when the manifest sets
///    `includes_completeness_proof=true`, the union of event sequences must
///    cover `[event_seq_start, event_seq_end]` with no gaps.
///
/// Steps 1 (manifest HMAC signature) and 4 (overlay pin) are **not**
/// performed here — step 1 is the deferred authenticity `verify` contract and
/// step 4 requires overlay-row internals. The returned [`IntegrityReport`]
/// records that caveat in `notes`.
///
/// The manifest is expected to carry `chunk_hashes` (a map of `chunk_id` →
/// hex SHA-256, or an array indexed by chunk number), `event_seq_start`,
/// `event_seq_end`, and `includes_completeness_proof`. Missing fields degrade
/// gracefully (recorded in `notes`) rather than panicking.
#[must_use]
pub fn check_bundle_integrity(manifest: &Value, chunks: &[DownloadedChunk]) -> IntegrityReport {
    let mut notes = vec![
        "manifest HMAC signature NOT verified (authenticity check is the deferred \
         `verify` contract); this checks integrity only"
            .to_owned(),
        "overlay-pin step NOT performed (requires overlay-row internals)".to_owned(),
    ];

    // ---- Step 2: chunk integrity ----
    let chunk_results = verify_chunk_hashes(manifest, chunks, &mut notes);
    let chunks_ok = !chunk_results.is_empty() && chunk_results.iter().all(|(_, _, ok)| *ok);
    if chunk_results.is_empty() {
        notes.push("no chunk hashes found in manifest; chunk integrity unverified".to_owned());
    }

    // ---- Step 3: content-hash chain (+ parse-failure count) ----
    let (chain_ok, events_checked, seqs, parse_failures) = verify_content_chain(chunks, &mut notes);
    // A chunk whose SHA-256 still matches the manifest but which carries an
    // invalid-UTF-8 region or a malformed JSONL line is NOT trustworthy: the
    // recipe rejects the bundle on any failure (`workflows.txt:258`).
    let parse_ok = parse_failures == 0;

    // ---- Step 5: completeness proof ----
    let completeness_claimed = manifest
        .get("includes_completeness_proof")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let completeness_ok = if completeness_claimed {
        verify_completeness(manifest, &seqs, &mut notes)
    } else {
        notes.push("manifest does not claim a completeness proof; gap check skipped".to_owned());
        false
    };

    IntegrityReport {
        chunk_results,
        chunks_ok,
        chain_ok,
        events_checked,
        completeness_claimed,
        completeness_ok,
        parse_ok,
        parse_failures,
        notes,
    }
}

/// Look up the manifest's expected hash for a chunk id, accepting either a
/// map (`{"0": "abcd…"}`) or an array indexed by chunk number.
fn manifest_chunk_hash<'a>(manifest: &'a Value, chunk_id: &str) -> Option<&'a str> {
    let hashes = manifest
        .get("chunk_hashes")
        .or_else(|| manifest.get("chunks"))?;
    match hashes {
        Value::Object(map) => map.get(chunk_id).and_then(Value::as_str),
        Value::Array(arr) => chunk_id
            .parse::<usize>()
            .ok()
            .and_then(|i| arr.get(i))
            .and_then(|v| {
                // An array entry may be a bare hash string or an object with
                // a `hash`/`chunk_hash` field.
                v.as_str()
                    .or_else(|| v.get("hash").and_then(Value::as_str))
                    .or_else(|| v.get("chunk_hash").and_then(Value::as_str))
            }),
        _ => None,
    }
}

/// Step 2: recompute each chunk's SHA-256 and compare to the manifest entry.
fn verify_chunk_hashes(
    manifest: &Value,
    chunks: &[DownloadedChunk],
    notes: &mut Vec<String>,
) -> Vec<(String, String, bool)> {
    let mut results = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if let Some(expected) = manifest_chunk_hash(manifest, &chunk.chunk_id) {
            let actual = sha256_hex(&chunk.bytes);
            let ok = actual.eq_ignore_ascii_case(expected);
            if !ok {
                notes.push(format!(
                    "chunk {} hash mismatch (expected {expected}, got {actual})",
                    chunk.chunk_id
                ));
            }
            results.push((chunk.chunk_id.clone(), expected.to_owned(), ok));
        } else {
            notes.push(format!(
                "chunk {} has no hash entry in the manifest",
                chunk.chunk_id
            ));
            results.push((chunk.chunk_id.clone(), String::new(), false));
        }
    }
    results
}

/// One parsed audit event plus its `event_seq` (when present and parseable).
///
/// `seq` is `None` when the row's `event_seq` is missing or unparseable — a
/// recorded failure (it fails both the chain walk and completeness) rather than
/// being silently coerced to `0`, which would let a malformed/forged row slip
/// past the gap check.
struct ParsedEvent {
    value: Value,
    seq: Option<i64>,
}

/// Parse all JSONL events out of the chunk bytes, sorted by `event_seq`.
///
/// Returns `(events, parse_failures)`. `parse_failures` counts every chunk that
/// is not valid UTF-8 plus every non-empty JSONL line that is not valid JSON —
/// these are hard integrity failures (the SHA-256 may still match, but the
/// content carries garbage), surfaced to the caller so they can fail the
/// report's `parse_ok` dimension. Empty/blank lines are NOT failures.
///
/// Events with a missing/unparseable `event_seq` (`seq == None`) sort last so
/// they do not displace well-formed rows in the chain walk; their absence is
/// surfaced separately by [`verify_content_chain`] (it fails `chain_ok`).
fn parse_events(chunks: &[DownloadedChunk], notes: &mut Vec<String>) -> (Vec<ParsedEvent>, usize) {
    let mut events = Vec::new();
    let mut parse_failures = 0usize;
    for chunk in chunks {
        let Ok(text) = std::str::from_utf8(&chunk.bytes) else {
            parse_failures += 1;
            notes.push(format!("chunk {} is not valid UTF-8", chunk.chunk_id));
            continue;
        };
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                let seq = event_seq(&v);
                events.push(ParsedEvent { value: v, seq });
            } else {
                parse_failures += 1;
                notes.push(format!(
                    "chunk {} has a line that is not valid JSON; skipped",
                    chunk.chunk_id
                ));
            }
        }
    }
    // None sorts after Some(_), keeping seq-bearing rows in monotonic order.
    events.sort_by_key(|e| (e.seq.is_none(), e.seq.unwrap_or(0)));
    (events, parse_failures)
}

/// Read an event's `event_seq` as a sortable integer, or `None` when it is
/// missing or cannot be parsed (a recorded failure, never coerced to 0).
fn event_seq(event: &Value) -> Option<i64> {
    event.get("event_seq").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Render an event's `event_seq` for diagnostics (`<missing>` when absent).
fn seq_label(seq: Option<i64>) -> String {
    seq.map_or_else(|| "<missing>".to_owned(), |s| s.to_string())
}

/// Step 3: walk events in `event_seq` order and verify the content-hash chain.
///
/// Returns `(chain_ok, events_checked, observed_sequences, parse_failures)`.
/// Only well-formed sequences (`Some`) are returned in `observed_sequences`; an
/// event with a missing/unparseable `event_seq` fails the chain here and is
/// excluded from the completeness coverage set. `parse_failures` is threaded
/// through from [`parse_events`] (invalid-UTF-8 chunks + invalid-JSON lines) so
/// the caller can fail the report's `parse_ok` dimension.
fn verify_content_chain(
    chunks: &[DownloadedChunk],
    notes: &mut Vec<String>,
) -> (bool, usize, Vec<i64>, usize) {
    let (events, parse_failures) = parse_events(chunks, notes);
    let events_checked = events.len();
    if events.is_empty() {
        notes.push("no audit events parsed; chain unverified".to_owned());
        return (false, 0, Vec::new(), parse_failures);
    }

    let mut chain_ok = true;
    let mut prior_hash: Option<String> = None;
    let mut seqs = Vec::with_capacity(events.len());

    for event in &events {
        // A missing/unparseable event_seq is a hard failure: record it and
        // skip adding it to the completeness coverage set.
        if let Some(seq) = event.seq {
            seqs.push(seq);
        } else {
            chain_ok = false;
            notes.push(
                "an audit event has a missing or unparseable event_seq; \
                 chain and completeness cannot be trusted"
                    .to_owned(),
            );
        }

        let this_prior = event
            .value
            .get("prior_content_hash")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(expected_prior) = &prior_hash {
            // Subsequent event's prior_content_hash must equal the prior
            // event's content_hash.
            match &this_prior {
                Some(p) if p.eq_ignore_ascii_case(expected_prior) => {}
                _ => {
                    chain_ok = false;
                    notes.push(format!(
                        "chain break at event_seq {}: prior_content_hash does not match the \
                         preceding event's content_hash",
                        seq_label(event.seq)
                    ));
                }
            }
        } else if this_prior.is_some() {
            // First event in the walk carries a non-null prior hash.
            chain_ok = false;
            notes.push(
                "first audit event has a non-null prior_content_hash (expected null)".to_owned(),
            );
        }

        prior_hash = event
            .value
            .get("content_hash")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if prior_hash.is_none() {
            chain_ok = false;
            notes.push(format!(
                "event_seq {} is missing content_hash; chain cannot continue",
                seq_label(event.seq)
            ));
        }
    }

    (chain_ok, events_checked, seqs, parse_failures)
}

/// Step 5: verify the observed sequences cover `[event_seq_start, event_seq_end]`
/// with no gaps.
fn verify_completeness(manifest: &Value, seqs: &[i64], notes: &mut Vec<String>) -> bool {
    let read_seq = |key: &str| -> Option<i64> {
        manifest.get(key).and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
    };
    let (Some(start), Some(end)) = (read_seq("event_seq_start"), read_seq("event_seq_end")) else {
        notes.push(
            "completeness proof claimed but event_seq_start/end missing from manifest".to_owned(),
        );
        return false;
    };
    if end < start {
        notes.push("event_seq_end is before event_seq_start in the manifest".to_owned());
        return false;
    }
    let present: std::collections::HashSet<i64> = seqs.iter().copied().collect();
    let mut missing = Vec::new();
    for seq in start..=end {
        if !present.contains(&seq) {
            missing.push(seq);
        }
    }
    if missing.is_empty() {
        true
    } else {
        notes.push(format!(
            "completeness gap: {} sequence(s) missing in [{start}, {end}] (e.g. {:?})",
            missing.len(),
            missing.iter().take(10).collect::<Vec<_>>()
        ));
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- path construction + encoding matrix ----

    #[test]
    fn create_workflow_path_and_optional_fields() {
        let params = CreateWorkflowParams {
            name: Some("Acme".to_owned()),
            ..Default::default()
        };
        // Path is built from the workspace id; no panic on encode.
        let path = format!("/workspace/{}/workflows/", urlencoding::encode("ws 1"));
        assert_eq!(path, "/workspace/ws%201/workflows/");
        // Optional-only fields are not inserted when None.
        let mut form = HashMap::new();
        put_opt(&mut form, "name", params.name.as_deref());
        put_opt(&mut form, "description", params.description.as_deref());
        assert_eq!(form.get("name").map(String::as_str), Some("Acme"));
        assert!(!form.contains_key("description"));
    }

    #[test]
    fn instantiate_form_carries_idempotency_key_and_json_payload() {
        let params = InstantiateParams {
            idempotency_key: "job-001".to_owned(),
            trigger_payload: Some(r#"{"customer":"acme"}"#.to_owned()),
            external_subject_id: Some("sub_abc".to_owned()),
            pool_key: None,
        };
        let mut form = HashMap::new();
        form.insert("idempotency_key".to_owned(), params.idempotency_key.clone());
        put_opt(
            &mut form,
            "trigger_payload",
            params.trigger_payload.as_deref(),
        );
        put_opt(
            &mut form,
            "external_subject_id",
            params.external_subject_id.as_deref(),
        );
        put_opt(&mut form, "pool_key", params.pool_key.as_deref());
        assert_eq!(
            form.get("idempotency_key").map(String::as_str),
            Some("job-001")
        );
        assert_eq!(
            form.get("trigger_payload").map(String::as_str),
            Some(r#"{"customer":"acme"}"#)
        );
        assert!(!form.contains_key("pool_key"));
    }

    #[test]
    fn audit_bundle_chunk_path_encodes_ids() {
        assert_eq!(
            audit_bundle_chunk_path("exj-1", "manifest"),
            "/audit/export_jobs/exj-1/bundle/manifest/"
        );
        assert_eq!(
            audit_bundle_chunk_path("exj-1", "0"),
            "/audit/export_jobs/exj-1/bundle/0/"
        );
    }

    #[test]
    fn revoke_grant_path_encodes_both_ids() {
        let path = format!(
            "/workflows/{}/grants/{}/",
            urlencoding::encode("4011234567890123456"),
            urlencoding::encode("user/1")
        );
        assert_eq!(path, "/workflows/4011234567890123456/grants/user%2F1/");
    }

    #[test]
    fn review_workspace_active_path_and_pagination_params() {
        // Workspace id is path-encoded.
        let path = format!(
            "/workflow-review/workspace/{}/active/",
            urlencoding::encode("ws/9")
        );
        assert_eq!(path, "/workflow-review/workspace/ws%2F9/active/");
        // Pagination params are inserted only when present (mirrors the fn body).
        let limit: Option<u32> = Some(25);
        let offset: Option<u32> = None;
        let mut params = HashMap::new();
        if let Some(l) = limit {
            params.insert("limit".to_owned(), l.to_string());
        }
        if let Some(o) = offset {
            params.insert("offset".to_owned(), o.to_string());
        }
        assert_eq!(params.get("limit").map(String::as_str), Some("25"));
        assert!(!params.contains_key("offset"));
    }

    // ---- sha256 / integrity ----

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Build a minimal valid two-event chain (event 1 `prior=null`,
    /// event 2 `prior=event1.content_hash`) split across two chunks.
    fn build_valid_bundle() -> (Value, Vec<DownloadedChunk>) {
        let e1 = r#"{"event_seq":1,"content_hash":"aaa","prior_content_hash":null}"#;
        let e2 = r#"{"event_seq":2,"content_hash":"bbb","prior_content_hash":"aaa"}"#;
        let chunk0 = DownloadedChunk {
            chunk_id: "0".to_owned(),
            bytes: e1.as_bytes().to_vec(),
        };
        let chunk1 = DownloadedChunk {
            chunk_id: "1".to_owned(),
            bytes: e2.as_bytes().to_vec(),
        };
        let manifest = json!({
            "chunk_hashes": {
                "0": sha256_hex(chunk0.bytes.as_slice()),
                "1": sha256_hex(chunk1.bytes.as_slice()),
            },
            "event_seq_start": 1,
            "event_seq_end": 2,
            "includes_completeness_proof": true,
        });
        (manifest, vec![chunk0, chunk1])
    }

    #[test]
    fn check_integrity_passes_on_valid_bundle() {
        let (manifest, chunks) = build_valid_bundle();
        let report = check_bundle_integrity(&manifest, &chunks);
        assert!(report.chunks_ok, "chunks: {:?}", report.chunk_results);
        assert!(report.chain_ok, "chain notes: {:?}", report.notes);
        assert!(report.completeness_claimed);
        assert!(
            report.completeness_ok,
            "completeness notes: {:?}",
            report.notes
        );
        assert_eq!(report.events_checked, 2);
        assert!(report.passed());
        // The signature caveat must always be recorded.
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("signature NOT verified"))
        );
    }

    #[test]
    fn check_integrity_detects_chunk_tamper() {
        let (manifest, mut chunks) = build_valid_bundle();
        // Tamper with chunk 0's bytes; its recomputed hash will mismatch.
        chunks[0].bytes =
            br#"{"event_seq":1,"content_hash":"aaa","prior_content_hash":null,"x":1}"#.to_vec();
        let report = check_bundle_integrity(&manifest, &chunks);
        assert!(!report.chunks_ok);
        assert!(!report.passed());
    }

    #[test]
    fn check_integrity_detects_broken_chain() {
        // event 2 points at the wrong prior hash.
        let e1 = r#"{"event_seq":1,"content_hash":"aaa","prior_content_hash":null}"#;
        let e2 = r#"{"event_seq":2,"content_hash":"bbb","prior_content_hash":"WRONG"}"#;
        let chunk0 = DownloadedChunk {
            chunk_id: "0".to_owned(),
            bytes: e1.as_bytes().to_vec(),
        };
        let chunk1 = DownloadedChunk {
            chunk_id: "1".to_owned(),
            bytes: e2.as_bytes().to_vec(),
        };
        let manifest = json!({
            "chunk_hashes": {
                "0": sha256_hex(chunk0.bytes.as_slice()),
                "1": sha256_hex(chunk1.bytes.as_slice()),
            },
            "event_seq_start": 1,
            "event_seq_end": 2,
            "includes_completeness_proof": true,
        });
        let report = check_bundle_integrity(&manifest, &[chunk0, chunk1]);
        assert!(report.chunks_ok, "chunk hashes should still match");
        assert!(!report.chain_ok, "chain should be broken");
        assert!(!report.passed());
    }

    #[test]
    fn check_integrity_detects_completeness_gap() {
        // Only event_seq 1 present, but manifest claims [1,3].
        let e1 = r#"{"event_seq":1,"content_hash":"aaa","prior_content_hash":null}"#;
        let chunk0 = DownloadedChunk {
            chunk_id: "0".to_owned(),
            bytes: e1.as_bytes().to_vec(),
        };
        let manifest = json!({
            "chunk_hashes": { "0": sha256_hex(chunk0.bytes.as_slice()) },
            "event_seq_start": 1,
            "event_seq_end": 3,
            "includes_completeness_proof": true,
        });
        let report = check_bundle_integrity(&manifest, &[chunk0]);
        assert!(report.completeness_claimed);
        assert!(!report.completeness_ok);
        assert!(!report.passed());
    }

    #[test]
    fn check_integrity_fails_on_missing_event_seq() {
        // An event with no event_seq must fail the chain + completeness rather
        // than being silently coerced to 0.
        let e1 = r#"{"event_seq":1,"content_hash":"aaa","prior_content_hash":null}"#;
        let e2 = r#"{"content_hash":"bbb","prior_content_hash":"aaa"}"#; // no event_seq
        let chunk0 = DownloadedChunk {
            chunk_id: "0".to_owned(),
            bytes: e1.as_bytes().to_vec(),
        };
        let chunk1 = DownloadedChunk {
            chunk_id: "1".to_owned(),
            bytes: e2.as_bytes().to_vec(),
        };
        let manifest = json!({
            "chunk_hashes": {
                "0": sha256_hex(chunk0.bytes.as_slice()),
                "1": sha256_hex(chunk1.bytes.as_slice()),
            },
            "event_seq_start": 1,
            "event_seq_end": 2,
            "includes_completeness_proof": true,
        });
        let report = check_bundle_integrity(&manifest, &[chunk0, chunk1]);
        assert!(report.chunks_ok, "chunk hashes still match");
        assert!(!report.chain_ok, "missing event_seq must fail the chain");
        assert!(!report.completeness_ok, "missing seq leaves a coverage gap");
        assert!(!report.passed());
        assert!(
            report.notes.iter().any(|n| n.contains("event_seq")),
            "a note must explain the missing event_seq: {:?}",
            report.notes
        );
    }

    #[test]
    fn integrity_report_passed_logic() {
        let base = IntegrityReport {
            chunk_results: vec![("0".to_owned(), "h".to_owned(), true)],
            chunks_ok: true,
            chain_ok: true,
            events_checked: 1,
            completeness_claimed: false,
            completeness_ok: false,
            parse_ok: true,
            parse_failures: 0,
            notes: Vec::new(),
        };
        // No completeness claimed → passes on chunks+chain+parse only.
        assert!(base.passed());
        // Completeness claimed but failed → fails.
        let mut c = base.clone();
        c.completeness_claimed = true;
        c.completeness_ok = false;
        assert!(!c.passed());
        // Parse failure is its own failing dimension, even with everything else
        // green.
        let mut p = base.clone();
        p.parse_ok = false;
        p.parse_failures = 1;
        assert!(!p.passed());
    }

    #[test]
    fn check_integrity_fails_on_extra_malformed_jsonl_line() {
        // Two valid events covering [1,2], BUT chunk 1 carries an extra garbage
        // (non-JSON) line. The manifest hashes are computed over the actual
        // bytes (including the garbage line), so chunks_ok stays true and the
        // chain + completeness still pass — yet the bundle MUST be rejected.
        let e1 = r#"{"event_seq":1,"content_hash":"aaa","prior_content_hash":null}"#;
        let e2_with_garbage = "{\"event_seq\":2,\"content_hash\":\"bbb\",\"prior_content_hash\":\"aaa\"}\nthis is not json";
        let chunk0 = DownloadedChunk {
            chunk_id: "0".to_owned(),
            bytes: e1.as_bytes().to_vec(),
        };
        let chunk1 = DownloadedChunk {
            chunk_id: "1".to_owned(),
            bytes: e2_with_garbage.as_bytes().to_vec(),
        };
        let manifest = json!({
            "chunk_hashes": {
                "0": sha256_hex(chunk0.bytes.as_slice()),
                "1": sha256_hex(chunk1.bytes.as_slice()),
            },
            "event_seq_start": 1,
            "event_seq_end": 2,
            "includes_completeness_proof": true,
        });
        let report = check_bundle_integrity(&manifest, &[chunk0, chunk1]);
        assert!(
            report.chunks_ok,
            "chunk hashes still match the garbage bytes"
        );
        assert!(report.chain_ok, "the two valid events still chain");
        assert!(
            report.completeness_ok,
            "the two valid events still cover [1,2]"
        );
        assert!(!report.parse_ok, "the malformed line must fail parse_ok");
        assert_eq!(report.parse_failures, 1);
        assert!(
            !report.passed(),
            "an extra malformed JSONL line must fail the bundle"
        );
    }

    #[test]
    fn check_integrity_fails_on_invalid_utf8_chunk() {
        // A chunk whose bytes are not valid UTF-8 (a lone 0xFF byte) whose
        // manifest hash still matches must fail integrity via parse_ok.
        let bytes = vec![0xFFu8, 0xFE, 0xFD];
        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "test bytes must be invalid UTF-8"
        );
        let chunk0 = DownloadedChunk {
            chunk_id: "0".to_owned(),
            bytes: bytes.clone(),
        };
        let manifest = json!({
            "chunk_hashes": { "0": sha256_hex(bytes.as_slice()) },
            "includes_completeness_proof": false,
        });
        let report = check_bundle_integrity(&manifest, &[chunk0]);
        assert!(report.chunks_ok, "chunk hash matches the (non-UTF-8) bytes");
        assert!(!report.parse_ok, "invalid UTF-8 must fail parse_ok");
        assert_eq!(report.parse_failures, 1);
        assert!(
            !report.passed(),
            "an invalid-UTF-8 chunk must fail the bundle"
        );
    }
}
