/// CLI argument parsing for the Fast.io CLI.
///
/// Defines the root `Cli` struct and all subcommands using clap's derive API.
//
// ## Clap rename / alias / deprecation recipe (read before renaming a command)
//
// Three attributes cover the rename/deprecate patterns used throughout the
// retool. Apply them on the `Commands` (or nested) enum variant:
//
//   * `#[command(visible_alias = "<new>")]` — an additional accepted name
//     that ALSO appears in `--help` and completions. Use for surfacing a new
//     short form alongside the canonical one (e.g. `workflow` / `wf`).
//   * `#[command(alias = "<old>")]` — an accepted-but-HIDDEN back-compat
//     name (does not show in `--help`/completions). Use to keep old
//     invocations working after a rename, e.g. `ai` -> `ripley`,
//     `info` -> `details`. Back-compat aliases that change behavior must
//     remap the request body/endpoint, not just the name.
//   * `#[command(hide = true)]` — hide an entire deprecated subcommand from
//     `--help` while keeping it parseable (e.g. removed billing compat shims).
//
// Renames land in their owning phase (e.g. `ai` -> `ripley` is Phase 1); this
// comment is the single documented reference for the recipe.
use clap::{Parser, Subcommand, ValueEnum};
use std::fmt;

/// Fast.io cloud storage CLI.
#[derive(Parser)]
#[command(
    name = "fastio",
    version,
    about = "Command-line interface for the Fast.io cloud storage platform",
    long_about = None,
)]
pub struct Cli {
    /// Output format (json, table, csv, markdown). Auto-detects if omitted.
    #[arg(long, global = true, value_parser = ["json", "table", "csv", "markdown", "md"])]
    pub format: Option<String>,

    /// Comma-separated list of fields to include in output.
    #[arg(long, global = true)]
    pub fields: Option<String>,

    /// Server-side response verbosity (terse, standard, full). Selects how
    /// much data the API returns via `?output=<detail>` on supported
    /// endpoints; orthogonal to `--format` (client rendering). Defaults to
    /// the server's `full` shape when omitted.
    #[arg(long, global = true, value_parser = ["terse", "standard", "full"])]
    pub detail: Option<String>,

    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Suppress all output.
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// Increase verbosity (-v info, -vv debug, -vvv trace API calls).
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Named profile to use.
    #[arg(long, global = true)]
    pub profile: Option<String>,

    /// Bearer token for authentication (overrides stored credentials).
    #[arg(long, global = true, env = "FASTIO_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Override the API base URL.
    #[arg(long, global = true)]
    pub api_base: Option<String>,

    /// The subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level command groups.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum Commands {
    /// Authentication and credential management.
    #[command(subcommand)]
    Auth(AuthCommands),

    /// User profile management.
    #[command(subcommand)]
    User(UserCommands),
    /// Organization management.
    #[command(subcommand)]
    Org(OrgCommands),
    /// Workspace management.
    #[command(subcommand)]
    Workspace(WorkspaceCommands),
    /// Workspace member management.
    #[command(subcommand)]
    Member(MemberCommands),
    /// Invitations.
    #[command(subcommand)]
    Invitation(InvitationCommands),

    /// File and folder operations.
    #[command(subcommand)]
    Files(FilesCommands),
    /// File uploads.
    #[command(subcommand)]
    Upload(UploadCommands),
    /// File downloads.
    #[command(subcommand)]
    Download(DownloadCommands),
    /// Share management (data rooms).
    #[command(subcommand)]
    Share(ShareCommands),
    /// Offload multi-step work to Ripley — Fast.io's AI agent. Ask questions
    /// about your content, generate AI shares, and search semantically. (The
    /// former `ai` group; `ai` still works as a hidden alias.)
    #[command(subcommand, alias = "ai")]
    Ripley(RipleyCommands),
    /// File comments.
    #[command(subcommand)]
    Comment(CommentCommands),
    /// Activity events.
    #[command(subcommand)]
    Event(EventCommands),
    /// File previews.
    #[command(subcommand)]
    Preview(PreviewCommands),
    /// Organization and workspace assets.
    #[command(subcommand)]
    Asset(AssetCommands),
    /// [legacy] Task management. Superseded by `fastio workflow`; remains
    /// functional for now.
    #[command(
        subcommand,
        long_about = "[legacy] Task management.\n\nThis is a legacy primitive, superseded by `fastio workflow`; it remains functional for now. Prefer `fastio workflow` for new work."
    )]
    Task(TaskCommands),
    /// [legacy] Worklog management. Superseded by `fastio workflow`; remains
    /// functional for now.
    #[command(
        subcommand,
        long_about = "[legacy] Worklog management.\n\nThis is a legacy primitive, superseded by `fastio workflow`; it remains functional for now. Prefer `fastio workflow` for new work."
    )]
    Worklog(WorklogCommands),
    /// [legacy] Approval workflows. Superseded by `fastio workflow`; remains
    /// functional for now.
    #[command(
        subcommand,
        long_about = "[legacy] Approval workflows.\n\nThis is a legacy primitive, superseded by `fastio workflow`; it remains functional for now. Prefer `fastio workflow` for new work."
    )]
    Approval(ApprovalCommands),
    /// [legacy] Todo items. Superseded by `fastio workflow`; remains functional
    /// for now.
    #[command(
        subcommand,
        long_about = "[legacy] Todo items.\n\nThis is a legacy primitive, superseded by `fastio workflow`; it remains functional for now. Prefer `fastio workflow` for new work."
    )]
    Todo(TodoCommands),

    /// Connected apps and integrations.
    #[command(subcommand)]
    Apps(AppsCommands),
    /// Cloud import and sync.
    #[command(subcommand)]
    Import(ImportCommands),
    /// File locking.
    #[command(subcommand)]
    Lock(LockCommands),

    /// Unified search across a workspace or share (grouped result buckets).
    #[command(subcommand)]
    Search(SearchCommands),

    /// Render a markdown note or `.md` file in the terminal.
    ///
    /// This is a dedicated markdown viewer: it always emits rendered (or, with
    /// `--raw`/when piped, verbatim) markdown and ignores the global `--format`
    /// and `--fields` flags. Only note nodes and markdown files are supported;
    /// other file types are rejected rather than dumped as raw bytes.
    View {
        /// Workspace ID.
        workspace_id: String,
        /// Node ID of the note or `.md` file to view.
        node_id: String,
        /// Print the raw markdown without terminal rendering.
        #[arg(long)]
        raw: bool,
        /// Read a specific version (note version `OpaqueId`).
        #[arg(long)]
        version: Option<String>,
        /// Reserved: disable paging. (No pager is ever launched; accepted for
        /// forward-compatibility and to make non-interactive intent explicit.)
        #[arg(long)]
        no_pager: bool,
    },

    /// Metadata extraction and template management.
    #[command(subcommand)]
    Metadata(MetadataCommands),

    /// Workflow Orchestration (v3.2): durable multi-step runtime, templates,
    /// triggers, human obligations, signed audit, webhooks, and pools. This is
    /// the new orchestration surface — distinct from the `[legacy]` task /
    /// approval / todo primitives.
    #[command(subcommand, visible_alias = "wf")]
    Workflow(WorkflowCommands),

    /// E-signature: draft, send, void, and download `SignEnvelopes` (PDFs sent
    /// to recipients for electronic signature). Parented to a workspace OR an
    /// org. Signing is a paid-plan feature.
    #[command(subcommand)]
    Sign(SignCommands),

    /// AI instructions for user / org / workspace / share profiles.
    #[command(subcommand)]
    Instructions(InstructionsCommands),

    /// System health and status checks (no auth required).
    #[command(subcommand)]
    System(SystemCommands),

    /// Start the MCP (Model Context Protocol) server over stdio.
    Mcp {
        /// Optional comma-separated list of tools to enable (default: all).
        #[arg(long)]
        tools: Option<String>,
    },

    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: ShellType,
    },

    /// Manage CLI configuration and profiles.
    #[command(subcommand)]
    Configure(ConfigureCommands),

    /// Print the agent skill guide (usage patterns for AI agents and automation).
    Skill,
}

// ─── Search ──────────────────────────────────────────────────────────────────

/// Unified-search subcommands.
///
/// One query, results **grouped by type** into buckets (files, metadata,
/// comments, workflows for a workspace; files + comments for a share). Each
/// bucket paginates independently via its own `--<bucket>-limit/offset`.
/// `--only` filters which buckets are *displayed* client-side — the server
/// always searches every applicable bucket (there is no server `only`
/// parameter), so it does not reduce server work.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SearchCommands {
    /// Search everything in a workspace (files + metadata + comments + workflows).
    Workspace {
        /// Workspace ID.
        workspace_id: String,
        /// Search query (max 1024 characters; must not be blank).
        query: String,
        /// Page size for the files bucket.
        #[arg(long)]
        files_limit: Option<u32>,
        /// Offset for the files bucket.
        #[arg(long)]
        files_offset: Option<u32>,
        /// Page size for the metadata bucket.
        #[arg(long)]
        metadata_limit: Option<u32>,
        /// Offset for the metadata bucket.
        #[arg(long)]
        metadata_offset: Option<u32>,
        /// Page size for the comments bucket.
        #[arg(long)]
        comments_limit: Option<u32>,
        /// Offset for the comments bucket.
        #[arg(long)]
        comments_offset: Option<u32>,
        /// Page size for the workflows bucket.
        #[arg(long)]
        workflows_limit: Option<u32>,
        /// Offset for the workflows bucket.
        #[arg(long)]
        workflows_offset: Option<u32>,
        /// Comma-separated buckets to DISPLAY (e.g. `files,comments`).
        /// Client-side filter only; the server still searches every bucket.
        #[arg(long)]
        only: Option<String>,
    },
    /// Search everything in a share (files + comments; metadata is workspace-only).
    Share {
        /// Share ID.
        share_id: String,
        /// Search query (max 1024 characters; must not be blank).
        query: String,
        /// Page size for the files bucket.
        #[arg(long)]
        files_limit: Option<u32>,
        /// Offset for the files bucket.
        #[arg(long)]
        files_offset: Option<u32>,
        /// Page size for the comments bucket.
        #[arg(long)]
        comments_limit: Option<u32>,
        /// Offset for the comments bucket.
        #[arg(long)]
        comments_offset: Option<u32>,
        /// Comma-separated buckets to DISPLAY (e.g. `files`).
        /// Client-side filter only; the server still searches every bucket.
        #[arg(long)]
        only: Option<String>,
    },
}

// ─── Workflow Orchestration ────────────────────────────────────────────────────

/// Workflow Orchestration subcommands (`fastio workflow`, alias `wf`).
///
/// Owner-side REST surface for the durable v3.2 runtime. All identifiers are
/// opaque strings: the workflow id is a 19-digit profile id, the obligation id
/// a plain numeric sequence, and the rest hyphenated `OpaqueId`s.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowCommands {
    /// Create a workflow profile in a workspace.
    Create {
        /// Workspace ID.
        workspace_id: String,
        /// Display name.
        #[arg(long)]
        name: Option<String>,
        /// Human-readable description.
        #[arg(long)]
        description: Option<String>,
        /// Bind a published template revision at create time.
        #[arg(long)]
        template_id: Option<String>,
        /// Credit budget cap for the runtime.
        #[arg(long)]
        agent_credit_cap: Option<u64>,
        /// Visibility: workspace, private, or `participants_only`.
        #[arg(long)]
        visibility: Option<String>,
    },
    /// List workflow profiles in a workspace (offset-paginated).
    List {
        /// Workspace ID.
        workspace_id: String,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get a single workflow profile.
    Get {
        /// Workflow ID (19-digit profile id).
        workflow_id: String,
    },
    /// Update mutable fields / transition lifecycle (form-encoded PATCH).
    Update {
        /// Workflow ID.
        workflow_id: String,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// Lifecycle state transition (e.g. active, paused, completed,
        /// cancelled, archived) — must follow the documented DAG.
        #[arg(long)]
        state: Option<String>,
        /// New credit budget cap.
        #[arg(long)]
        agent_credit_cap: Option<u64>,
    },
    /// Soft-archive a workflow (use `purge` for an owner-only hard delete).
    Delete {
        /// Workflow ID.
        workflow_id: String,
    },
    /// Permanently hard-delete a workflow (owner-only, irreversible).
    Purge {
        /// Workflow ID.
        workflow_id: String,
        /// Re-state the workflow ID to confirm the destructive purge.
        #[arg(long)]
        confirm: String,
    },
    /// Transfer a workflow to another workspace in the same organization.
    Transfer {
        /// Workflow ID.
        workflow_id: String,
        /// Destination workspace ID (must be in the same org).
        #[arg(long)]
        to_workspace: String,
    },
    /// Instantiate the workflow runtime (idempotent on the idempotency key).
    Instantiate {
        /// Workflow ID.
        workflow_id: String,
        /// REQUIRED replay-safe idempotency key. Omit only with
        /// `--generate-idempotency-key`.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Generate a random idempotency key (PRINTS A LOUD WARNING — this
        /// breaks replay safety; prefer an explicit, caller-stable key).
        #[arg(long)]
        generate_idempotency_key: bool,
        /// Resolved input bindings as a JSON string (or `@file.json`).
        #[arg(long)]
        trigger_payload: Option<String>,
        /// Integrator correlation handle (1..255 chars).
        #[arg(long)]
        external_subject_id: Option<String>,
        /// Concurrency pool key to admit this run under.
        #[arg(long)]
        pool_key: Option<String>,
    },
    /// Get the runtime state snapshot.
    State {
        /// Workflow ID.
        workflow_id: String,
    },
    /// Poll runtime state until terminal (bounded backoff + jitter).
    Wait {
        /// Workflow ID.
        workflow_id: String,
        /// Base seconds between polls (clamped 1..60; jitter is added).
        #[arg(long)]
        poll_interval: Option<u64>,
    },
    /// Pause an active workflow.
    Pause {
        /// Workflow ID.
        workflow_id: String,
    },
    /// Resume a paused workflow.
    Resume {
        /// Workflow ID.
        workflow_id: String,
    },
    /// Cancel a workflow (cascades to synchronous sub-children).
    Cancel {
        /// Workflow ID.
        workflow_id: String,
        /// Optional cancellation reason.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Rotate the per-workflow inbound HMAC key (returns the version int only).
    RotateInboundKey {
        /// Workflow ID.
        workflow_id: String,
    },

    /// Workflow grants (workflow-scoped roles).
    #[command(subcommand)]
    Grant(WorkflowGrantCommands),
    /// Step occurrences.
    #[command(subcommand)]
    Step(WorkflowStepCommands),
    /// Template revisions (immutable).
    #[command(subcommand)]
    Template(WorkflowTemplateCommands),
    /// Event-driven triggers.
    #[command(subcommand)]
    Trigger(WorkflowTriggerCommands),
    /// Workspace verb→template alias map.
    #[command(subcommand)]
    TriggerAlias(WorkflowTriggerAliasCommands),
    /// Human obligations ("waiting on me").
    #[command(subcommand)]
    Obligation(WorkflowObligationCommands),
    /// Cross-workspace / workspace / pool inbox.
    #[command(subcommand)]
    Inbox(WorkflowInboxCommands),
    /// Per-workflow extraction schema.
    #[command(subcommand)]
    Schema(WorkflowSchemaCommands),
    /// Audit log, signed export, integrity check, and dual-control redaction.
    #[command(subcommand)]
    Audit(WorkflowAuditCommands),
    /// Outbound webhook subscriptions.
    #[command(subcommand)]
    Outbound(WorkflowOutboundCommands),
    /// Concurrency pools.
    #[command(subcommand)]
    Pool(WorkflowPoolCommands),
    /// External-subject correlation.
    #[command(subcommand)]
    Subject(WorkflowSubjectCommands),
    /// Realtime channel token (mint only).
    #[command(subcommand)]
    Realtime(WorkflowRealtimeCommands),
    /// Workflow Review surface (v3.5b; 404 when the workspace flag is off).
    #[command(subcommand)]
    Review(WorkflowReviewCommands),
}

/// Workflow grant subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowGrantCommands {
    /// Grant a user a workflow-scoped role.
    Add {
        /// Workflow ID.
        workflow_id: String,
        /// Grantee user ID.
        user_id: String,
        /// Role: viewer, participant, or admin.
        role: String,
        /// Optional expiry timestamp.
        #[arg(long)]
        expires_at: Option<String>,
    },
    /// List a workflow's grants (cursor-paginated).
    List {
        /// Workflow ID.
        workflow_id: String,
        /// Page size (default 100, max 500).
        #[arg(long)]
        limit: Option<u32>,
        /// Cursor from a prior response's `pagination.next_cursor`.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Revoke a user's grant (soft revoke).
    Revoke {
        /// Workflow ID.
        workflow_id: String,
        /// Grantee user ID.
        user_id: String,
    },
}

/// Workflow step subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowStepCommands {
    /// Get a single step occurrence.
    Get {
        /// Workflow ID.
        workflow_id: String,
        /// Step occurrence ID.
        step_occurrence_id: String,
    },
    /// Dispatch a step occurrence's handler.
    Advance {
        /// Workflow ID.
        workflow_id: String,
        /// Step occurrence ID.
        step_occurrence_id: String,
        /// Optional output envelope as a JSON string (or `@file.json`).
        #[arg(long)]
        output: Option<String>,
        /// Re-read the occurrence and retry once on a CAS 409 conflict
        /// (default: surface the conflict).
        #[arg(long)]
        retry_on_conflict: bool,
    },
    /// Cancel a single step occurrence (CAS-guarded).
    Cancel {
        /// Workflow ID.
        workflow_id: String,
        /// Step occurrence ID.
        step_occurrence_id: String,
    },
    /// Submit a step's output envelope (CAS-guarded).
    Output {
        /// Workflow ID.
        workflow_id: String,
        /// Step occurrence ID.
        step_occurrence_id: String,
        /// Output envelope as a JSON string (or `@file.json`).
        output: String,
        /// Re-read the occurrence and retry once on a CAS 409 conflict
        /// (default: surface the conflict).
        #[arg(long)]
        retry_on_conflict: bool,
    },
    /// List occurrences for a step definition.
    Occurrences {
        /// Workflow ID.
        workflow_id: String,
        /// Step definition ID.
        step_id: String,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
}

/// Workflow template subcommands (immutable revisions — no `update`).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowTemplateCommands {
    /// Create a template revision (validated; 422 carries `validation_report`).
    Create {
        /// Workspace ID.
        workspace_id: String,
        /// Template body as a JSON string (or `@file.json`).
        template_body: String,
        /// Optional display name.
        #[arg(long)]
        name: Option<String>,
    },
    /// List template revisions for a workspace.
    List {
        /// Workspace ID.
        workspace_id: String,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get a single revision (`--include-body` inlines the body).
    Get {
        /// Template ID.
        template_id: String,
        /// Inline the full `template_body`.
        #[arg(long)]
        include_body: bool,
    },
    /// Publish a revision (legal only from `validated`).
    Publish {
        /// Template ID.
        template_id: String,
    },
    /// Withdraw a revision (legal only from `published`).
    Withdraw {
        /// Template ID.
        template_id: String,
    },
    /// Deprecate a revision (legal only from `published`).
    Deprecate {
        /// Template ID.
        template_id: String,
    },
}

/// Workflow trigger subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowTriggerCommands {
    /// Create a trigger.
    Create {
        /// Workspace ID.
        workspace_id: String,
        /// Kind: manual, scheduled, `event_match`, `inbound_webhook`, `ai_driven`.
        #[arg(long)]
        kind: Option<String>,
        /// Target template id (optionally `:vN`-versioned).
        #[arg(long)]
        target_template_id: Option<String>,
        /// Event-match expression as a JSON string (or `@file.json`).
        #[arg(long)]
        event_match: Option<String>,
        /// Parameter mapping as a JSON string (or `@file.json`).
        #[arg(long)]
        param_mapping: Option<String>,
        /// Per-hour rate limit.
        #[arg(long)]
        rate_limit_per_hour: Option<u64>,
        /// Concurrency cap.
        #[arg(long)]
        concurrency_cap: Option<u64>,
        /// Dedup scope.
        #[arg(long)]
        dedup_scope: Option<String>,
        /// Idempotency-key template.
        #[arg(long)]
        idempotency_key_template: Option<String>,
    },
    /// List triggers (`--enabled-filter true|false|all`).
    List {
        /// Workspace ID.
        workspace_id: String,
        /// Filter by enabled state: true, false, or all.
        #[arg(long)]
        enabled_filter: Option<String>,
    },
    /// Get a single trigger.
    Get {
        /// Trigger ID.
        trigger_id: String,
    },
    /// Update a trigger's mutable fields (form-encoded PATCH).
    Update {
        /// Trigger ID.
        trigger_id: String,
        /// Toggle enabled state.
        #[arg(long)]
        enabled: Option<bool>,
        /// New per-hour rate limit.
        #[arg(long)]
        rate_limit_per_hour: Option<u64>,
        /// New concurrency cap.
        #[arg(long)]
        concurrency_cap: Option<u64>,
    },
    /// Manually fire a trigger (integration testing / replay).
    Fire {
        /// Trigger ID.
        trigger_id: String,
        /// REQUIRED idempotency key (omit only with
        /// `--generate-idempotency-key`).
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Generate a random idempotency key (LOUD WARNING — breaks replay
        /// safety).
        #[arg(long)]
        generate_idempotency_key: bool,
        /// Trigger payload as a JSON string (or `@file.json`).
        #[arg(long)]
        trigger_payload: Option<String>,
    },
    /// Dry-run (backtest) a saved trigger over a historical window.
    DryRun {
        /// Trigger ID.
        trigger_id: String,
        /// Backtest window in days (≤ 90).
        #[arg(long)]
        window_days: Option<u64>,
        /// Sample-match cap.
        #[arg(long)]
        sample_limit: Option<u64>,
        /// Apply guard checks during the backtest.
        #[arg(long)]
        apply_guards: Option<bool>,
    },
    /// Dry-run an unsaved trigger draft (nothing saved or fired).
    DryRunDraft {
        /// Workspace ID.
        workspace_id: String,
        /// Inline event-match expression as a JSON string (or `@file.json`).
        #[arg(long)]
        event_match: Option<String>,
        /// Inline parameter mapping as a JSON string (or `@file.json`).
        #[arg(long)]
        param_mapping: Option<String>,
        /// Target template id.
        #[arg(long)]
        target_template_id: Option<String>,
        /// Backtest window in days (≤ 90).
        #[arg(long)]
        window_days: Option<u64>,
        /// Sample-match cap.
        #[arg(long)]
        sample_limit: Option<u64>,
    },
    /// Soft-delete a trigger (sets enabled=false).
    Delete {
        /// Trigger ID.
        trigger_id: String,
        /// Permanently hard-delete (requires prior soft-delete + zero
        /// in-flight fires).
        #[arg(long)]
        hard: bool,
    },
    /// Permanently hard-delete a trigger (soft-delete-first required).
    Purge {
        /// Trigger ID.
        trigger_id: String,
        /// Re-state the trigger ID to confirm the destructive purge.
        #[arg(long)]
        confirm: String,
    },
    /// Rotate the workspace inbound trigger key (returns the version int only).
    RotateInboundKey {
        /// Trigger ID.
        trigger_id: String,
    },
}

/// Workspace verb→template alias map subcommands (read-modify-write PATCH).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowTriggerAliasCommands {
    /// Show the workspace's verb→template alias map.
    Get {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Set (add or overwrite) a verb→template alias.
    Set {
        /// Workspace ID.
        workspace_id: String,
        /// Alias verb (e.g. `redact`).
        verb: String,
        /// Template name/id the verb maps to.
        template: String,
    },
    /// Remove a verb from the alias map.
    Remove {
        /// Workspace ID.
        workspace_id: String,
        /// Alias verb to remove.
        verb: String,
    },
    /// Replace the ENTIRE alias map with a supplied JSON object (verb→template).
    ///
    /// Unlike `set`/`remove` (which read-modify-write a single verb), this sets
    /// the whole map in one shot — any verb not present in the JSON is dropped.
    Replace {
        /// Workspace ID.
        workspace_id: String,
        /// The full verb→template map as a JSON object string (e.g.
        /// `'{"redact":"redact-tpl","summarize":"sum-tpl"}'`). `@file.json` is
        /// NOT expanded — pass the literal JSON.
        #[arg(long)]
        aliases_json: String,
    },
}

/// Workflow obligation subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowObligationCommands {
    /// List obligations for a workflow (workflow id is the required anchor).
    List {
        /// Workflow ID (required authz anchor).
        workflow_id: String,
        /// Filter by status.
        #[arg(long)]
        status: Option<String>,
        /// Filter by assigned user id.
        #[arg(long)]
        assigned_user_id: Option<String>,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get a single obligation.
    Get {
        /// Obligation ID (plain numeric sequence string).
        obligation_id: String,
    },
    /// Atomically claim a role-addressed obligation.
    Claim {
        /// Obligation ID.
        obligation_id: String,
    },
    /// Release a claimed obligation back into the pool (claimer-only).
    Release {
        /// Obligation ID.
        obligation_id: String,
    },
    /// Resolve an obligation (optional resolution payload).
    Resolve {
        /// Obligation ID.
        obligation_id: String,
        /// Resolution payload as a JSON string (or `@file.json`).
        #[arg(long)]
        resolution_payload: Option<String>,
    },
}

/// Workflow inbox subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowInboxCommands {
    /// Cross-workspace top-K inbox.
    Me,
    /// Workspace-scoped inbox.
    Workspace {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Pool-scoped inbox.
    Pool {
        /// Workspace ID.
        workspace_id: String,
        /// Pool key.
        pool_key: String,
    },
}

/// Workflow extraction-schema subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowSchemaCommands {
    /// Get the workflow's current extraction schema.
    Get {
        /// Workflow ID.
        workflow_id: String,
    },
    /// Replace the extraction schema (append-only; form-encoded PUT).
    Set {
        /// Workflow ID.
        workflow_id: String,
        /// Extraction schema as a JSON string (or `@file.json`).
        extraction_schema: String,
    },
    /// Auto-derive a proposed schema from sample files (SPENDS AI CREDITS).
    Derive {
        /// Workflow ID.
        workflow_id: String,
        /// Sample node ids as a JSON-string array (or `@file.json`).
        #[arg(long)]
        node_ids: Option<String>,
        /// Acknowledge the AI-credit spend non-interactively.
        #[arg(long)]
        confirm_ai_spend: bool,
    },
}

/// Workflow audit subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowAuditCommands {
    /// Paginated audit event log.
    Events {
        /// Workflow ID.
        workflow_id: String,
        /// Inline the event payload.
        #[arg(long)]
        include_payload: bool,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Signed-export job management.
    #[command(subcommand)]
    Export(WorkflowAuditExportCommands),
    /// Check the INTEGRITY (not authenticity) of a downloaded bundle.
    ///
    /// Verifies chunk SHA-256 hashes, the content-hash chain, and the
    /// completeness proof. Does NOT verify the HMAC signature (that is the
    /// deferred `verify` contract).
    CheckIntegrity {
        /// Path to the downloaded `manifest.json`.
        #[arg(long)]
        manifest: std::path::PathBuf,
        /// Paths to the downloaded chunk files, in chunk order (`0..N-1`).
        #[arg(long = "chunk", num_args = 1.., required = true)]
        chunks: Vec<std::path::PathBuf>,
    },
    /// Dual-control redaction.
    #[command(subcommand)]
    Redaction(WorkflowRedactionCommands),
}

/// Workflow audit-export subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowAuditExportCommands {
    /// Start an asynchronous signed export job.
    Start {
        /// Workflow ID.
        workflow_id: String,
        /// Export scope (e.g. `full`).
        #[arg(long)]
        scope: Option<String>,
        /// Include redaction overlay rows.
        #[arg(long)]
        include_overlays: Option<bool>,
        /// Redaction pin strategy (e.g. `job_start`).
        #[arg(long)]
        redaction_pin_strategy: Option<String>,
    },
    /// List export jobs for a workspace.
    List {
        /// Workspace ID.
        workspace_id: String,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get an export job's status.
    Get {
        /// Export job ID.
        job_id: String,
    },
    /// Stream the manifest or a chunk to disk.
    Download {
        /// Export job ID.
        job_id: String,
        /// Chunk id: `manifest` or an integer in `[0, total_chunks)`.
        #[arg(long, default_value = "manifest")]
        chunk: String,
        /// Output file path.
        #[arg(long, short)]
        output: std::path::PathBuf,
    },
}

/// Workflow dual-control redaction subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowRedactionCommands {
    /// Initiate a redaction (first admin).
    Request {
        /// Workspace ID.
        workspace_id: String,
        /// Target audit event id.
        #[arg(long)]
        target_event_id: String,
        /// Target workflow id.
        #[arg(long)]
        target_workflow_id: String,
        /// Redaction paths as a JSON-string array (or `@file.json`).
        #[arg(long)]
        redaction_paths: String,
        /// Reason for the redaction.
        #[arg(long)]
        reason: String,
    },
    /// Confirm a pending redaction (a DIFFERENT admin).
    Confirm {
        /// Workspace ID.
        workspace_id: String,
        /// Action id from the request phase.
        #[arg(long)]
        action_id: String,
        /// Confirmer user id (must match the authenticated caller).
        #[arg(long)]
        confirmer_user_id: String,
    },
    /// Get a committed redaction batch summary.
    Get {
        /// Workspace ID.
        workspace_id: String,
        /// Redaction batch id.
        redaction_id: String,
    },
}

/// Workflow outbound-webhook-subscription subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowOutboundCommands {
    /// Create a subscription (the HMAC secret is returned ONE TIME).
    Create {
        /// Workspace ID.
        workspace_id: String,
        /// HTTPS delivery target.
        #[arg(long)]
        target_url: String,
        /// Event types as a JSON-string array (or `@file.json`).
        #[arg(long)]
        event_type_subscriptions: String,
        /// Human-readable label.
        #[arg(long)]
        description: Option<String>,
        /// Per-hour delivery cap (0 = no cap).
        #[arg(long)]
        rate_limit_per_hour: Option<u64>,
        /// CDN-family allowlist as a JSON-string array (or `@file.json`).
        #[arg(long)]
        family_allowlist: Option<String>,
        /// Write the one-time secret to this file (0600); never echoed to
        /// stdout.
        #[arg(long)]
        secret_file: Option<std::path::PathBuf>,
    },
    /// List subscriptions for a workspace.
    List {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Get a single subscription (secret not returned).
    Get {
        /// Subscription ID.
        subscription_id: String,
    },
    /// Update a subscription (form-encoded PATCH).
    Update {
        /// Subscription ID.
        subscription_id: String,
        /// Toggle enabled state.
        #[arg(long)]
        enabled: Option<bool>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New per-hour delivery cap.
        #[arg(long)]
        rate_limit_per_hour: Option<u64>,
        /// New CDN-family allowlist as a JSON-string array (or `@file.json`).
        #[arg(long)]
        family_allowlist: Option<String>,
    },
    /// Hard-delete a subscription.
    Delete {
        /// Subscription ID.
        subscription_id: String,
    },
    /// Rotate the HMAC secret (new secret returned ONE TIME).
    RotateSecret {
        /// Subscription ID.
        subscription_id: String,
        /// Write the one-time secret to this file (0600); never echoed to
        /// stdout.
        #[arg(long)]
        secret_file: Option<std::path::PathBuf>,
    },
}

/// Workflow pool subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowPoolCommands {
    /// Create a concurrency pool.
    Create {
        /// Workspace ID.
        workspace_id: String,
        /// Pool key (unique within the workspace).
        pool_key: String,
        /// Maximum concurrent in-flight workflows.
        #[arg(long)]
        max_concurrent: Option<u64>,
        /// Pool source: tag, `template_id`, or freeform.
        #[arg(long)]
        pool_source: Option<String>,
        /// Admission policy at the cap: reject or queue.
        #[arg(long)]
        pool_admission_policy: Option<String>,
    },
    /// List pools in a workspace.
    List {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Get a single pool.
    Get {
        /// Workspace ID.
        workspace_id: String,
        /// Pool key.
        pool_key: String,
    },
    /// Delete a pool (requires zero running and zero queued workflows).
    Delete {
        /// Workspace ID.
        workspace_id: String,
        /// Pool key.
        pool_key: String,
    },
}

/// Workflow external-subject subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowSubjectCommands {
    /// List workflows indexed by an integrator correlation handle.
    Workflows {
        /// Workspace ID.
        workspace_id: String,
        /// External subject id (correlation handle).
        subject_id: String,
    },
}

/// Workflow realtime-channel subcommands (token mint only).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowRealtimeCommands {
    /// Mint a short-lived realtime-channel WebSocket token (no in-CLI client).
    Token {
        /// Workflow ID.
        workflow_id: String,
        /// Write the minted token to this file (0600) instead of stdout.
        #[arg(long)]
        token_file: Option<std::path::PathBuf>,
    },
}

/// Workflow Review (v3.5b) subcommands — flag-gated; 404 when off.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkflowReviewCommands {
    /// Get-or-create a review surface for a step occurrence (idempotent).
    Create {
        /// Step occurrence ID.
        step_occurrence_id: String,
    },
    /// Fetch a review surface (assets + reviewers + decision matrix).
    Get {
        /// Surface ID.
        surface_id: String,
    },
    /// Fetch a single review asset and its current round.
    Asset {
        /// Surface ID.
        surface_id: String,
        /// Asset ID.
        asset_id: String,
    },
    /// Record a review decision (approve / reject / `request_changes`).
    Decision {
        /// Surface ID.
        surface_id: String,
        /// Asset ID.
        asset_id: String,
        /// Decision: approve, reject, or `request_changes`.
        decision: String,
        /// Pinned current version id (CAS guard).
        #[arg(long)]
        version_id_pinned: String,
        /// Optional reviewer comment.
        #[arg(long)]
        comment: Option<String>,
    },
    /// Workspace admin force-resolves a stuck surface.
    AdminResolve {
        /// Surface ID.
        surface_id: String,
        /// Resolution: approved, rejected, or cancelled.
        resolution: String,
    },
}

// ─── Sign (E-Signature) ────────────────────────────────────────────────────────

/// E-signature subcommands (`fastio sign`).
///
/// `SignEnvelopes` are parented to a Workspace OR an Organization; the
/// `--parent-type` / `--parent-id` pair selects the owner. Drafts are created
/// and edited via these commands, then `send` emails real recipients. Signing
/// is a paid-plan feature (a non-entitled org returns `1670`).
// Justification: the envelope-lifecycle variant carries the create/update
// flag set and is larger than the download variants. This is a clap subcommand
// enum constructed once at parse time and immediately dispatched (never stored
// in bulk or passed by value in a hot path), so the size difference is
// immaterial; boxing a clap subcommand payload is non-idiomatic here.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SignCommands {
    /// Envelope lifecycle (create / list / get / update / delete / send / void).
    #[command(subcommand)]
    Envelope(SignEnvelopeCommands),
    /// Document byte downloads (source PDF, signed PDF).
    #[command(subcommand)]
    Document(SignDocumentCommands),
    /// Audit certificate download.
    #[command(subcommand)]
    Audit(SignAuditCommands),
}

/// `SignEnvelope` lifecycle subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SignEnvelopeCommands {
    /// Create a draft envelope.
    ///
    /// Use the ergonomic `--documents-json` / `--recipients-json` /
    /// `--fields-json` (or one `--body-json` for the whole request; each
    /// accepts `@file.json`) for non-trivial envelopes. For a trivial
    /// single-signer single-document draft, the simple flags
    /// `--source-node-id` + `--recipient-email` suffice.
    Create {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID (workspace or org, 19-digit).
        #[arg(long)]
        parent_id: String,
        /// Display name.
        #[arg(long)]
        name: Option<String>,
        /// UTC auto-expiry timestamp (e.g. "2026-06-15 14:30:00 UTC").
        #[arg(long)]
        expires_at: Option<String>,
        /// Whole request body as a JSON object (or `@file.json`). When set, the
        /// other create flags are ignored.
        #[arg(long)]
        body_json: Option<String>,
        /// Policy bag as a JSON object (or `@file.json`).
        #[arg(long)]
        policy_json: Option<String>,
        /// Documents as a JSON array (or `@file.json`).
        #[arg(long)]
        documents_json: Option<String>,
        /// Recipients as a JSON array (or `@file.json`).
        #[arg(long)]
        recipients_json: Option<String>,
        /// Field placements as a JSON array (or `@file.json`).
        #[arg(long)]
        fields_json: Option<String>,
        /// Simple path: a single source document storage node id.
        #[arg(long)]
        source_node_id: Option<String>,
        /// Simple path: pinned source version id for `--source-node-id`.
        #[arg(long)]
        source_version_id: Option<String>,
        /// Simple path: a single signer's email address.
        #[arg(long)]
        recipient_email: Option<String>,
        /// Simple path: the signer's display name.
        #[arg(long)]
        recipient_name: Option<String>,
        /// Simple path: the signer's auth method (`none` / `email_otp` /
        /// `sms_otp`).
        #[arg(long)]
        auth_method: Option<String>,
    },
    /// List envelopes for the parent (offset-paginated).
    List {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get a single envelope (documents/recipients/fields inlined).
    Get {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
    },
    /// Update mutable fields on a DRAFT envelope (a non-draft returns 403).
    ///
    /// `--recipients-json` / `--fields-json` are FULL replacements;
    /// `--documents-json` is a declarative replacement (omit to leave the
    /// document set unchanged). Each accepts `@file.json`.
    Update {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
        /// New display name.
        #[arg(long)]
        name: Option<String>,
        /// New UTC expiry timestamp.
        #[arg(long)]
        expires_at: Option<String>,
        /// New policy bag as a JSON object (or `@file.json`).
        #[arg(long)]
        policy_json: Option<String>,
        /// Declarative document replacement as a JSON array (or `@file.json`).
        #[arg(long)]
        documents_json: Option<String>,
        /// Full recipient replacement as a JSON array (or `@file.json`).
        #[arg(long)]
        recipients_json: Option<String>,
        /// Full field replacement as a JSON array (or `@file.json`).
        #[arg(long)]
        fields_json: Option<String>,
    },
    /// Soft-delete a DRAFT envelope (terminal envelopes refuse). Destructive.
    Delete {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Send a draft envelope (draft → sent). EMAILS REAL RECIPIENTS; idempotent.
    Send {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
        /// Skip the interactive confirmation prompt (send notifies recipients).
        #[arg(long)]
        yes: bool,
    },
    /// Void a non-terminal envelope (cascades to Voided). Credits NOT refunded.
    Void {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
        /// Reason for voiding (REQUIRED, max 1024 bytes).
        #[arg(long)]
        reason: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

/// `SignEnvelope` document-download subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SignDocumentCommands {
    /// Download a document's SOURCE PDF (the file uploaded at create time).
    Download {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
        /// Document ID.
        document_id: String,
        /// Output file path.
        #[arg(long, short)]
        output: String,
    },
    /// Download a document's SIGNED PDF (not ready until the document completes).
    #[command(name = "signed-download")]
    SignedDownload {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
        /// Document ID.
        document_id: String,
        /// Output file path.
        #[arg(long, short)]
        output: String,
    },
}

/// `SignEnvelope` audit-certificate subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SignAuditCommands {
    /// Download the envelope's audit certificate (JSON; not ready until the
    /// envelope reaches a terminal state).
    Download {
        /// Parent type: `workspace` or `org`.
        #[arg(long)]
        parent_type: String,
        /// Parent ID.
        #[arg(long)]
        parent_id: String,
        /// Envelope ID.
        envelope_id: String,
        /// Output file path.
        #[arg(long, short)]
        output: String,
    },
}

// ─── Auth ────────────────────────────────────────────────────────────────────

/// Auth subcommands.
#[derive(Subcommand)]
#[non_exhaustive]
pub enum AuthCommands {
    /// Log in to Fast.io. Uses browser PKCE flow by default.
    /// Provide --email and --password for direct authentication.
    Login {
        /// Email address for basic auth login.
        #[arg(long)]
        email: Option<String>,
        /// Password for basic auth login.
        #[arg(long)]
        password: Option<String>,
    },
    /// Clear stored credentials for the current profile.
    Logout,
    /// Show current authentication status.
    Status,
    /// Create a new Fast.io account.
    Signup {
        /// Email address.
        #[arg(long)]
        email: String,
        /// Password.
        #[arg(long)]
        password: String,
        /// First name.
        #[arg(long)]
        first_name: Option<String>,
        /// Last name.
        #[arg(long)]
        last_name: Option<String>,
    },
    /// Send or confirm email verification.
    Verify {
        /// Email address to verify.
        #[arg(long)]
        email: String,
        /// Verification code (omit to send a new code).
        #[arg(long)]
        code: Option<String>,
    },
    /// Two-factor authentication management.
    #[command(subcommand, name = "2fa")]
    TwoFa(TwoFaCommands),
    /// API key management.
    #[command(subcommand, name = "api-key")]
    ApiKey(ApiKeyCommands),
    /// Verify token validity.
    Check,
    /// Show session info from stored credentials.
    Session,
    /// Check email availability.
    #[command(name = "email-check")]
    EmailCheck {
        /// Email to check.
        email: String,
    },
    /// Request a password reset email.
    #[command(name = "password-reset-request")]
    PasswordResetRequest {
        /// Email address.
        email: String,
    },
    /// Complete a password reset.
    #[command(name = "password-reset")]
    PasswordReset {
        /// Reset code.
        code: String,
        /// New password.
        #[arg(long = "new-password")]
        password1: String,
        /// Confirm new password.
        #[arg(long = "confirm-password")]
        password2: String,
    },
    /// OAuth session management.
    #[command(subcommand)]
    Oauth(OauthCommands),
    /// Check the scopes and capabilities of the current token.
    Scopes,
    /// Check whether a password reset code is valid.
    #[command(name = "password-reset-check")]
    PasswordResetCheck {
        /// The reset code to check.
        code: String,
    },
}

/// 2FA subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TwoFaCommands {
    /// Enable 2FA on a channel (sms, totp, whatsapp).
    Setup {
        /// 2FA channel to enable.
        #[arg(long)]
        channel: String,
    },
    /// Verify a 2FA code after login.
    Verify {
        /// The 2FA verification code.
        #[arg(long)]
        code: String,
    },
    /// Disable 2FA.
    Disable {
        /// 2FA verification token.
        #[arg(long)]
        token: String,
    },
    /// Check 2FA status.
    Status,
    /// Send a 2FA code on a channel.
    Send {
        /// Channel: sms, totp, or whatsapp.
        #[arg(long)]
        channel: String,
    },
    /// Verify TOTP setup.
    #[command(name = "verify-setup")]
    VerifySetup {
        /// The TOTP verification token.
        #[arg(long)]
        token: String,
    },
}

/// API key subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ApiKeyCommands {
    /// Create a new API key.
    Create {
        /// Key label / memo.
        #[arg(long)]
        name: Option<String>,
        /// Scopes as a JSON array string.
        #[arg(long)]
        scopes: Option<String>,
    },
    /// List all API keys.
    List,
    /// Delete an API key.
    Delete {
        /// The API key ID to delete.
        #[arg(long)]
        key_id: String,
    },
    /// Get API key details.
    Get {
        /// The API key ID.
        key_id: String,
    },
    /// Update an API key.
    Update {
        /// The API key ID.
        key_id: String,
        /// New label / memo.
        #[arg(long)]
        name: Option<String>,
        /// New scopes.
        #[arg(long)]
        scopes: Option<String>,
    },
}

/// OAuth session subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OauthCommands {
    /// List OAuth sessions.
    List,
    /// Get OAuth session details.
    Details {
        /// Session ID.
        session_id: String,
    },
    /// Revoke a single session.
    Revoke {
        /// Session ID.
        session_id: String,
    },
    /// Revoke all sessions.
    #[command(name = "revoke-all")]
    RevokeAll,
}

// ─── User ────────────────────────────────────────────────────────────────────

/// User subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserCommands {
    /// Get current user profile.
    Info,
    /// Update user profile.
    Update {
        /// First name.
        #[arg(long)]
        first_name: Option<String>,
        /// Last name.
        #[arg(long)]
        last_name: Option<String>,
        /// Display name.
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Manage user avatar.
    #[command(subcommand)]
    Avatar(UserAvatarCommands),
    /// Manage user settings.
    #[command(subcommand)]
    Settings(UserSettingsCommands),
    /// Search for users.
    Search {
        /// Search query.
        query: String,
    },
    /// Close/delete the current account.
    Close {
        /// Confirmation string.
        confirmation: String,
    },
    /// Get user details by ID.
    Details {
        /// User ID.
        user_id: String,
    },
    /// List accessible profile types.
    Profiles,
    /// Check country authorization.
    Allowed,
    /// Check org creation eligibility.
    #[command(name = "org-limits")]
    OrgLimits,
    /// List the user's shares.
    Shares,
    /// User invitations management.
    #[command(subcommand)]
    Invitations(UserInvitationsCommands),
    /// User asset management.
    #[command(subcommand)]
    Asset(UserAssetCommands),
    /// Enable or disable photo auto-sync from SSO providers.
    Autosync {
        /// State: "enable" or "disable".
        #[arg(value_parser = ["enable", "disable"])]
        state: String,
    },
    /// Get support PIN and identity verification hash.
    Pin,
    /// Validate a phone number.
    Phone {
        /// Country code (e.g. "1" for US).
        #[arg(long)]
        country_code: String,
        /// Phone number (e.g. "5551234567").
        #[arg(long)]
        phone_number: String,
    },
}

/// User invitations subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserInvitationsCommands {
    /// List pending invitations.
    List,
    /// Get invitation details.
    Details {
        /// Invitation ID.
        invitation_id: String,
    },
    /// Accept all pending invitations.
    #[command(name = "accept-all")]
    AcceptAll,
}

/// User asset subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserAssetCommands {
    /// List available asset types.
    Types,
    /// List user assets.
    List {
        /// User ID.
        user_id: String,
    },
    /// Delete a user asset.
    Delete {
        /// Asset type name.
        asset_type: String,
    },
    /// Upload a user asset (e.g. `profile_pic`).
    Upload {
        /// Asset type name (e.g. `profile_pic`).
        #[arg(long)]
        asset_type: String,
        /// Path to the file to upload.
        #[arg(long)]
        file: String,
    },
    /// Read/download a user asset binary.
    Read {
        /// User ID.
        #[arg(long)]
        user_id: String,
        /// Asset type name (e.g. `profile_pic`).
        #[arg(long)]
        asset_type: String,
        /// Output file path.
        #[arg(long)]
        output: String,
    },
}

/// User avatar subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserAvatarCommands {
    /// Upload an avatar image.
    Upload {
        /// Path to the image file.
        file: String,
    },
    /// Remove the current avatar.
    Remove,
}

/// User settings subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UserSettingsCommands {
    /// Get user settings.
    Get,
    /// Update user settings.
    Update {
        /// First name.
        #[arg(long)]
        first_name: Option<String>,
        /// Last name.
        #[arg(long)]
        last_name: Option<String>,
    },
}

// ─── Org ─────────────────────────────────────────────────────────────────────

/// Organization subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgCommands {
    /// List your organizations.
    List {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new organization.
    Create {
        /// Organization display name.
        name: String,
        /// URL-safe subdomain for the organization.
        #[arg(long)]
        domain: String,
        /// Organization description.
        #[arg(long)]
        description: Option<String>,
        /// Industry type (e.g. technology, healthcare).
        #[arg(long)]
        industry: Option<String>,
        /// Billing contact email.
        #[arg(long)]
        billing_email: Option<String>,
    },
    /// Get organization details.
    Info {
        /// Organization ID or domain.
        org_id: String,
    },
    /// Update organization settings.
    Update {
        /// Organization ID.
        org_id: String,
        /// New display name.
        #[arg(long)]
        name: Option<String>,
        /// New domain.
        #[arg(long)]
        domain: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New industry.
        #[arg(long)]
        industry: Option<String>,
        /// Billing email.
        #[arg(long)]
        billing_email: Option<String>,
        /// Homepage URL.
        #[arg(long)]
        homepage_url: Option<String>,
    },
    /// Delete (close) an organization. Permanent and irreversible.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Confirmation string (must match org domain or ID).
        #[arg(long)]
        confirm: String,
    },
    /// Billing information and management.
    #[command(subcommand)]
    Billing(OrgBillingCommands),
    /// Organization member management.
    #[command(subcommand)]
    Members(OrgMembersCommands),
    /// Transfer organization ownership.
    Transfer {
        /// Organization ID.
        org_id: String,
        /// User ID of the new owner.
        new_owner_id: String,
    },
    /// Discover organizations you can join.
    Discover {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get public organization info.
    #[command(name = "public-details")]
    PublicDetails {
        /// Organization ID.
        org_id: String,
    },
    /// Get plan limits.
    ///
    /// Hidden: credit usage moved under `org billing usage` (which keeps a
    /// hidden `limits` alias). This top-level command still ROUTES for one-release
    /// back-compat but is hidden from help.
    #[command(hide = true)]
    Limits {
        /// Organization ID.
        org_id: String,
    },
    /// Org invitation management.
    #[command(subcommand)]
    Invitations(OrgInvitationsCommands),
    /// Transfer token management.
    #[command(subcommand, name = "transfer-token")]
    TransferToken(OrgTransferTokenCommands),
    /// Claim org ownership via transfer token.
    #[command(name = "transfer-claim")]
    TransferClaim {
        /// Transfer token string.
        token: String,
    },
    /// Discover all organizations.
    #[command(name = "discover-all")]
    DiscoverAll {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Discover available organizations.
    #[command(name = "discover-available")]
    DiscoverAvailable {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Check domain availability.
    #[command(name = "discover-check-domain")]
    DiscoverCheckDomain {
        /// Domain to check.
        domain: String,
    },
    /// List external organizations.
    #[command(name = "discover-external")]
    DiscoverExternal {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// List org workspaces.
    Workspaces {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// List org shares.
    Shares {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Org asset management.
    #[command(subcommand, name = "asset")]
    OrgAsset(OrgAssetCommands),
    /// Create workspace in org.
    #[command(name = "create-workspace")]
    CreateWorkspace {
        /// Organization ID.
        org_id: String,
        /// Workspace name.
        name: String,
        /// Folder name.
        #[arg(long)]
        folder_name: Option<String>,
        /// Description.
        #[arg(long)]
        description: Option<String>,
    },
}

/// Org billing subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgBillingCommands {
    /// Get billing details for an organization.
    ///
    /// Renamed from `info` (kept as a hidden back-compat alias).
    #[command(alias = "info")]
    Details {
        /// Organization ID.
        org_id: String,
    },
    /// List available billing plans.
    Plans,
    /// Get credit usage and limits for an organization.
    ///
    /// Renamed from `limits` (kept as a hidden back-compat alias).
    #[command(alias = "limits")]
    Usage {
        /// Organization ID.
        org_id: String,
    },
    /// Get usage meters/metrics for an organization.
    Meters {
        /// Organization ID.
        org_id: String,
        /// Meter type (e.g. `storage_bytes`, `bandwidth_bytes`, `ai_tokens`).
        #[arg(long)]
        meter: String,
        /// Start time for the meter range.
        #[arg(long)]
        start_time: Option<String>,
        /// End time for the meter range.
        #[arg(long)]
        end_time: Option<String>,
        /// Filter by workspace ID (mutually exclusive with `--share-id`).
        #[arg(long)]
        workspace_id: Option<String>,
        /// Filter by share ID (mutually exclusive with `--workspace-id`).
        #[arg(long)]
        share_id: Option<String>,
    },
    /// Schedule a subscription to cancel at the end of the billing period.
    Cancel {
        /// Organization ID.
        org_id: String,
        /// Confirm the scheduled cancellation.
        #[arg(long)]
        yes: bool,
    },
    /// Reactivate a subscription scheduled to cancel (owner-only).
    Reactivate {
        /// Organization ID.
        org_id: String,
    },
    /// Deprecated: removed. Use `reactivate` (hidden compat shim, no network).
    #[command(hide = true)]
    Activate {
        /// Organization ID.
        org_id: String,
    },
    /// Deprecated: removed. Use `reactivate` (hidden compat shim, no network).
    #[command(hide = true)]
    Reset {
        /// Organization ID.
        org_id: String,
    },
    /// List billable members.
    Members {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Subscribe to a paid plan.
    ///
    /// Renamed from `create` (kept as a hidden back-compat alias).
    #[command(alias = "create")]
    Subscribe {
        /// Organization ID.
        org_id: String,
        /// Plan ID (e.g. `solo_monthly`, `business_v2_monthly`, `growth_monthly`).
        ///
        /// Accepts the legacy `--plan-id` spelling as a hidden alias so the
        /// old `org billing create <org> --plan-id <id>` invocation keeps
        /// parsing for one-release back-compat.
        #[arg(long, alias = "plan-id")]
        plan: String,
    },
    /// List billing invoices (cursor-paginated).
    Invoices {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Invoice-ID cursor: return invoices after this ID.
        #[arg(long)]
        starting_after: Option<String>,
    },
}

/// Org members subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgMembersCommands {
    /// List organization members.
    List {
        /// Organization ID.
        org_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Invite a member to the organization.
    Invite {
        /// Organization ID.
        org_id: String,
        /// Email address to invite.
        email: String,
        /// Role: admin, member, or guest.
        #[arg(long)]
        role: Option<String>,
    },
    /// Remove a member from the organization.
    Remove {
        /// Organization ID.
        org_id: String,
        /// Member user ID or email to remove.
        member_id: String,
    },
    /// Update a member's role.
    #[command(name = "update-role")]
    UpdateRole {
        /// Organization ID.
        org_id: String,
        /// Member user ID to update.
        member_id: String,
        /// New role: admin, member, or guest.
        role: String,
    },
    /// Get member details.
    Details {
        /// Organization ID.
        org_id: String,
        /// Member user ID.
        member_id: String,
    },
    /// Leave organization.
    Leave {
        /// Organization ID.
        org_id: String,
    },
    /// Join organization.
    Join {
        /// Organization ID.
        org_id: String,
    },
}

/// Org invitations subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgInvitationsCommands {
    /// List org invitations.
    List {
        /// Organization ID.
        org_id: String,
        /// Filter by state.
        #[arg(long)]
        state: Option<String>,
        /// Max results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Update an invitation.
    Update {
        /// Organization ID.
        org_id: String,
        /// Invitation ID.
        invitation_id: String,
        /// New state.
        #[arg(long)]
        state: Option<String>,
        /// New role.
        #[arg(long)]
        role: Option<String>,
    },
    /// Delete an invitation.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Invitation ID.
        invitation_id: String,
    },
}

/// Org transfer token subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgTransferTokenCommands {
    /// Create a transfer token.
    Create {
        /// Organization ID.
        org_id: String,
    },
    /// List transfer tokens.
    List {
        /// Organization ID.
        org_id: String,
        /// Max results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Delete a transfer token.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Token ID.
        token_id: String,
    },
}

/// Org asset subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum OrgAssetCommands {
    /// List available asset types.
    Types,
    /// List org assets.
    List {
        /// Organization ID.
        org_id: String,
    },
    /// Delete an org asset.
    Delete {
        /// Organization ID.
        org_id: String,
        /// Asset type name.
        asset_type: String,
    },
}

// ─── Workspace ───────────────────────────────────────────────────────────────

/// Workspace subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorkspaceCommands {
    /// List all workspaces.
    List {
        /// Filter by organization ID.
        #[arg(long)]
        org: Option<String>,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new workspace.
    Create {
        /// Workspace display name.
        name: String,
        /// Organization ID to create the workspace in.
        #[arg(long)]
        org: String,
        /// URL-safe folder name (derived from name if omitted).
        #[arg(long)]
        folder_name: Option<String>,
        /// Workspace description.
        #[arg(long)]
        description: Option<String>,
        /// Enable AI intelligence features.
        #[arg(long)]
        intelligence: Option<bool>,
    },
    /// Get workspace details.
    Info {
        /// Workspace ID or folder name.
        workspace_id: String,
    },
    /// Update workspace settings.
    Update {
        /// Workspace ID.
        workspace_id: String,
        /// New display name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New folder name.
        #[arg(long)]
        folder_name: Option<String>,
        /// Toggle AI indexing (intelligence). Enabling requires the
        /// `content_ai` and `ai_agent` plan features; disabling flushes
        /// embeddings and re-enabling re-indexes (costs AI credits).
        #[arg(long)]
        intelligence: Option<bool>,
    },
    /// Delete a workspace. Permanent and irreversible.
    Delete {
        /// Workspace ID.
        workspace_id: String,
        /// Confirmation string (must match workspace folder name or ID).
        #[arg(long)]
        confirm: String,
    },
    /// Enable the legacy workflow feature (tasks/worklogs/approvals/todos) on a
    /// workspace. Does not affect AI indexing — use `workspace update
    /// --intelligence` for that.
    #[command(name = "enable-workflow")]
    EnableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// Disable the legacy workflow feature on a workspace. Existing
    /// tasks/worklogs/approvals/todos are preserved but inaccessible until
    /// re-enabled.
    #[command(name = "disable-workflow")]
    DisableWorkflow {
        /// Workspace ID.
        workspace_id: String,
    },
    /// List active background jobs (poll after async metadata extract).
    #[command(name = "jobs-status")]
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
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get workspace limits and quotas.
    Limits {
        /// Workspace ID.
        workspace_id: String,
    },
}

// ─── Member ──────────────────────────────────────────────────────────────────

/// Member subcommands (workspace members).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum MemberCommands {
    /// List workspace members.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add a member to a workspace.
    Add {
        /// Email address or user ID to add.
        email: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Permission role: admin, member, or guest.
        #[arg(long)]
        role: Option<String>,
    },
    /// Remove a member from a workspace.
    Remove {
        /// Member ID to remove.
        member_id: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
    },
    /// Update a member's role.
    Update {
        /// Member ID to update.
        member_id: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// New role: admin, member, or guest.
        #[arg(long)]
        role: String,
    },
    /// Get member details.
    Info {
        /// Member ID.
        member_id: String,
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
    },
}

// ─── Invitation ──────────────────────────────────────────────────────────────

/// Invitation subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum InvitationCommands {
    /// List pending invitations for the current user.
    List {
        /// Max results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Accept an invitation (or all pending invitations).
    Accept {
        /// Invitation ID (omit to accept all).
        invitation_id: Option<String>,
    },
    /// Decline an invitation.
    Decline {
        /// Invitation ID.
        invitation_id: String,
        /// Entity type: workspace or share.
        #[arg(long)]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// Delete an invitation.
    Delete {
        /// Invitation ID.
        invitation_id: String,
        /// Entity type: workspace or share.
        #[arg(long)]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
}

// ─── Files ──────────────────────────────────────────────────────────────────

/// File and folder subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum FilesCommands {
    /// List files and folders in a workspace directory.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Parent folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Sort column: name, updated, created, type.
        #[arg(long, value_parser = ["name", "updated", "created", "type"])]
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        #[arg(long, value_parser = ["asc", "desc"])]
        sort_dir: Option<String>,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Get details for one or more files or folders.
    ///
    /// A single node ID (after dedup) returns the existing single-node
    /// response shape (`{node: {...}}`). Two or more unique IDs
    /// auto-route to the bulk `/storage/{ids}/details/` endpoint and
    /// return `{count_*, nodes: [...], errors: [...]}` (per-id errors
    /// are normal). Calls with more than 25 IDs are chunked
    /// client-side. The CLI accepts at most 1000 IDs per invocation
    /// to bound wall-time and rate-limit footprint — going over
    /// produces a clear error message (the runtime cap is enforced
    /// in `info()` rather than at clap-parse time so the message
    /// can include the actual count).
    Info {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// One or more storage node IDs (positional).
        #[arg(required = true, num_args = 1..)]
        node_ids: Vec<String>,
    },
    /// Create a new folder.
    #[command(name = "create-folder")]
    CreateFolder {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Folder name.
        name: String,
        /// Parent folder node ID (defaults to root).
        #[arg(long)]
        parent: Option<String>,
    },
    /// Move a file or folder to another location.
    Move {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to move.
        node_id: String,
        /// Destination folder node ID.
        #[arg(long)]
        to: String,
    },
    /// Copy a file or folder.
    Copy {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to copy.
        node_id: String,
        /// Destination folder node ID.
        #[arg(long)]
        to: String,
    },
    /// Rename a file or folder.
    Rename {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to rename.
        node_id: String,
        /// New name.
        new_name: String,
    },
    /// Delete a file or folder (move to trash).
    Delete {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to delete.
        node_id: String,
    },
    /// Restore a file or folder from trash.
    Restore {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to restore.
        node_id: String,
    },
    /// Permanently delete a trashed file or folder.
    Purge {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to permanently delete.
        node_id: String,
    },
    /// List items in the trash.
    Trash {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Sort column: name, updated, created, type.
        #[arg(long)]
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        #[arg(long)]
        sort_dir: Option<String>,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// List versions of a file.
    Versions {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Search for files in a workspace (keyword + semantic when intelligence
    /// is enabled).
    Search {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Search query.
        query: String,
        /// Maximum number of results (1-500; capped to 10 when --details).
        #[arg(long)]
        limit: Option<u32>,
        /// Result offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
        /// Comma-separated `nodeId:versionId` pairs (max 100) to narrow the
        /// searched files.
        #[arg(long)]
        scope: Option<String>,
        /// Comma-separated `nodeId:depth` pairs (max 100) to narrow the
        /// searched folders.
        #[arg(long)]
        folder_scope: Option<String>,
        /// Enrich each hit with the full node resource (caps default limit to 10).
        #[arg(long)]
        details: bool,
        /// [deprecated] Ignored — the search endpoint does not use keyset
        /// pagination. Use --limit/--offset instead.
        #[arg(long, hide = true)]
        page_size: Option<u32>,
        /// [deprecated] Ignored — the search endpoint does not use keyset
        /// pagination. Use --limit/--offset instead.
        #[arg(long, hide = true)]
        cursor: Option<String>,
    },
    /// List recently accessed files.
    Recent {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Add a share link to a folder.
    #[command(name = "add-link")]
    AddLink {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Parent folder node ID.
        parent: String,
        /// Share ID to link.
        share_id: String,
    },
    /// Transfer a node to another workspace.
    Transfer {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID to transfer.
        node_id: String,
        /// Target workspace ID.
        #[arg(long)]
        to_workspace: String,
    },
    /// Restore a specific version of a file.
    #[command(name = "version-restore")]
    VersionRestore {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
        /// Version ID.
        version_id: String,
    },
    /// File lock operations.
    #[command(subcommand)]
    Lock(FileLockCommands),
    /// Read file content (text).
    Read {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Create or get a quickshare link.
    Quickshare {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
}

/// File lock subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum FileLockCommands {
    /// Acquire a file lock.
    Acquire {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Check lock status.
    Status {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
    },
    /// Release a file lock.
    Release {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        #[arg(long)]
        lock_token: String,
    },
}

// ─── Upload ─────────────────────────────────────────────────────────────────

/// Upload subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum UploadCommands {
    /// Upload one or more local files.
    ///
    /// A single path uses the single-file pipeline (single-call for ≤ 4 MB,
    /// chunked otherwise). Two or more paths auto-route through the batch
    /// endpoint (`/upload/batch/`): small files are packed into sequential
    /// batches of ≤ 200 files / ≤ 100 MB, oversize files (> 4 MB) fall back
    /// to the chunked pipeline per file.
    File {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// One or more local files to upload.
        #[arg(num_args = 1.., required_unless_present = "preserve_tree")]
        file_paths: Vec<String>,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Upload an entire directory tree, preserving sub-folder structure
        /// via per-file `relative_path`. Mutually exclusive with positional
        /// file paths.
        #[arg(long, value_name = "DIR", conflicts_with = "file_paths")]
        preserve_tree: Option<String>,
        /// Exit 0 even if some files in a batch errored. Without this flag,
        /// any per-file error causes a nonzero exit with a summary.
        #[arg(long)]
        allow_partial: bool,
        /// Optional echo-back correlation tag (1-150 chars, alphanumeric and
        /// hyphens only). Passed through to the server on batch uploads.
        #[arg(long)]
        creator: Option<String>,
    },
    /// Upload text content as a file.
    Text {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filename for the uploaded file.
        #[arg(long)]
        name: String,
        /// Text content.
        content: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
    },
    /// Import a file from a URL.
    Url {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Source URL.
        url: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Override filename (derived from URL if omitted).
        #[arg(long)]
        name: Option<String>,
    },
    /// Create an upload session manually.
    #[command(name = "create-session")]
    CreateSession {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filename.
        filename: String,
        /// File size in bytes.
        filesize: u64,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
    },
    /// Upload a single chunk.
    Chunk {
        /// Upload key/ID.
        upload_key: String,
        /// Chunk number (1-based).
        chunk_num: u32,
        /// Path to chunk data file.
        file: String,
    },
    /// Trigger assembly after all chunks are uploaded.
    Finalize {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Check upload status.
    Status {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Cancel an upload.
    Cancel {
        /// Upload key/ID.
        upload_key: String,
    },
    /// List active upload sessions.
    #[command(name = "list-sessions")]
    ListSessions,
    /// Cancel all uploads.
    #[command(name = "cancel-all")]
    CancelAll,
    /// Check chunk status.
    #[command(name = "chunk-status")]
    ChunkStatus {
        /// Upload key/ID.
        upload_key: String,
    },
    /// Delete a chunk.
    #[command(name = "chunk-delete")]
    ChunkDelete {
        /// Upload key/ID.
        upload_key: String,
        /// Chunk number.
        chunk_num: u32,
    },
    /// List web imports.
    #[command(name = "web-list")]
    WebList,
    /// Cancel a web import.
    #[command(name = "web-cancel")]
    WebCancel {
        /// Upload ID.
        upload_id: String,
    },
    /// Check web import status.
    #[command(name = "web-status")]
    WebStatus {
        /// Upload ID.
        upload_id: String,
    },
    /// Get upload limits.
    Limits,
    /// Get restricted file extensions.
    Extensions,
    /// Upload a file via streaming (no exact size required upfront).
    Stream {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Path to the local file (use - for stdin).
        file_path: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Maximum upload size in bytes (defaults to plan limit).
        #[arg(long)]
        max_size: Option<u64>,
        /// Override filename (required for stdin, derived from path otherwise).
        #[arg(long)]
        name: Option<String>,
        /// Pre-computed hash of the file content for integrity verification.
        #[arg(long, requires = "hash_algo")]
        hash: Option<String>,
        /// Hash algorithm used (e.g. sha256). Requires --hash.
        #[arg(long, requires = "hash")]
        hash_algo: Option<String>,
    },
    /// Create a streaming upload session manually.
    #[command(name = "create-stream-session")]
    CreateStreamSession {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filename.
        filename: String,
        /// Destination folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Maximum upload size in bytes (defaults to plan limit).
        #[arg(long)]
        max_size: Option<u64>,
    },
    /// Send data to a streaming upload session (auto-finalizes).
    #[command(name = "stream-send")]
    StreamSend {
        /// Upload key/ID from create-stream-session.
        upload_key: String,
        /// Path to data file.
        file: String,
        /// Maximum file size in bytes (rejects before reading if exceeded).
        #[arg(long)]
        max_size: Option<u64>,
        /// Pre-computed hash of the file content.
        #[arg(long, requires = "hash_algo")]
        hash: Option<String>,
        /// Hash algorithm used (e.g. sha256). Requires --hash.
        #[arg(long, requires = "hash")]
        hash_algo: Option<String>,
    },
}

// ─── Download ───────────────────────────────────────────────────────────────

/// Download subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum DownloadCommands {
    /// Download a single file.
    File {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID of the file to download.
        node_id: String,
        /// Output file path (auto-determined if omitted).
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Download a folder as a ZIP archive.
    Folder {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID of the folder to download.
        node_id: String,
        /// Output file path (auto-determined if omitted).
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Download multiple files.
    Batch {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node IDs to download.
        node_ids: Vec<String>,
        /// Output directory (defaults to current directory).
        #[arg(long, short)]
        output_dir: Option<String>,
    },
}

// ─── Share ──────────────────────────────────────────────────────────────────

/// Share subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ShareCommands {
    /// List all shares.
    List {
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new share.
    Create {
        /// Share name/title.
        name: String,
        /// Workspace ID to create the share in.
        #[arg(long)]
        workspace: String,
        /// Share description.
        #[arg(long)]
        description: Option<String>,
        /// Access options.
        #[arg(long)]
        access_options: Option<String>,
        /// Password for share access.
        #[arg(long)]
        password: Option<String>,
        /// Enable anonymous uploads.
        #[arg(long)]
        anonymous_uploads: Option<bool>,
        /// Enable AI intelligence features.
        #[arg(long)]
        intelligence: Option<bool>,
        /// Download security level (high, medium, or off).
        #[arg(long, value_parser = ["high", "medium", "off"])]
        download_security: Option<String>,
    },
    /// Get share details.
    Info {
        /// Share ID or custom name.
        share_id: String,
    },
    /// Update share settings.
    Update {
        /// Share ID.
        share_id: String,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New access options.
        #[arg(long)]
        access_options: Option<String>,
        /// Enable or disable downloads (legacy — prefer --download-security).
        #[arg(long)]
        download_enabled: Option<bool>,
        /// Enable or disable comments.
        #[arg(long)]
        comments_enabled: Option<bool>,
        /// Enable or disable anonymous uploads.
        #[arg(long)]
        anonymous_uploads: Option<bool>,
        /// Download security level (high, medium, or off).
        #[arg(long, value_parser = ["high", "medium", "off"])]
        download_security: Option<String>,
    },
    /// Delete a share. Permanent and irreversible.
    Delete {
        /// Share ID.
        share_id: String,
        /// Confirmation string (must match share ID or custom name).
        #[arg(long)]
        confirm: String,
    },
    /// Archive a share.
    Archive {
        /// Share ID.
        share_id: String,
    },
    /// Unarchive a share.
    Unarchive {
        /// Share ID.
        share_id: String,
    },
    /// Authenticate to a password-protected share.
    PasswordAuth {
        /// Share ID.
        share_id: String,
        /// Password for the share.
        password: String,
    },
    /// Authenticate as a guest to a share.
    GuestAuth {
        /// Share ID.
        share_id: String,
    },
    /// Get public details for a share.
    PublicInfo {
        /// Share ID.
        share_id: String,
    },
    /// List available shares for the current user.
    Available,
    /// Check if a share name is available.
    CheckName {
        /// Share name to check.
        name: String,
    },
    /// Enable workflow on a share.
    WorkflowEnable {
        /// Share ID.
        share_id: String,
    },
    /// Disable workflow on a share.
    WorkflowDisable {
        /// Share ID.
        share_id: String,
    },
    /// Share file operations.
    #[command(subcommand)]
    Files(ShareFilesCommands),
    /// Share member operations.
    #[command(subcommand)]
    Members(ShareMembersCommands),
}

/// Share file subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ShareFilesCommands {
    /// List files and folders in a share.
    List {
        /// Share ID.
        share_id: String,
        /// Parent folder node ID (defaults to root).
        #[arg(long)]
        folder: Option<String>,
        /// Sort column: name, updated, created, type.
        #[arg(long, value_parser = ["name", "updated", "created", "type"])]
        sort_by: Option<String>,
        /// Sort direction: asc, desc.
        #[arg(long, value_parser = ["asc", "desc"])]
        sort_dir: Option<String>,
        /// Page size: 100, 250, 500.
        #[arg(long)]
        page_size: Option<u32>,
        /// Cursor for next page of results.
        #[arg(long)]
        cursor: Option<String>,
    },
}

/// Share member subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ShareMembersCommands {
    /// List share members.
    List {
        /// Share ID.
        share_id: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add a member to a share.
    Add {
        /// Share ID.
        share_id: String,
        /// Email address to add.
        email: String,
        /// Permission role: admin, member, or guest.
        #[arg(long)]
        role: Option<String>,
    },
    /// Remove a member from a share.
    Remove {
        /// Share ID.
        share_id: String,
        /// Member ID to remove.
        member_id: String,
    },
}

// ─── Comment ────────────────────────────────────────────────────────────────

/// Comment subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum CommentCommands {
    /// List comments on a file.
    List {
        /// Storage node ID.
        node_id: String,
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
        /// Sort order: created or -created.
        #[arg(long, value_parser = ["created", "-created"])]
        sort: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add a comment to a file.
    Create {
        /// Storage node ID.
        node_id: String,
        /// Comment text.
        text: String,
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
    },
    /// Reply to an existing comment.
    Reply {
        /// Comment ID to reply to.
        comment_id: String,
        /// Reply text.
        text: String,
        /// Storage node ID.
        #[arg(long)]
        node_id: String,
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
    },
    /// Delete a comment.
    Delete {
        /// Comment ID.
        comment_id: String,
    },
    /// List all comments across a workspace or share.
    ListAll {
        /// Entity type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
        /// Sort order: created or -created.
        #[arg(long, value_parser = ["created", "-created"])]
        sort: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get comment details.
    Info {
        /// Comment ID.
        comment_id: String,
    },
    /// Add an emoji reaction to a comment.
    React {
        /// Comment ID.
        comment_id: String,
        /// Emoji to react with (e.g. thumbsup, heart).
        emoji: String,
    },
    /// Remove your emoji reaction from a comment.
    Unreact {
        /// Comment ID.
        comment_id: String,
    },
}

// ─── Event ──────────────────────────────────────────────────────────────────

/// Event subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum EventCommands {
    /// List/search activity events.
    List {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get event details.
    Info {
        /// Event ID.
        event_id: String,
    },
    /// Long-poll for activity updates.
    Poll {
        /// Workspace or share ID to monitor.
        entity_id: String,
        /// Last activity timestamp for incremental polling.
        #[arg(long)]
        lastactivity: Option<String>,
        /// Max seconds the server will hold the connection (1-95).
        #[arg(long)]
        wait: Option<u32>,
    },
    /// Acknowledge an event.
    Ack {
        /// Event ID to acknowledge.
        event_id: String,
    },
    /// Get an AI-powered summary of events.
    Summarize {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Filter by subcategory.
        #[arg(long)]
        subcategory: Option<String>,
        /// Free-text context for the AI summarizer.
        #[arg(long)]
        user_context: Option<String>,
        /// Maximum number of events to include.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
}

// ─── Preview ────────────────────────────────────────────────────────────────

/// Preview subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum PreviewCommands {
    /// Get a preauthorized preview URL.
    Get {
        /// Storage node ID.
        node_id: String,
        /// Preview type.
        #[arg(long, value_parser = ["binary", "thumbnail", "image", "pdf", "hlsstream", "audio", "spreadsheet"])]
        preview_type: String,
        /// Context type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
    },
    /// Get a thumbnail preview URL (shorthand for --preview-type thumbnail).
    Thumbnail {
        /// Storage node ID.
        node_id: String,
        /// Context type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
    },
    /// Request a file transformation URL (resize, crop, format conversion).
    Transform {
        /// Storage node ID.
        node_id: String,
        /// Transform name (e.g. "image").
        #[arg(long)]
        transform_name: String,
        /// Context type: workspace or share.
        #[arg(long, value_parser = ["workspace", "share"])]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// Target width in pixels.
        #[arg(long)]
        width: Option<u32>,
        /// Target height in pixels.
        #[arg(long)]
        height: Option<u32>,
        /// Output format: png, jpg, webp.
        #[arg(long)]
        output_format: Option<String>,
        /// Size preset: `IconSmall`, `IconMedium`, Preview.
        #[arg(long)]
        size: Option<String>,
    },
}

// ─── Asset ──────────────────────────────────────────────────────────────────

/// Asset subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum AssetCommands {
    /// Upload an asset (logo, banner, etc.).
    Upload {
        /// Asset type name (e.g. logo, banner, photo).
        asset_type: String,
        /// Path to the file to upload.
        file: String,
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// Remove an asset.
    Remove {
        /// Asset type name.
        asset_type: String,
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// List assets on an entity.
    List {
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
    },
    /// List available asset types.
    Types {
        /// Entity type: org, workspace, or share.
        #[arg(long, value_parser = ["org", "workspace", "share"])]
        entity_type: String,
    },
}

// ─── AI ─────────────────────────────────────────────────────────────────────

/// Ripley (AI agent) subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum RipleyCommands {
    /// Ask Ripley a question and wait for the answer (headline verb).
    Ask {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// The question to ask.
        question: String,
        /// Scope to specific file versions: comma-separated `nodeId:versionId`
        /// pairs (max 100). Cannot be combined with `--files-attach`.
        #[arg(long)]
        files_scope: Option<String>,
        /// Scope to folders: comma-separated `nodeId:depth` pairs (max 100,
        /// depth 1-10). Cannot be combined with `--files-attach`.
        #[arg(long)]
        folders_scope: Option<String>,
        /// Attach specific file versions: comma-separated `nodeId:versionId`
        /// pairs (max 20). Cannot be combined with the scope flags.
        #[arg(long)]
        files_attach: Option<String>,
        /// Response style.
        #[arg(long, value_parser = ["concise", "detailed"])]
        personality: Option<String>,
        /// Chat kind (workspace-only; `agent` requires the `ai_agent` plan feature).
        #[arg(long, value_parser = ["user", "agent"])]
        kind: Option<String>,
        /// Return the chat/message IDs immediately without waiting for the answer.
        #[arg(long)]
        no_wait: bool,
    },
    /// Send a chat message and get the AI response.
    Chat {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// User message text.
        message: String,
        /// Existing chat ID (creates new if omitted).
        #[arg(long)]
        chat_id: Option<String>,
        /// Scope the chat to specific file versions: comma-separated
        /// `nodeId:versionId` pairs (max 100). Both parts are required.
        #[arg(long)]
        files_scope: Option<String>,
        /// Scope the chat to folders: comma-separated `nodeId:depth` pairs
        /// (max 100, depth 1-10).
        #[arg(long)]
        folders_scope: Option<String>,
        /// Attach specific file versions to the chat: comma-separated
        /// `nodeId:versionId` pairs.
        #[arg(long)]
        files_attach: Option<String>,
        /// [deprecated] Scope to file node IDs (comma-separated); prefer
        /// `--files-scope nodeId:versionId`. Bare node IDs (no version) are
        /// rejected because the API requires a non-empty version per pair.
        #[arg(long, value_delimiter = ',', hide = true)]
        node_ids: Option<Vec<String>>,
        /// [deprecated] Folder ID to scope to; mapped to `folders_scope=<id>:10`.
        /// Prefer `--folders-scope nodeId:depth`.
        #[arg(long, hide = true)]
        folder_id: Option<String>,
        /// [deprecated] No longer maps to a chat parameter; accepted but ignored.
        #[arg(long, hide = true)]
        intelligence: Option<bool>,
    },
    /// Semantic search over indexed workspace files.
    Search {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Search query.
        query: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get chat message history.
    History {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Chat ID (lists all chats if omitted).
        #[arg(long)]
        chat_id: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Generate a shareable AI summary from specific workspace files.
    Summary {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// File node IDs to include in the summary (at least one required).
        node_ids: Vec<String>,
    },
    /// Cancel an in-progress chat message (idempotent; safe when nothing is pending).
    Cancel {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Chat ID.
        #[arg(long)]
        chat_id: String,
    },
    /// List the caller's chats.
    List {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Filter by chat kind.
        #[arg(long, value_parser = ["user", "agent", "all"])]
        kind: Option<String>,
        /// List soft-deleted chats instead.
        #[arg(long)]
        deleted: bool,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Show full details and history for a chat.
    Details {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Chat ID.
        chat_id: String,
    },
    /// List messages in a chat (oldest-first).
    Messages {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Chat ID.
        chat_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Show a single message's details.
    Message {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Chat ID.
        chat_id: String,
        /// Message ID.
        message_id: String,
    },
    /// Rename a chat.
    Update {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Chat ID.
        chat_id: String,
        /// New chat name.
        #[arg(long)]
        name: String,
    },
    /// Publish a private chat (make it public; one-way).
    Publish {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Chat ID.
        chat_id: String,
    },
    /// Soft-delete a chat.
    Delete {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Chat ID.
        chat_id: String,
    },
    /// List recent AI token-usage transactions (workspace-only).
    Transactions {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
    },
    /// AI-generate a title and description for a share (share-only).
    Autotitle {
        /// Share ID.
        #[arg(long)]
        share: String,
        /// Optional context to guide generation.
        #[arg(long)]
        user_context: Option<String>,
    },
    /// Manage the caller's AI-memory blob (org or workspace; self-only).
    #[command(subcommand)]
    Memory(RipleyMemoryCommands),
    /// Hand work to Ripley to run on your behalf (not yet available).
    #[command(hide = true, alias = "run")]
    Delegate {
        /// Workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// The instruction to delegate.
        instruction: String,
    },
    /// Show the status of a delegated job (not yet available).
    #[command(hide = true)]
    Status {
        /// Delegated-job ID.
        id: String,
    },
    /// Show the tool-call log of a delegated job (not yet available).
    #[command(hide = true)]
    Logs {
        /// Delegated-job ID.
        id: String,
    },
    /// Cancel an in-flight delegated job (not yet available).
    #[command(hide = true, name = "cancel-job")]
    CancelJob {
        /// Delegated-job ID.
        id: String,
    },
}

/// Ripley AI-memory subcommands (self-only; org or workspace scope).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum RipleyMemoryCommands {
    /// Read the caller's AI-memory blob.
    Get {
        /// Organization ID.
        #[arg(long, required_unless_present = "workspace")]
        org: Option<String>,
        /// Workspace ID (alternative to org).
        #[arg(long, conflicts_with = "org")]
        workspace: Option<String>,
    },
    /// Write the caller's AI-memory blob (≤64KB; optional revision CAS).
    Set {
        /// Organization ID.
        #[arg(long, required_unless_present = "workspace")]
        org: Option<String>,
        /// Workspace ID (alternative to org).
        #[arg(long, conflicts_with = "org")]
        workspace: Option<String>,
        /// New markdown content (max 64KB; empty string stores an empty row).
        content: String,
        /// Optimistic-concurrency revision: write only if the row's current
        /// revision matches (409 on mismatch).
        #[arg(long)]
        revision: Option<u64>,
    },
    /// Hard-delete the caller's AI-memory blob.
    Delete {
        /// Organization ID.
        #[arg(long, required_unless_present = "workspace")]
        org: Option<String>,
        /// Workspace ID (alternative to org).
        #[arg(long, conflicts_with = "org")]
        workspace: Option<String>,
    },
}

// ─── Task ───────────────────────────────────────────────────────────────────

/// Task subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TaskCommands {
    /// List tasks in a workspace.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filter by task list ID.
        #[arg(long)]
        list_id: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new task.
    Create {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task title.
        title: String,
        /// Task description.
        #[arg(long)]
        description: Option<String>,
        /// Status: pending, `in_progress`, complete, blocked.
        #[arg(long, value_parser = ["pending", "in_progress", "complete", "blocked"])]
        status: Option<String>,
        /// Priority: 0=none, 1=low, 2=medium, 3=high, 4=critical.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: Option<u8>,
        /// Assignee profile ID.
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Get task details.
    Info {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Update a task.
    Update {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
        /// New title.
        #[arg(long)]
        title: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
        /// New status: pending, `in_progress`, complete, blocked.
        #[arg(long, value_parser = ["pending", "in_progress", "complete", "blocked"])]
        status: Option<String>,
        /// New priority: 0=none, 1=low, 2=medium, 3=high, 4=critical.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: Option<u8>,
        /// New assignee profile ID.
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Delete a task.
    Delete {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Assign a task to a user.
    Assign {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
        /// Assignee profile ID (omit to unassign).
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Mark a task as complete.
    Complete {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
    },
    /// Move a task to a different list.
    Move {
        /// Source task list ID.
        #[arg(long)]
        list_id: String,
        /// Task ID.
        task_id: String,
        /// Target task list ID.
        #[arg(long)]
        target_list_id: String,
        /// Sort order in the target list.
        #[arg(long)]
        sort_order: Option<u32>,
    },
    /// Bulk change status for multiple tasks in a list.
    #[command(name = "bulk-status")]
    BulkStatus {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Comma-separated task IDs.
        #[arg(long)]
        task_ids: String,
        /// New status: pending, `in_progress`, complete, blocked.
        #[arg(long, value_parser = ["pending", "in_progress", "complete", "blocked"])]
        status: String,
    },
    /// Reorder tasks within a list.
    Reorder {
        /// Task list ID.
        #[arg(long)]
        list_id: String,
        /// Comma-separated task IDs in desired order.
        #[arg(long)]
        task_ids: String,
    },
    /// Reorder task lists in a workspace or share.
    #[command(name = "reorder-lists")]
    ReorderLists {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace")]
        profile_type: String,
        /// Workspace or share ID.
        #[arg(long)]
        profile_id: String,
        /// Comma-separated list IDs in desired order.
        #[arg(long)]
        list_ids: String,
    },
    /// Filtered task list (personal view on a workspace, group view on a share).
    Filter {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Filter: assigned, created, status.
        #[arg(value_parser = ["assigned", "created", "status"])]
        filter: String,
        /// Status (required for `status` filter; optional for assigned/created).
        #[arg(long, value_parser = ["pending", "in_progress", "complete", "blocked"])]
        status: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Task count summary for a workspace or share.
    Summary {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
    },
    /// Manage task lists.
    #[command(subcommand)]
    Lists(TaskListCommands),
}

/// Task list subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TaskListCommands {
    /// List all task lists in a workspace or share.
    List {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new task list in a workspace or share.
    Create {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace")]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Task list name.
        name: String,
        /// Task list description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Update a task list.
    Update {
        /// Task list ID.
        list_id: String,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        /// New description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Delete a task list.
    Delete {
        /// Task list ID.
        list_id: String,
    },
}

// ─── Worklog ────────────────────────────────────────────────────────────────

/// Worklog subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum WorklogCommands {
    /// List worklog entries.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Entity type (profile, task, `task_list`, node). Defaults to "profile".
        #[arg(long, value_parser = ["profile", "task", "task_list", "node"])]
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        #[arg(long)]
        entity_id: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Append a worklog entry.
    Append {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Message content.
        message: String,
        /// Entity type (profile, task, `task_list`, node). Defaults to "profile".
        #[arg(long, value_parser = ["profile", "task", "task_list", "node"])]
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        #[arg(long)]
        entity_id: Option<String>,
    },
    /// Create an interjection (urgent entry requiring acknowledgement).
    Interject {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Message content.
        message: String,
        /// Entity type (profile, task, `task_list`, node). Defaults to "profile".
        #[arg(long, value_parser = ["profile", "task", "task_list", "node"])]
        entity_type: Option<String>,
        /// Entity ID (defaults to workspace ID).
        #[arg(long)]
        entity_id: Option<String>,
    },
    /// Get worklog entry details.
    Details {
        /// Worklog entry ID.
        entry_id: String,
    },
    /// List unacknowledged interjections for an entity.
    #[command(name = "list-interjections")]
    ListInterjections {
        /// Entity type (profile, task, `task_list`, node).
        #[arg(long, value_parser = ["profile", "task", "task_list", "node"])]
        entity_type: String,
        /// Entity ID.
        #[arg(long)]
        entity_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Acknowledge a worklog interjection.
    Acknowledge {
        /// Worklog entry ID to acknowledge.
        entry_id: String,
    },
    /// List all worklog entries in a workspace or share.
    #[command(name = "list-all")]
    ListAll {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Filtered worklog list (personal view on a workspace, group view on a share).
    Filter {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Filter: authored, interjections.
        #[arg(value_parser = ["authored", "interjections"])]
        filter: String,
        /// Entry type (authored filter only).
        #[arg(long, value_parser = ["info", "decision", "error", "status_change", "request", "interjection"])]
        entry_type: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Worklog entry summary for a workspace or share.
    Summary {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
    },
}

// ─── Approval ───────────────────────────────────────────────────────────────

/// Approval subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ApprovalCommands {
    /// List approvals in a workspace.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filter by status: pending, approved, rejected.
        #[arg(long, value_parser = ["pending", "approved", "rejected"])]
        status: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Request an approval.
    Request {
        /// Profile type the approval is scoped to: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Entity type: task, node, `worklog_entry`, or share.
        #[arg(long, value_parser = ["task", "node", "worklog_entry", "share"])]
        entity_type: String,
        /// Entity ID.
        entity_id: String,
        /// Description of what needs approval.
        #[arg(long)]
        description: String,
        /// Designated approver profile ID.
        #[arg(long)]
        approver_id: Option<String>,
        /// Informational deadline (YYYY-MM-DD HH:MM:SS).
        #[arg(long)]
        deadline: Option<String>,
        /// Associated artifact node ID.
        #[arg(long)]
        node_id: Option<String>,
        /// Metadata properties as a JSON object string.
        #[arg(long)]
        properties: Option<String>,
    },
    /// Get approval details.
    Info {
        /// Profile type the approval is scoped to: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace). Omit to use the
        /// legacy unscoped route.
        #[arg(long, alias = "workspace")]
        profile_id: Option<String>,
        /// Approval ID.
        approval_id: String,
    },
    /// Approve an approval request.
    Approve {
        /// Profile type the approval is scoped to: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace). Omit to use the
        /// legacy unscoped route.
        #[arg(long, alias = "workspace")]
        profile_id: Option<String>,
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        #[arg(long)]
        comment: Option<String>,
    },
    /// Reject an approval request.
    Reject {
        /// Profile type the approval is scoped to: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace). Omit to use the
        /// legacy unscoped route.
        #[arg(long, alias = "workspace")]
        profile_id: Option<String>,
        /// Approval ID.
        approval_id: String,
        /// Optional comment.
        #[arg(long)]
        comment: Option<String>,
    },
    /// Update a pending approval (at least one field required).
    Update {
        /// Profile type the approval is scoped to: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace). Omit to use the
        /// legacy unscoped route.
        #[arg(long, alias = "workspace")]
        profile_id: Option<String>,
        /// Approval ID.
        approval_id: String,
        /// Updated description.
        #[arg(long)]
        description: Option<String>,
        /// Updated designated approver profile ID.
        #[arg(long)]
        approver_id: Option<String>,
        /// Updated deadline (YYYY-MM-DD HH:MM:SS).
        #[arg(long)]
        deadline: Option<String>,
        /// Updated associated node ID.
        #[arg(long)]
        node_id: Option<String>,
        /// Updated metadata properties as a JSON object string.
        #[arg(long)]
        properties: Option<String>,
    },
    /// Delete an approval (pending or resolved).
    Delete {
        /// Profile type the approval is scoped to: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace). Omit to use the
        /// legacy unscoped route.
        #[arg(long, alias = "workspace")]
        profile_id: Option<String>,
        /// Approval ID.
        approval_id: String,
    },
    /// Filtered approval list (personal view on a workspace, group view on a share).
    Filter {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Filter: pending, created, assigned, resolved.
        #[arg(value_parser = ["pending", "created", "assigned", "resolved"])]
        filter: String,
        /// Status filter (created/assigned only): pending, approved, rejected.
        #[arg(long, value_parser = ["pending", "approved", "rejected"])]
        status: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Approval count summary for a workspace or share.
    Summary {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share profile ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
    },
    /// List the authenticated user's approvals across all profiles.
    Mine {
        /// Filter: pending, created, resolved.
        #[arg(value_parser = ["pending", "created", "resolved"])]
        filter: String,
        /// Status filter (created only): pending, approved, rejected.
        #[arg(long, value_parser = ["pending", "approved", "rejected"])]
        status: Option<String>,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
}

// ─── Todo ───────────────────────────────────────────────────────────────────

/// Todo subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum TodoCommands {
    /// List todos in a workspace or share.
    List {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace")]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Create a new todo in a workspace or share.
    Create {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Todo title.
        title: String,
        /// Assignee profile ID.
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Update a todo.
    Update {
        /// Todo ID.
        todo_id: String,
        /// New title.
        #[arg(long)]
        title: Option<String>,
        /// Mark as done or not done.
        #[arg(long)]
        done: Option<bool>,
        /// New assignee profile ID.
        #[arg(long)]
        assignee_id: Option<String>,
    },
    /// Toggle a todo's completion state.
    Toggle {
        /// Todo ID.
        todo_id: String,
    },
    /// Delete a todo.
    Delete {
        /// Todo ID.
        todo_id: String,
    },
    /// Bulk toggle todo completion in a workspace or share.
    #[command(name = "bulk-toggle")]
    BulkToggle {
        /// Workspace ID.
        #[arg(long, required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID (alternative to workspace).
        #[arg(long, conflicts_with = "workspace")]
        share: Option<String>,
        /// Comma-separated todo IDs.
        #[arg(long)]
        todo_ids: String,
        /// Set completion state (true = done, false = not done).
        #[arg(long, default_value = "true")]
        done: bool,
    },
    /// Filtered todo list (personal view on a workspace, group view on a share).
    Filter {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
        /// Filter: assigned, created, done, pending.
        #[arg(value_parser = ["assigned", "created", "done", "pending"])]
        filter: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Todo count summary for a workspace or share.
    Summary {
        /// Profile type: workspace or share.
        #[arg(long, default_value = "workspace", value_parser = ["workspace", "share"])]
        profile_type: String,
        /// Workspace or share ID (alias: --workspace).
        #[arg(long, alias = "workspace")]
        profile_id: String,
    },
}

// ─── Completions ─────────────────────────────────────────────────────────────

/// Supported shells for completion script generation.
#[derive(Clone, Copy, Debug, ValueEnum)]
#[non_exhaustive]
pub enum ShellType {
    /// Bash shell.
    Bash,
    /// Zsh shell.
    Zsh,
    /// Fish shell.
    Fish,
    /// `PowerShell`.
    Powershell,
}

impl std::fmt::Display for ShellType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bash => write!(f, "bash"),
            Self::Zsh => write!(f, "zsh"),
            Self::Fish => write!(f, "fish"),
            Self::Powershell => write!(f, "powershell"),
        }
    }
}

// ─── Configure ───────────────────────────────────────────────────────────────

/// Configuration management subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ConfigureCommands {
    /// Interactive profile setup.
    Init {
        /// Profile name to create or update.
        #[arg(long, default_value = "default")]
        name: String,
        /// API base URL.
        #[arg(long)]
        api_base: Option<String>,
        /// Authentication method: pkce, basic, or `api_key`.
        #[arg(long, value_parser = ["pkce", "basic", "api_key"])]
        auth_method: Option<String>,
    },
    /// List all configured profiles.
    List,
    /// Set the default profile.
    SetDefault {
        /// Profile name to set as default.
        name: String,
    },
    /// Show current configuration.
    Show,
    /// Delete a named profile.
    Delete {
        /// Profile name to delete.
        name: String,
    },
}

// ─── Apps ────────────────────────────────────────────────────────────────────

/// Apps subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum AppsCommands {
    /// List all available apps.
    List,
    /// Get details for a specific app.
    Details {
        /// App identifier.
        app_id: String,
    },
    /// Launch an app in a context.
    Launch {
        /// App identifier.
        app_id: String,
        /// Context type: workspace or share.
        #[arg(long)]
        context_type: String,
        /// Context ID.
        #[arg(long)]
        context_id: String,
    },
    /// List apps available for a specific tool.
    #[command(name = "tool-apps")]
    GetToolApps {
        /// Tool name.
        tool_name: String,
    },
}

// ─── Import ──────────────────────────────────────────────────────────────────

/// Cloud import subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ImportCommands {
    /// List available cloud import providers.
    #[command(name = "list-providers")]
    ListProviders {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
    },
    /// List provider identities.
    #[command(name = "list-identities")]
    ListIdentities {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Provision a new provider identity.
    #[command(name = "provision-identity")]
    ProvisionIdentity {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Cloud provider: `google_drive`, box, `onedrive_business`, dropbox.
        #[arg(long)]
        provider: String,
    },
    /// Get identity details.
    #[command(name = "identity-details")]
    IdentityDetails {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
    },
    /// Revoke a provider identity.
    #[command(name = "revoke-identity")]
    RevokeIdentity {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
    },
    /// List import sources.
    #[command(name = "list-sources")]
    ListSources {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Filter by status.
        #[arg(long)]
        status: Option<String>,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Discover shared folders from a provider.
    Discover {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
    },
    /// Create an import source.
    #[command(name = "create-source")]
    CreateSource {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
        /// Remote folder path.
        #[arg(long)]
        remote_path: String,
        /// Display name.
        #[arg(long)]
        remote_name: Option<String>,
        /// Sync interval in seconds (300-86400).
        #[arg(long)]
        sync_interval: Option<u32>,
        /// Access mode: `read_only` or `read_write`.
        #[arg(long)]
        access_mode: Option<String>,
    },
    /// Get source details.
    #[command(name = "source-details")]
    SourceDetails {
        /// Source ID.
        source_id: String,
    },
    /// Update source settings.
    #[command(name = "update-source")]
    UpdateSource {
        /// Source ID.
        source_id: String,
        /// Sync interval in seconds.
        #[arg(long)]
        sync_interval: Option<u32>,
        /// Status action: paused or synced.
        #[arg(long)]
        status: Option<String>,
        /// Display name.
        #[arg(long)]
        remote_name: Option<String>,
        /// Access mode: `read_only` or `read_write`.
        #[arg(long)]
        access_mode: Option<String>,
    },
    /// Delete a source.
    #[command(name = "delete-source")]
    DeleteSource {
        /// Source ID.
        source_id: String,
    },
    /// Disconnect source with keep/delete.
    Disconnect {
        /// Source ID.
        source_id: String,
        /// Action: keep or delete.
        #[arg(long, value_parser = ["keep", "delete"])]
        action: String,
    },
    /// Trigger immediate refresh sync.
    Refresh {
        /// Source ID.
        source_id: String,
    },
    /// List jobs for a source.
    #[command(name = "list-jobs")]
    ListJobs {
        /// Source ID.
        source_id: String,
        /// Max results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get job details.
    #[command(name = "job-details")]
    JobDetails {
        /// Source ID.
        source_id: String,
        /// Job ID.
        #[arg(long)]
        job_id: String,
    },
    /// Cancel a running job.
    #[command(name = "cancel-job")]
    CancelJob {
        /// Source ID.
        source_id: String,
        /// Job ID.
        #[arg(long)]
        job_id: String,
    },
    /// List write-back jobs.
    #[command(name = "list-writebacks")]
    ListWritebacks {
        /// Source ID.
        source_id: String,
        /// Filter by status.
        #[arg(long)]
        status: Option<String>,
        /// Max results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get write-back details.
    #[command(name = "writeback-details")]
    WritebackDetails {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
    },
    /// Push a file to remote storage.
    #[command(name = "push-writeback")]
    PushWriteback {
        /// Source ID.
        source_id: String,
        /// Node ID.
        #[arg(long)]
        node_id: String,
    },
    /// Retry a failed write-back.
    #[command(name = "retry-writeback")]
    RetryWriteback {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
    },
    /// Resolve a write-back conflict.
    #[command(name = "resolve-conflict")]
    ResolveConflict {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
        /// Resolution: `keep_local` or `keep_remote`.
        #[arg(long, value_parser = ["keep_local", "keep_remote"])]
        resolution: String,
    },
    /// Cancel a pending write-back.
    #[command(name = "cancel-writeback")]
    CancelWriteback {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        #[arg(long)]
        writeback_id: String,
    },
}

// ─── Lock ────────────────────────────────────────────────────────────────────

/// File locking subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum LockCommands {
    /// Acquire an exclusive lock on a file.
    Acquire {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
    },
    /// Check lock status for a file.
    Status {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
    },
    /// Release a lock on a file.
    Release {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        #[arg(long)]
        lock_token: String,
    },
    /// Renew (heartbeat) an existing lock on a file.
    Heartbeat {
        /// Context type: workspace or share.
        #[arg(long, default_value = "workspace")]
        context_type: String,
        /// Context ID (workspace or share ID).
        #[arg(long)]
        context_id: String,
        /// File node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        #[arg(long)]
        lock_token: String,
    },
}

// ─── Metadata ─────────────────────────────────────────────────────────────────

/// Metadata extraction subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum MetadataCommands {
    /// List files eligible for metadata extraction.
    Eligible {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Add files to a metadata template.
    #[command(name = "add-nodes")]
    AddNodes {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// JSON-encoded array of node IDs.
        #[arg(long)]
        node_ids: String,
    },
    /// Remove files from a metadata template.
    #[command(name = "remove-nodes")]
    RemoveNodes {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// JSON-encoded array of node IDs.
        #[arg(long)]
        node_ids: String,
    },
    /// List files mapped to a metadata template.
    #[command(name = "list-nodes")]
    ListNodes {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// Maximum number of results.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
        /// Template field name to sort by (optional).
        #[arg(long)]
        sort_field: Option<String>,
        /// Sort direction when --sort-field is set (asc or desc).
        #[arg(long, value_parser = ["asc", "desc"], requires = "sort_field")]
        sort_dir: Option<String>,
    },
    /// AI-based file matching for a template. SPENDS AI CREDITS — requires
    /// --confirm-ai-spend (or an interactive y/N confirmation on a TTY).
    #[command(name = "auto-match")]
    AutoMatch {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// Optional batch-size override (clamped server-side to the
        /// supported range). Omit to use the server default.
        #[arg(long)]
        batch_size: Option<u32>,
        /// Acknowledge that this is an AI-credit-spending action. Required
        /// to proceed non-interactively; on a TTY you are prompted instead.
        #[arg(long)]
        confirm_ai_spend: bool,
    },
    /// Batch extract metadata for all files in a template. SPENDS AI CREDITS
    /// — requires --confirm-ai-spend (or an interactive y/N confirmation on
    /// a TTY). Up to 1,000 files are processed per job.
    #[command(name = "extract-all")]
    ExtractAll {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID.
        #[arg(long)]
        template_id: String,
        /// JSON-encoded array of template field names for partial
        /// extraction (omit to extract every field).
        #[arg(long)]
        fields: Option<String>,
        /// Re-extract every mapped node even if it already has values for
        /// this template (default skips nodes that already have values).
        #[arg(long)]
        force: bool,
        /// Acknowledge that this is an AI-credit-spending action. Required
        /// to proceed non-interactively; on a TTY you are prompted instead.
        #[arg(long)]
        confirm_ai_spend: bool,
    },
    /// Get metadata details for one or more files.
    ///
    /// A single node ID (after dedup) returns the existing single-node
    /// response shape (the metadata object as the body). Two or more
    /// unique IDs auto-route to the bulk
    /// `/storage/{ids}/metadata/details/` endpoint and return
    /// `{count_*, objects: [...], templates: {...}, errors: [...]}`
    /// (per-id errors are normal). Calls with more than 25 IDs are
    /// chunked client-side. The CLI accepts at most 1000 IDs per
    /// invocation to bound wall-time and rate-limit footprint.
    Details {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// One or more storage node IDs (positional).
        #[arg(required = true, num_args = 1..)]
        node_ids: Vec<String>,
    },
    /// Enqueue an async metadata extraction for a single file. SPENDS AI
    /// CREDITS — requires --confirm-ai-spend (or an interactive y/N
    /// confirmation on a TTY). Usually returns a `job_id`; poll
    /// `workspace jobs-status` until status is "completed", then read
    /// values from the metadata details endpoint (or pass --wait to do
    /// this automatically). A full-row call whose effective scope is empty
    /// (every template field has `autoextract: false`) responds
    /// successfully without enqueueing a job — do not assume a `job_id` is
    /// always present.
    Extract {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// File node ID.
        #[arg(long)]
        node_id: String,
        /// Template ID to extract against (optional; defaults server-side
        /// to the first template mapped to the file).
        #[arg(long)]
        template_id: Option<String>,
        /// JSON-encoded array of field names for partial extraction
        /// (omit for full-row extraction).
        #[arg(long)]
        fields: Option<String>,
        /// Poll the workspace jobs-status endpoint until the extraction
        /// job reaches a terminal state, then report the outcome.
        #[arg(long)]
        wait: bool,
        /// Seconds between job-status polls when --wait is set (default 3,
        /// clamped to 1..=60).
        #[arg(long)]
        poll_interval: Option<u64>,
        /// Acknowledge that this is an AI-credit-spending action. Required
        /// to proceed non-interactively; on a TTY you are prompted instead.
        #[arg(long)]
        confirm_ai_spend: bool,
    },
    /// Preview files that would match a proposed template name + description.
    /// SPENDS AI CREDITS — requires --confirm-ai-spend (or an interactive y/N
    /// confirmation on a TTY).
    #[command(name = "preview-match")]
    PreviewMatch {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Proposed template name (1-255 chars).
        #[arg(long)]
        name: String,
        /// Natural-language description of the view/template.
        #[arg(long)]
        description: String,
        /// Acknowledge that this is an AI-credit-spending action. Required
        /// to proceed non-interactively; on a TTY you are prompted instead.
        #[arg(long)]
        confirm_ai_spend: bool,
    },
    /// Suggest custom columns for a proposed template (AI-assisted). SPENDS
    /// AI CREDITS — requires --confirm-ai-spend (or an interactive y/N
    /// confirmation on a TTY).
    #[command(name = "suggest-fields")]
    SuggestFields {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// JSON-encoded array of 1-25 sample node IDs from preview-match.
        #[arg(long)]
        node_ids: String,
        /// View description (also passed to preview-match).
        #[arg(long)]
        description: String,
        /// Optional short hint ("photo collection", max 64 chars, letters/numbers/spaces).
        #[arg(long)]
        user_context: Option<String>,
        /// Acknowledge that this is an AI-credit-spending action. Required
        /// to proceed non-interactively; on a TTY you are prompted instead.
        #[arg(long)]
        confirm_ai_spend: bool,
    },
    /// Create a metadata template (a.k.a. "view").
    #[command(name = "create-template")]
    CreateTemplate {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template name (shown as "view name" in the UI).
        #[arg(long)]
        name: String,
        /// Template description.
        #[arg(long)]
        description: String,
        /// Template category.
        #[arg(long)]
        category: String,
        /// JSON-encoded array of column definitions (compatible with suggest-fields output).
        #[arg(long)]
        fields: String,
    },
    /// Lexical keyword search over workspace metadata field values.
    ///
    /// Multi-token queries require ALL tokens to appear (case-insensitive).
    /// Substring matching applies for queries up to 64 chars; longer
    /// queries are matched word-by-word. Indexing is asynchronous (1–2 s)
    /// — do not search-immediately-after-write as a correctness check.
    Search {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Search keyword(s) (max 1024 chars; whitespace-trimmed).
        query: String,
        /// Restrict to nodes with at least one value contributed by this
        /// template (custom fields are excluded when set).
        #[arg(long)]
        template_id: Option<String>,
        /// Page size (1-100, default 100 server-side; combined with
        /// offset, must not exceed 10000).
        #[arg(long)]
        limit: Option<u32>,
        /// Skip-N offset (offset + limit may not exceed 10000).
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Enqueue an async TSV export of the caller's saved view for a template.
    ///
    /// The TSV is written into the destination folder by a background
    /// worker; poll the destination folder for the resulting filename.
    /// Same-view + same-destination calls while a prior job is in
    /// flight return `status: "duplicate"` instead of stacking.
    #[command(name = "export-view")]
    ExportView {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID the saved view belongs to.
        #[arg(long)]
        template_id: String,
        /// Destination folder node ID (defaults to workspace root). Max
        /// 64 chars.
        #[arg(long)]
        parent_node_id: Option<String>,
    },
}

// ─── Instructions ─────────────────────────────────────────────────────────────

/// AI instructions subcommands.
///
/// `content` is a markdown blob up to 65,536 raw bytes (multibyte chars
/// count for more than one). Setting an empty string is equivalent to
/// `clear`. Profile-wide writes (`set-org`, `set-workspace`, `set-share`)
/// require admin/owner privilege; the `*-user` variants write the
/// caller's own per-user override.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum InstructionsCommands {
    /// Get the calling user's self-scoped AI instructions.
    #[command(name = "get-user")]
    GetUser,
    /// Set the calling user's self-scoped AI instructions.
    #[command(name = "set-user")]
    SetUser {
        /// Markdown content (up to 65,536 raw bytes).
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Clear the calling user's self-scoped AI instructions.
    #[command(name = "clear-user")]
    ClearUser,

    /// Get the org-wide AI instructions.
    #[command(name = "get-org")]
    GetOrg {
        /// Org ID.
        #[arg(long)]
        org_id: String,
    },
    /// Set the org-wide AI instructions (owner / admin only).
    #[command(name = "set-org")]
    SetOrg {
        /// Org ID.
        #[arg(long)]
        org_id: String,
        /// Markdown content (up to 65,536 raw bytes).
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Clear the org-wide AI instructions (owner / admin only).
    #[command(name = "clear-org")]
    ClearOrg {
        /// Org ID.
        #[arg(long)]
        org_id: String,
    },
    /// Get the calling user's per-user override of an org's instructions.
    #[command(name = "get-org-user")]
    GetOrgUser {
        /// Org ID.
        #[arg(long)]
        org_id: String,
    },
    /// Set the calling user's per-user override of an org's instructions.
    #[command(name = "set-org-user")]
    SetOrgUser {
        /// Org ID.
        #[arg(long)]
        org_id: String,
        /// Markdown content (up to 65,536 raw bytes).
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Clear the calling user's per-user override of an org's instructions.
    #[command(name = "clear-org-user")]
    ClearOrgUser {
        /// Org ID.
        #[arg(long)]
        org_id: String,
    },

    /// Get the workspace-wide AI instructions.
    #[command(name = "get-workspace")]
    GetWorkspace {
        /// Workspace ID.
        #[arg(long)]
        workspace_id: String,
    },
    /// Set the workspace-wide AI instructions (owner / admin only).
    #[command(name = "set-workspace")]
    SetWorkspace {
        /// Workspace ID.
        #[arg(long)]
        workspace_id: String,
        /// Markdown content (up to 65,536 raw bytes).
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Clear the workspace-wide AI instructions (owner / admin only).
    #[command(name = "clear-workspace")]
    ClearWorkspace {
        /// Workspace ID.
        #[arg(long)]
        workspace_id: String,
    },
    /// Get the calling user's per-user override of a workspace's instructions.
    #[command(name = "get-workspace-user")]
    GetWorkspaceUser {
        /// Workspace ID.
        #[arg(long)]
        workspace_id: String,
    },
    /// Set the calling user's per-user override of a workspace's instructions.
    /// Blocked for guests.
    #[command(name = "set-workspace-user")]
    SetWorkspaceUser {
        /// Workspace ID.
        #[arg(long)]
        workspace_id: String,
        /// Markdown content (up to 65,536 raw bytes).
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Clear the calling user's per-user override of a workspace's instructions.
    #[command(name = "clear-workspace-user")]
    ClearWorkspaceUser {
        /// Workspace ID.
        #[arg(long)]
        workspace_id: String,
    },

    /// Get the share-wide AI instructions.
    #[command(name = "get-share")]
    GetShare {
        /// Share ID.
        #[arg(long)]
        share_id: String,
    },
    /// Set the share-wide AI instructions (owner / admin only).
    #[command(name = "set-share")]
    SetShare {
        /// Share ID.
        #[arg(long)]
        share_id: String,
        /// Markdown content (up to 65,536 raw bytes).
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Clear the share-wide AI instructions (owner / admin only).
    #[command(name = "clear-share")]
    ClearShare {
        /// Share ID.
        #[arg(long)]
        share_id: String,
    },
    /// Get the calling user's per-user override of a share's instructions.
    /// Registered share members only — anonymous/link guests blocked.
    #[command(name = "get-share-user")]
    GetShareUser {
        /// Share ID.
        #[arg(long)]
        share_id: String,
    },
    /// Set the calling user's per-user override of a share's instructions.
    /// Registered share members only.
    #[command(name = "set-share-user")]
    SetShareUser {
        /// Share ID.
        #[arg(long)]
        share_id: String,
        /// Markdown content (up to 65,536 raw bytes).
        #[arg(long, allow_hyphen_values = true)]
        content: String,
    },
    /// Clear the calling user's per-user override of a share's instructions.
    #[command(name = "clear-share-user")]
    ClearShareUser {
        /// Share ID.
        #[arg(long)]
        share_id: String,
    },
}

// ─── System ───────────────────────────────────────────────────────────────────

/// System health subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SystemCommands {
    /// Health check (no authentication required).
    Ping,
    /// System status (no authentication required).
    Status,
}

// ─── Manual Debug impls (redact sensitive fields) ────────────────────────────

impl fmt::Debug for Cli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cli")
            .field("format", &self.format)
            .field("fields", &self.fields)
            .field("detail", &self.detail)
            .field("no_color", &self.no_color)
            .field("quiet", &self.quiet)
            .field("verbose", &self.verbose)
            .field("profile", &self.profile)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("api_base", &self.api_base)
            .field("command", &self.command)
            .finish()
    }
}

impl fmt::Debug for AuthCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Login { email, password: _ } => f
                .debug_struct("Login")
                .field("email", email)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::Signup {
                email,
                password: _,
                first_name,
                last_name,
            } => f
                .debug_struct("Signup")
                .field("email", email)
                .field("password", &"[REDACTED]")
                .field("first_name", first_name)
                .field("last_name", last_name)
                .finish(),
            Self::PasswordReset {
                code,
                password1: _,
                password2: _,
            } => f
                .debug_struct("PasswordReset")
                .field("code", code)
                .field("password1", &"[REDACTED]")
                .field("password2", &"[REDACTED]")
                .finish(),
            Self::Logout => write!(f, "Logout"),
            Self::Status => write!(f, "Status"),
            Self::Verify { email, code } => f
                .debug_struct("Verify")
                .field("email", email)
                .field("code", code)
                .finish(),
            Self::TwoFa(cmds) => f.debug_tuple("TwoFa").field(cmds).finish(),
            Self::ApiKey(cmds) => f.debug_tuple("ApiKey").field(cmds).finish(),
            Self::Check => write!(f, "Check"),
            Self::Session => write!(f, "Session"),
            Self::EmailCheck { email } => {
                f.debug_struct("EmailCheck").field("email", email).finish()
            }
            Self::PasswordResetRequest { email } => f
                .debug_struct("PasswordResetRequest")
                .field("email", email)
                .finish(),
            Self::Oauth(cmds) => f.debug_tuple("Oauth").field(cmds).finish(),
            Self::Scopes => write!(f, "Scopes"),
            Self::PasswordResetCheck { code } => f
                .debug_struct("PasswordResetCheck")
                .field("code", code)
                .finish(),
            #[allow(unreachable_patterns)]
            _ => write!(f, "AuthCommands(<unknown variant>)"),
        }
    }
}

#[cfg(test)]
mod ripley_alias_tests {
    use super::{
        ApprovalCommands, Cli, Commands, OrgBillingCommands, OrgCommands, RipleyCommands,
        SearchCommands, TaskCommands, TodoCommands, WorklogCommands,
    };
    use clap::{CommandFactory, Parser};

    /// Clap's own internal invariant checker — catches duplicate/ambiguous
    /// aliases, bad arg combos, etc. at test time.
    #[test]
    fn cli_command_debug_assert() {
        Cli::command().debug_assert();
    }

    #[test]
    fn search_workspace_parses_with_bucket_flags() {
        let cli = Cli::try_parse_from([
            "fastio",
            "search",
            "workspace",
            "ws1",
            "quarterly report",
            "--files-limit",
            "10",
            "--comments-offset",
            "5",
            "--only",
            "files,comments",
        ])
        .expect("search workspace should parse");
        match cli.command {
            Commands::Search(SearchCommands::Workspace {
                workspace_id,
                query,
                files_limit,
                comments_offset,
                only,
                ..
            }) => {
                assert_eq!(workspace_id, "ws1");
                assert_eq!(query, "quarterly report");
                assert_eq!(files_limit, Some(10));
                assert_eq!(comments_offset, Some(5));
                assert_eq!(only.as_deref(), Some("files,comments"));
            }
            other => panic!("expected Search(Workspace), got {other:?}"),
        }
    }

    #[test]
    fn search_share_parses() {
        let cli = Cli::try_parse_from(["fastio", "search", "share", "sh1", "report"])
            .expect("search share should parse");
        match cli.command {
            Commands::Search(SearchCommands::Share {
                share_id, query, ..
            }) => {
                assert_eq!(share_id, "sh1");
                assert_eq!(query, "report");
            }
            other => panic!("expected Search(Share), got {other:?}"),
        }
    }

    #[test]
    fn view_parses_with_flags() {
        let cli = Cli::try_parse_from([
            "fastio",
            "view",
            "ws1",
            "n1",
            "--raw",
            "--version",
            "v3",
            "--no-pager",
        ])
        .expect("view should parse");
        match cli.command {
            Commands::View {
                workspace_id,
                node_id,
                raw,
                version,
                no_pager,
            } => {
                assert_eq!(workspace_id, "ws1");
                assert_eq!(node_id, "n1");
                assert!(raw);
                assert_eq!(version.as_deref(), Some("v3"));
                assert!(no_pager);
            }
            other => panic!("expected View, got {other:?}"),
        }
    }

    #[test]
    fn files_search_accepts_new_and_hidden_deprecated_flags() {
        // New flags parse; the hidden deprecated --page-size/--cursor are still
        // accepted (ignored at dispatch) so old scripts don't break.
        let cli = Cli::try_parse_from([
            "fastio",
            "files",
            "search",
            "--workspace",
            "ws1",
            "q",
            "--limit",
            "20",
            "--scope",
            "f1:v1",
            "--details",
            "--page-size",
            "100",
        ])
        .expect("files search should parse new + deprecated flags");
        match cli.command {
            Commands::Files(super::FilesCommands::Search {
                limit,
                scope,
                details,
                page_size,
                ..
            }) => {
                assert_eq!(limit, Some(20));
                assert_eq!(scope.as_deref(), Some("f1:v1"));
                assert!(details);
                assert_eq!(page_size, Some(100));
            }
            other => panic!("expected Files(Search), got {other:?}"),
        }
    }

    /// The canonical `ripley` group parses to the `Ripley` variant.
    #[test]
    fn ripley_chat_parses_to_ripley_variant() {
        let cli = Cli::try_parse_from(["fastio", "ripley", "chat", "--workspace", "ws1", "hi"])
            .expect("ripley chat should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Chat {
                workspace, message, ..
            }) => {
                assert_eq!(workspace, "ws1");
                assert_eq!(message, "hi");
            }
            other => panic!("expected Ripley(Chat), got {other:?}"),
        }
    }

    /// The hidden `ai` alias still parses to the same `Ripley` variant
    /// (back-compat is load-bearing per the resolved premise decision).
    #[test]
    fn ai_alias_parses_to_ripley_variant() {
        let cli = Cli::try_parse_from(["fastio", "ai", "chat", "--workspace", "ws2", "hello"])
            .expect("ai alias should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Chat {
                workspace, message, ..
            }) => {
                assert_eq!(workspace, "ws2");
                assert_eq!(message, "hello");
            }
            other => panic!("expected Ripley(Chat) via `ai` alias, got {other:?}"),
        }
    }

    /// `ripley` is visible in `--help`; the `ai` alias is hidden (clap
    /// `alias` is hidden by default — that is the desired behavior).
    #[test]
    fn ripley_visible_ai_hidden_in_help() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();
        assert!(help.contains("ripley"), "`ripley` should appear in help");
        // The hidden `ai` alias must NOT be advertised as its own listed
        // subcommand line. The token `ai` can appear inside prose
        // (descriptions), so assert it is not a standalone left-column entry.
        let listed_as_subcommand = help
            .lines()
            .any(|l| l.trim_start().starts_with("ai ") || l.trim_start() == "ai");
        assert!(
            !listed_as_subcommand,
            "hidden `ai` alias must not be listed as a subcommand in help"
        );
    }

    /// Legacy `--node-ids`/`--folder-id`/`--intelligence` flags are still
    /// accepted (hidden) on `ripley chat` so old invocations don't break.
    #[test]
    fn legacy_chat_flags_still_accepted() {
        let cli = Cli::try_parse_from([
            "fastio",
            "ripley",
            "chat",
            "--workspace",
            "ws",
            "--node-ids",
            "a,b",
            "--folder-id",
            "f1",
            "--intelligence",
            "true",
            "q",
        ])
        .expect("legacy flags should still parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Chat {
                node_ids,
                folder_id,
                intelligence,
                ..
            }) => {
                assert_eq!(
                    node_ids.as_deref(),
                    Some(&["a".to_owned(), "b".to_owned()][..])
                );
                assert_eq!(folder_id.as_deref(), Some("f1"));
                assert_eq!(intelligence, Some(true));
            }
            other => panic!("expected Ripley(Chat), got {other:?}"),
        }
    }

    /// The new visible `--files-scope` / `--folders-scope` / `--files-attach`
    /// flags parse and reach the `Chat` variant.
    #[test]
    fn new_scope_flags_parse_to_ripley_variant() {
        let cli = Cli::try_parse_from([
            "fastio",
            "ripley",
            "chat",
            "--workspace",
            "ws",
            "--files-scope",
            "n1:v1,n2:v2",
            "--folders-scope",
            "f1:5",
            "--files-attach",
            "a1:v1",
            "q",
        ])
        .expect("new scope flags should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Chat {
                files_scope,
                folders_scope,
                files_attach,
                ..
            }) => {
                assert_eq!(files_scope.as_deref(), Some("n1:v1,n2:v2"));
                assert_eq!(folders_scope.as_deref(), Some("f1:5"));
                assert_eq!(files_attach.as_deref(), Some("a1:v1"));
            }
            other => panic!("expected Ripley(Chat), got {other:?}"),
        }
    }

    // ── Phase 2 surface parse tests ──────────────────────────────────────

    use super::RipleyMemoryCommands;

    #[test]
    fn ask_parses_with_workspace_and_no_wait() {
        let cli = Cli::try_parse_from([
            "fastio",
            "ripley",
            "ask",
            "--workspace",
            "ws1",
            "--no-wait",
            "what is up?",
        ])
        .expect("ripley ask should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Ask {
                workspace,
                share,
                question,
                no_wait,
                ..
            }) => {
                assert_eq!(workspace.as_deref(), Some("ws1"));
                assert!(share.is_none());
                assert_eq!(question, "what is up?");
                assert!(no_wait);
            }
            other => panic!("expected Ripley(Ask), got {other:?}"),
        }
    }

    #[test]
    fn ask_workspace_and_share_conflict() {
        // --workspace and --share are mutually exclusive.
        let res = Cli::try_parse_from([
            "fastio",
            "ripley",
            "ask",
            "--workspace",
            "ws1",
            "--share",
            "s1",
            "q",
        ]);
        assert!(res.is_err(), "workspace + share must conflict");
    }

    #[test]
    fn ask_requires_workspace_or_share() {
        let res = Cli::try_parse_from(["fastio", "ripley", "ask", "q"]);
        assert!(res.is_err(), "ask must require --workspace or --share");
    }

    #[test]
    fn list_parses_kind_and_deleted() {
        let cli = Cli::try_parse_from([
            "fastio",
            "ripley",
            "list",
            "--share",
            "s1",
            "--kind",
            "agent",
            "--deleted",
        ])
        .expect("ripley list should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::List {
                share,
                kind,
                deleted,
                ..
            }) => {
                assert_eq!(share.as_deref(), Some("s1"));
                assert_eq!(kind.as_deref(), Some("agent"));
                assert!(deleted);
            }
            other => panic!("expected Ripley(List), got {other:?}"),
        }
    }

    #[test]
    fn list_rejects_bad_kind() {
        let res = Cli::try_parse_from([
            "fastio",
            "ripley",
            "list",
            "--workspace",
            "ws1",
            "--kind",
            "bogus",
        ]);
        assert!(res.is_err(), "invalid --kind must be rejected");
    }

    #[test]
    fn transactions_is_workspace_only() {
        let cli = Cli::try_parse_from(["fastio", "ripley", "transactions", "--workspace", "ws1"])
            .expect("transactions should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Transactions { workspace }) => {
                assert_eq!(workspace, "ws1");
            }
            other => panic!("expected Ripley(Transactions), got {other:?}"),
        }
        // No `--share` flag exists on transactions.
        let res = Cli::try_parse_from(["fastio", "ripley", "transactions", "--share", "s1"]);
        assert!(res.is_err(), "transactions must not accept --share");
    }

    #[test]
    fn autotitle_is_share_only() {
        let cli = Cli::try_parse_from(["fastio", "ripley", "autotitle", "--share", "s1"])
            .expect("autotitle should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Autotitle { share, .. }) => {
                assert_eq!(share, "s1");
            }
            other => panic!("expected Ripley(Autotitle), got {other:?}"),
        }
        let res = Cli::try_parse_from(["fastio", "ripley", "autotitle", "--workspace", "ws1"]);
        assert!(res.is_err(), "autotitle must not accept --workspace");
    }

    #[test]
    fn memory_set_parses_org_content_revision() {
        let cli = Cli::try_parse_from([
            "fastio",
            "ripley",
            "memory",
            "set",
            "--org",
            "o1",
            "--revision",
            "7",
            "hello",
        ])
        .expect("memory set should parse");
        match cli.command {
            Commands::Ripley(RipleyCommands::Memory(RipleyMemoryCommands::Set {
                org,
                workspace,
                content,
                revision,
            })) => {
                assert_eq!(org.as_deref(), Some("o1"));
                assert!(workspace.is_none());
                assert_eq!(content, "hello");
                assert_eq!(revision, Some(7));
            }
            other => panic!("expected Ripley(Memory(Set)), got {other:?}"),
        }
    }

    #[test]
    fn memory_get_requires_org_or_workspace() {
        let res = Cli::try_parse_from(["fastio", "ripley", "memory", "get"]);
        assert!(res.is_err(), "memory get must require --org or --workspace");
    }

    #[test]
    fn memory_org_and_workspace_conflict() {
        let res = Cli::try_parse_from([
            "fastio",
            "ripley",
            "memory",
            "get",
            "--org",
            "o1",
            "--workspace",
            "ws1",
        ]);
        assert!(res.is_err(), "memory --org + --workspace must conflict");
    }

    #[test]
    fn delegated_job_stubs_parse_but_are_hidden() {
        // The hidden stubs still parse (so the "pending" message can fire),
        // but must not be advertised in help.
        let cli = Cli::try_parse_from(["fastio", "ripley", "delegate", "do a thing"])
            .expect("delegate should parse");
        assert!(matches!(
            cli.command,
            Commands::Ripley(RipleyCommands::Delegate { .. })
        ));
        // `run` is a hidden alias of `delegate`.
        let cli = Cli::try_parse_from(["fastio", "ripley", "run", "do a thing"])
            .expect("run alias should parse");
        assert!(matches!(
            cli.command,
            Commands::Ripley(RipleyCommands::Delegate { .. })
        ));
        for verb in ["status", "logs", "cancel-job"] {
            let cli = Cli::try_parse_from(["fastio", "ripley", verb, "JOB123"])
                .unwrap_or_else(|e| panic!("`ripley {verb}` should parse: {e}"));
            assert!(matches!(cli.command, Commands::Ripley(_)));
        }
    }

    #[test]
    fn delegated_job_verbs_are_not_listed_in_ripley_help() {
        // Render the `ripley` subcommand's help and confirm the hidden
        // delegated-job verbs do not appear as listed subcommands.
        let mut cmd = Cli::command();
        let ripley = cmd
            .find_subcommand_mut("ripley")
            .expect("ripley subcommand present");
        let help = ripley.render_long_help().to_string();
        for hidden in ["delegate", "status", "logs", "cancel-job"] {
            let listed = help.lines().any(|l| {
                let t = l.trim_start();
                t == hidden || t.starts_with(&format!("{hidden} "))
            });
            assert!(
                !listed,
                "hidden delegated-job verb `{hidden}` must not be listed in help"
            );
        }
        // The headline `ask` and `memory` verbs ARE visible.
        assert!(
            help.contains("ask"),
            "`ask` should be visible in ripley help"
        );
        assert!(
            help.contains("memory"),
            "`memory` should be visible in ripley help"
        );
    }

    #[test]
    fn legacy_workflow_groups_marked_in_top_level_help() {
        // Each legacy primitive group's about line must carry `[legacy]`.
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        cmd.write_long_help(&mut buf).expect("render help");
        let help = String::from_utf8(buf).expect("utf8 help");
        for group in ["task", "worklog", "approval", "todo"] {
            // Find the subcommand and inspect its about text directly (robust to
            // help-layout wrapping).
            let sub = Cli::command()
                .get_subcommands()
                .find(|c| c.get_name() == group)
                .map(|c| {
                    c.get_about()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            assert!(
                sub.contains("[legacy]"),
                "{group} about must contain [legacy], got: {sub}"
            );
        }
        // Sanity: the rendered help is non-empty.
        assert!(!help.is_empty());
    }

    #[test]
    fn approval_request_parses_scope_and_optional_fields() {
        let cli = Cli::try_parse_from([
            "fastio",
            "approval",
            "request",
            "--profile-type",
            "share",
            "--profile-id",
            "sh1",
            "--entity-type",
            "task",
            "abc",
            "--description",
            "review please",
            "--approver-id",
            "appr1",
            "--deadline",
            "2025-06-15 23:59:59",
        ])
        .expect("approval request should parse");
        match cli.command {
            Commands::Approval(ApprovalCommands::Request {
                profile_type,
                profile_id,
                entity_type,
                entity_id,
                description,
                approver_id,
                deadline,
                ..
            }) => {
                assert_eq!(profile_type, "share");
                assert_eq!(profile_id, "sh1");
                assert_eq!(entity_type, "task");
                assert_eq!(entity_id, "abc");
                assert_eq!(description, "review please");
                assert_eq!(approver_id.as_deref(), Some("appr1"));
                assert_eq!(deadline.as_deref(), Some("2025-06-15 23:59:59"));
            }
            other => panic!("expected approval request, got {other:?}"),
        }
    }

    #[test]
    fn approval_approve_defaults_profile_type_to_workspace() {
        let cli = Cli::try_parse_from([
            "fastio",
            "approval",
            "approve",
            "--profile-id",
            "ws1",
            "appr1",
        ])
        .expect("approval approve should parse");
        match cli.command {
            Commands::Approval(ApprovalCommands::Approve {
                profile_type,
                profile_id,
                approval_id,
                ..
            }) => {
                assert_eq!(profile_type, "workspace");
                assert_eq!(profile_id.as_deref(), Some("ws1"));
                assert_eq!(approval_id, "appr1");
            }
            other => panic!("expected approval approve, got {other:?}"),
        }
    }

    #[test]
    fn approval_approve_works_without_scope() {
        // Backward compat: the historical `approval approve <id>` syntax must
        // parse without any scope flag (profile_id is then None → legacy route).
        let cli = Cli::try_parse_from(["fastio", "approval", "approve", "appr1"])
            .expect("approval approve should parse without a scope");
        match cli.command {
            Commands::Approval(ApprovalCommands::Approve {
                profile_id,
                approval_id,
                ..
            }) => {
                assert!(profile_id.is_none());
                assert_eq!(approval_id, "appr1");
            }
            other => panic!("expected approval approve, got {other:?}"),
        }
    }

    #[test]
    fn approval_mine_parses_filter() {
        let cli = Cli::try_parse_from(["fastio", "approval", "mine", "pending"])
            .expect("approval mine should parse");
        match cli.command {
            Commands::Approval(ApprovalCommands::Mine { filter, .. }) => {
                assert_eq!(filter, "pending");
            }
            other => panic!("expected approval mine, got {other:?}"),
        }
    }

    #[test]
    fn task_filter_and_summary_parse() {
        let cli =
            Cli::try_parse_from(["fastio", "task", "filter", "--workspace", "ws1", "assigned"])
                .expect("task filter should parse");
        assert!(matches!(
            cli.command,
            Commands::Task(TaskCommands::Filter { .. })
        ));
        let cli = Cli::try_parse_from(["fastio", "task", "summary", "--workspace", "ws1"])
            .expect("task summary should parse");
        assert!(matches!(
            cli.command,
            Commands::Task(TaskCommands::Summary { .. })
        ));
    }

    #[test]
    fn worklog_list_accepts_node_entity_type() {
        // workflow.txt lists `node` as a valid worklog entity type; the CLI
        // value_parser must accept it (previously rejected).
        let cli = Cli::try_parse_from([
            "fastio",
            "worklog",
            "list",
            "--workspace",
            "ws1",
            "--entity-type",
            "node",
            "--entity-id",
            "n1",
        ])
        .expect("worklog list --entity-type node should parse");
        match cli.command {
            Commands::Worklog(WorklogCommands::List { entity_type, .. }) => {
                assert_eq!(entity_type.as_deref(), Some("node"));
            }
            other => panic!("expected worklog list, got {other:?}"),
        }
    }

    #[test]
    fn worklog_filter_listall_summary_parse() {
        for (args, ok) in [
            (
                vec!["fastio", "worklog", "list-all", "--workspace", "ws1"],
                true,
            ),
            (
                vec![
                    "fastio",
                    "worklog",
                    "filter",
                    "--workspace",
                    "ws1",
                    "authored",
                ],
                true,
            ),
            (
                vec!["fastio", "worklog", "summary", "--workspace", "ws1"],
                true,
            ),
        ] {
            assert_eq!(
                Cli::try_parse_from(args.clone()).is_ok(),
                ok,
                "parsing {args:?}"
            );
        }
    }

    #[test]
    fn todo_filter_and_summary_parse() {
        let cli =
            Cli::try_parse_from(["fastio", "todo", "filter", "--workspace", "ws1", "pending"])
                .expect("todo filter should parse");
        assert!(matches!(
            cli.command,
            Commands::Todo(TodoCommands::Filter { .. })
        ));
        let cli = Cli::try_parse_from(["fastio", "todo", "summary", "--workspace", "ws1"])
            .expect("todo summary should parse");
        assert!(matches!(
            cli.command,
            Commands::Todo(TodoCommands::Summary { .. })
        ));
    }

    // ── Phase 7 billing parse tests ──────────────────────────────────────

    #[test]
    fn billing_subscribe_accepts_plan_and_legacy_plan_id() {
        // Both the canonical --plan and the legacy --plan-id alias parse to the
        // same value (one-release back-compat for `org billing create`).
        for flag in ["--plan", "--plan-id"] {
            let cli = Cli::try_parse_from([
                "fastio",
                "org",
                "billing",
                "subscribe",
                "org123",
                flag,
                "business_v2_monthly",
            ])
            .unwrap_or_else(|e| panic!("billing subscribe {flag} should parse: {e}"));
            match cli.command {
                Commands::Org(OrgCommands::Billing(OrgBillingCommands::Subscribe {
                    org_id,
                    plan,
                })) => {
                    assert_eq!(org_id, "org123");
                    assert_eq!(plan, "business_v2_monthly", "via {flag}");
                }
                other => panic!("expected Org Billing Subscribe via {flag}, got {other:?}"),
            }
        }
    }

    #[test]
    fn billing_create_alias_with_legacy_plan_id_parses() {
        // The hidden `create` alias + legacy `--plan-id` together (the exact
        // pre-retool invocation) must still parse.
        let cli = Cli::try_parse_from([
            "fastio",
            "org",
            "billing",
            "create",
            "org123",
            "--plan-id",
            "solo_monthly",
        ])
        .expect("`billing create --plan-id` should still parse");
        match cli.command {
            Commands::Org(OrgCommands::Billing(OrgBillingCommands::Subscribe { org_id, plan })) => {
                assert_eq!(org_id, "org123");
                assert_eq!(plan, "solo_monthly");
            }
            other => panic!("expected Org Billing Subscribe, got {other:?}"),
        }
    }

    #[test]
    fn top_level_org_limits_still_routes_when_hidden() {
        // `org limits` is hidden from help but must still parse/route for
        // one-release back-compat.
        let cli = Cli::try_parse_from(["fastio", "org", "limits", "org123"])
            .expect("top-level `org limits` should still parse");
        match cli.command {
            Commands::Org(OrgCommands::Limits { org_id }) => assert_eq!(org_id, "org123"),
            other => panic!("expected Org Limits, got {other:?}"),
        }
    }
}
