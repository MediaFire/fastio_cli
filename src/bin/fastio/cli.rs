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
//     short form alongside the canonical one (e.g. `how-to` / `howto`).
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

/// The named size presets accepted by `preview transform --size`
/// (case-insensitive; matches the server's `ImageNamedTranformations`).
const PREVIEW_SIZE_PRESETS: &[&str] = &["IconTiny", "IconSmall", "IconMedium", "Preview"];

/// Validate `preview transform --size` against the named presets,
/// case-insensitively (the server matches `strtolower`-to-`strtolower`), and
/// pass the user's value through unchanged. Defense-in-depth mirroring the
/// api-layer `validate_transform_params`.
fn parse_preview_size(value: &str) -> Result<String, String> {
    if PREVIEW_SIZE_PRESETS
        .iter()
        .any(|p| p.eq_ignore_ascii_case(value))
    {
        Ok(value.to_owned())
    } else {
        Err(format!(
            "invalid size '{value}' (valid: IconTiny, IconSmall, IconMedium, Preview)"
        ))
    }
}

/// Validate `preview transform --rotate` against the allowed rotations
/// {0, 90, 180, 270}. Defense-in-depth mirroring the api-layer validation.
fn parse_preview_rotate(value: &str) -> Result<u32, String> {
    let n: u32 = value
        .parse()
        .map_err(|_| format!("invalid rotate '{value}' (valid: 0, 90, 180, 270)"))?;
    if matches!(n, 0 | 90 | 180 | 270) {
        Ok(n)
    } else {
        Err(format!("invalid rotate '{n}' (valid: 0, 90, 180, 270)"))
    }
}

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
///
/// Some subcommand groups (e.g. `Share`, with its full create/update settings
/// surface) are large; boxing a clap subcommand payload is non-idiomatic, and
/// the top-level command is parsed once, so the size difference is immaterial.
#[allow(clippy::large_enum_variant)]
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
    /// Per-workspace dashboard: the calling member's ranked, actionable card
    /// feed (@mentions, file activity, file versions, synthesis; signature cards
    /// only when E-Sign is enabled platform-side). Dismiss / snooze / undismiss
    /// are per-member and out-of-band — they only hide a card from your own feed,
    /// never resolving the underlying card subject.
    #[command(subcommand)]
    Dashboard(DashboardCommands),
    /// File previews.
    #[command(subcommand)]
    Preview(PreviewCommands),
    /// Organization and workspace assets.
    #[command(subcommand)]
    Asset(AssetCommands),
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

    /// Ask a grounded "how do I…" question about Fast.io and get a
    /// product-aware answer (or a short clarifying question) back in one call.
    ///
    /// Org-less and open to any authenticated user — answers are generated over
    /// Fast.io's own how-to knowledge, so you get usage guidance without
    /// scraping the docs. For Q&A over your OWN files, use `ripley ask`
    /// instead; how-to answers questions about Fast.io itself.
    #[command(visible_alias = "howto")]
    HowTo {
        /// The natural-language question (1–2000 characters, non-blank).
        question: String,
        /// Phrase the answer for a specific client: `mcp` (Fast.io MCP
        /// consolidated tools) or `code` (execute-proxy calls for a code-mode
        /// agent). Omit for the default REST-API phrasing.
        #[arg(long, value_parser = ["mcp", "code"])]
        surface: Option<String>,
        /// Optional free-text background about your situation (what you are
        /// trying to accomplish, what you have tried). Up to 8000 characters;
        /// treated strictly as data, never as instructions.
        #[arg(long)]
        context: Option<String>,
    },

    /// Metadata extraction and template management.
    #[command(subcommand)]
    Metadata(MetadataCommands),

    /// E-signature: draft, send, void, and download `SignEnvelopes` (PDFs sent
    /// to recipients for electronic signature). Every envelope is parented to a
    /// workspace (each subcommand takes a required `--workspace <id>`). Signing
    /// is a paid-plan feature.
    ///
    /// Disabled by default (feature sunset 2026-07): the runtime kill-switch in
    /// `main.rs` blocks execution unless `FASTIO_ENABLE_ESIGN=1`, and `hide =
    /// true` keeps the surface out of top-level `--help`. The env var does not
    /// un-hide the entry (hide is static); only execution is gated.
    #[command(subcommand, hide = true)]
    Sign(SignCommands),

    /// File Shares: durable, link-shareable views of a single workspace file
    /// (the successor to the retired `QuickShare`). Create / manage shares and
    /// grants, read or write the bound file, and mint realtime tokens. Read
    /// commands (info / download / versions / preview) can run anonymously when
    /// the share's access tier allows it.
    // No `fs` alias: it drifts in scope and collides with user expectations for
    // `files` (a `fs` shorthand reads as "file system" / "files"). If a product
    // decision later wants it, it can return as a documented alias.
    #[command(subcommand)]
    Fileshare(FileshareCommands),

    /// System health and status checks (no auth required).
    #[command(subcommand)]
    System(SystemCommands),

    /// Inspect Fast.io identifiers offline (no auth, no network).
    ///
    /// Classifies an `OpaqueId` by its self-describing length and type prefix
    /// (29-char = 1-char type; 30-char = 2-char type — the workflow family
    /// under `w` plus the non-workflow Task/Comment types), mapping it to its
    /// entity type and surfacing tier. Useful when an id arrives in a webhook,
    /// event, or payload and you need to know what it refers to before acting
    /// on it.
    #[command(subcommand)]
    Id(IdCommands),

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
/// comments for a workspace; files + comments for a share). Each
/// bucket paginates independently via its own `--<bucket>-limit/offset`.
/// `--only` filters which buckets are *displayed* client-side — the server
/// always searches every applicable bucket (there is no server `only`
/// parameter), so it does not reduce server work.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SearchCommands {
    /// Search everything in a workspace (files + metadata + comments).
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

// ─── Sign (E-Signature) ────────────────────────────────────────────────────────

/// E-signature subcommands (`fastio sign`).
///
/// `SignEnvelopes` are parented to a Workspace; every subcommand takes a
/// required `--workspace <id>` flag. Drafts are created and edited via these
/// commands, then `send` emails real recipients. Signing is a paid-plan feature
/// (a non-entitled org returns `1670`; access also requires workspace
/// membership).
// Justification: the envelope-lifecycle variant carries the create/update
// flag set and is larger than the download variants. This is a clap subcommand
// enum constructed once at parse time and immediately dispatched (never stored
// in bulk or passed by value in a hot path), so the size difference is
// immaterial; boxing a clap subcommand payload is non-idiomatic here.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SignCommands {
    /// Envelope lifecycle (create / list / get / update / send / void).
    #[command(subcommand)]
    Envelope(SignEnvelopeCommands),
    /// Reusable signing-template blueprints (create / list / get / update /
    /// delete / instantiate).
    #[command(subcommand)]
    Template(SignTemplateCommands),
    /// Document byte downloads (source PDF, preview, signed PDF).
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
    ///
    /// The response is the flat envelope (no inlined documents / recipients /
    /// fields; `provider` is null until sent). Run `sign envelope get <id>` to
    /// read the server-generated document/recipient/field ids.
    Create {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
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
    /// List envelopes for the workspace (offset-paginated, newest first).
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Lifecycle status filter: a single status or a CSV of
        /// `draft,sent,in_progress,completed,declined,expired,voided,failed`.
        #[arg(long)]
        status: Option<String>,
        /// Only envelopes created after this time (format `Y-m-d H:i:s UTC`).
        #[arg(long)]
        created_after: Option<String>,
        /// Only envelopes created before this time (format `Y-m-d H:i:s UTC`).
        #[arg(long)]
        created_before: Option<String>,
        /// Pagination limit.
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination offset.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Get a single envelope (documents/recipients/fields inlined).
    Get {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
    },
    /// Update mutable fields on a DRAFT envelope (a non-draft returns 403).
    ///
    /// An update is a FULL recipient replacement — `--recipients-json` (≥1) is
    /// REQUIRED. `--fields-json` is a full replacement; `--documents-json` is a
    /// declarative replacement (omit to leave the document set unchanged). Each
    /// accepts `@file.json`.
    ///
    /// DECLARATIVE — `--expires-at` and `--policy-json` are rewritten on every
    /// update: OMITTING one CLEARS it (resets to null). Re-send the current value
    /// (from `sign envelope get`) to keep it. `--name` / `--documents-json` /
    /// `--fields-json` are preserved when omitted.
    Update {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
        /// New display name. Omit to keep the current name; a name cannot be cleared via update.
        #[arg(long)]
        name: Option<String>,
        /// New UTC expiry timestamp. DECLARATIVE: omitting CLEARS the expiry
        /// (resets to null) — re-send the current value to keep it.
        #[arg(long)]
        expires_at: Option<String>,
        /// New policy bag as a JSON object (or `@file.json`). DECLARATIVE:
        /// omitting CLEARS the policy (resets to null) — re-send to keep it.
        #[arg(long)]
        policy_json: Option<String>,
        /// Declarative document replacement as a JSON array (or `@file.json`).
        #[arg(long)]
        documents_json: Option<String>,
        /// Full recipient replacement as a JSON array (or `@file.json`).
        /// REQUIRED — an update always replaces the recipient roster (≥1).
        #[arg(long)]
        recipients_json: Option<String>,
        /// Full field replacement as a JSON array (or `@file.json`).
        #[arg(long)]
        fields_json: Option<String>,
    },
    /// Send a draft envelope (draft → sent). EMAILS REAL RECIPIENTS; idempotent.
    Send {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
        /// Skip the interactive confirmation prompt (send notifies recipients).
        #[arg(long)]
        yes: bool,
    },
    /// Void a non-terminal envelope (cascades to Voided). Credits NOT refunded.
    Void {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
        /// Reason for voiding (REQUIRED, max 1024 bytes).
        #[arg(long)]
        reason: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Re-drive a STUCK envelope through self-healing recovery (admin).
    ///
    /// Idempotent with no-op success — re-driving a non-stuck or already-terminal
    /// envelope succeeds without side effects. A permanent signing-pipeline
    /// failure cascades the envelope to the terminal Failed state. Takes no body
    /// and notifies no one, so no confirmation is required.
    Retry {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
    },
    /// Mint YOUR (the calling member's) signing link for an envelope.
    ///
    /// The primary action for a dashboard `signature` card — the `envelope_id`
    /// is the card's `target.id`. The response is structured: `sign_url` is
    /// non-null only when you can sign now; `is_terminal` means the envelope is
    /// completed/void/declined; `reauth_required` means re-authenticate first;
    /// otherwise you are blocked by routing order (see `blocked_signers`).
    /// Requires a write-scope token (a read-only token is rejected).
    MySignLink {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID (from a signature card's `target.id`).
        envelope_id: String,
    },
}

/// `SignEnvelope` document-download subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SignDocumentCommands {
    /// Download a document's SOURCE PDF (the file uploaded at create time).
    Download {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
        /// Document ID.
        document_id: String,
        /// Output file path.
        #[arg(long, short)]
        output: String,
    },
    /// Preview a document's SOURCE PDF (same bytes as `download`, served for
    /// in-app rendering).
    Preview {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
        /// Document ID.
        document_id: String,
        /// Output file path.
        #[arg(long, short)]
        output: String,
    },
    /// Download a document's SIGNED PDF (not ready until the envelope completes).
    #[command(name = "signed-download")]
    SignedDownload {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
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
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Envelope ID.
        envelope_id: String,
        /// Output file path.
        #[arg(long, short)]
        output: String,
    },
}

/// Signing-template (`fastio sign template`) subcommands.
///
/// A `SignTemplate` is a workspace-parented, reusable envelope blueprint (template
/// id `sa…`). Bodies are JSON; the `--snapshot` / `--recipient-bindings` /
/// `--documents` arguments accept inline JSON or an `@file.json` path. `update`
/// is optimistic-CAS (`--expected-version` is required); `delete` is a reversible
/// soft-delete; `instantiate` creates a DRAFT envelope.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum SignTemplateCommands {
    /// Create a signing template from a snapshot blueprint.
    Create {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Display name (required, max 255 chars).
        #[arg(long)]
        name: String,
        /// Optional description (max 1024 chars).
        #[arg(long)]
        description: Option<String>,
        /// Snapshot blueprint as a JSON OBJECT (or `@file.json`) — the
        /// `recipient_slots` / `document_slots` / `fields` / `policy` bag.
        /// Passed through verbatim; the server validates its internal shape.
        #[arg(long)]
        snapshot: String,
    },
    /// List signing templates for the workspace (offset-paginated).
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Pagination offset (default 0).
        #[arg(long)]
        offset: Option<u32>,
        /// Pagination limit (default 50, max 200).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Get a single signing template.
    Get {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID (`sa…` `OpaqueId`).
        template_id: String,
    },
    /// Update a signing template (optimistic-CAS via `--expected-version`).
    Update {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID (`sa…` `OpaqueId`).
        template_id: String,
        /// REQUIRED expected current version (≥1). A stale value is rejected
        /// server-side as a version conflict (409 / 147321).
        #[arg(long)]
        expected_version: u64,
        /// New display name (max 255 chars). Omit to leave unchanged.
        #[arg(long)]
        name: Option<String>,
        /// New description (max 1024 chars). Omit to leave unchanged.
        #[arg(long)]
        description: Option<String>,
        /// New snapshot blueprint as a JSON OBJECT (or `@file.json`). When
        /// present this is a FULL replacement of the blueprint; omit to leave
        /// the snapshot unchanged.
        #[arg(long)]
        snapshot: Option<String>,
    },
    /// Soft-delete a signing template (reversible; never blocked by referrers).
    Delete {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID (`sa…` `OpaqueId`).
        template_id: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Instantiate a template into a fresh DRAFT envelope (reversible).
    Instantiate {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Template ID (`sa…` `OpaqueId`).
        template_id: String,
        /// REQUIRED recipient bindings as a JSON OBJECT/map (or `@file.json`)
        /// keyed by `slot_key` → `{email, display_name?, auth_method?}`. An
        /// array is rejected.
        #[arg(long)]
        recipient_bindings: String,
        /// Optional document bindings as a JSON ARRAY (or `@file.json`) of
        /// `{document_slot_index, source_node_id, source_version_id?}`.
        #[arg(long)]
        documents: Option<String>,
        /// Optional name override for the created envelope.
        #[arg(long)]
        envelope_name: Option<String>,
    },
}

// ─── File Shares ───────────────────────────────────────────────────────────────

/// File Share subcommands (`fastio fileshare`).
///
/// A File Share is a durable, link-shareable view of one workspace file. The
/// management surface (create / list / update / delete / grants / upload /
/// ws-token / activity) requires authentication; the consumption surface (info /
/// download / versions / preview) can run anonymously when the share's access
/// tier permits, or with an optional link password.
///
/// NOTE: `Debug` is implemented MANUALLY (not derived) so the `--password`
/// values never appear in a debug rendering — see the `impl fmt::Debug` below.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
#[non_exhaustive]
pub enum FileshareCommands {
    /// Create a File Share bound to a workspace file node (the binding is
    /// immutable). Requires workspace membership.
    Create {
        /// Workspace ID that owns the file.
        #[arg(long)]
        workspace: String,
        /// `OpaqueId` of the file node to share (must be a file, not a folder).
        #[arg(long)]
        node: String,
        /// Optional display title (max 255 chars).
        #[arg(long)]
        title: Option<String>,
        /// Access tier. Defaults to `named_people` server-side.
        #[arg(long, value_parser = ["anyone_with_link", "any_registered", "named_people"])]
        access_option: Option<String>,
        /// Optional link password (1-255 chars). WARNING: a value passed on the
        /// command line is visible in `ps` and your shell history. Prefer the
        /// `FASTIO_FILESHARE_PASSWORD` environment variable, which this command
        /// reads when `--password` is omitted. (A future `--password-file` may
        /// be added.)
        #[arg(long)]
        password: Option<String>,
        /// Relative expiry in seconds from now (1..=3155760000). Mutually
        /// exclusive with `--expires-at`. Omitted = durable (never expires).
        #[arg(long, conflicts_with = "expires_at")]
        expires: Option<u64>,
        /// Absolute expiry datetime (a value without a timezone is UTC).
        /// Mutually exclusive with `--expires`.
        #[arg(long)]
        expires_at: Option<String>,
    },
    /// List a workspace's File Shares (offset-paginated). Requires membership.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Result offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
        /// Maximum number of results to return.
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Show a File Share's public viewer details, including the caller's
    /// `effective_capability`. Can run anonymously (tier-dependent); supply
    /// `--password` for a password-protected link.
    Info {
        /// File Share ID.
        fileshare_id: String,
        /// Link password (see `create --password` for the `ps`/history warning;
        /// `FASTIO_FILESHARE_PASSWORD` is read when this is omitted).
        #[arg(long)]
        password: Option<String>,
    },
    /// Update a File Share's mutable settings (title / access / password /
    /// expiry). Requires membership. Supply at least one change.
    Update {
        /// File Share ID.
        fileshare_id: String,
        /// New display title (max 255). A title cannot be cleared.
        #[arg(long)]
        title: Option<String>,
        /// New access tier.
        #[arg(long, value_parser = ["anyone_with_link", "any_registered", "named_people"])]
        access_option: Option<String>,
        /// New link password. WARNING: visible in `ps`/shell history — prefer
        /// `FASTIO_FILESHARE_PASSWORD` (read when omitted). Mutually exclusive
        /// with `--clear-password`.
        #[arg(long, conflicts_with = "clear_password")]
        password: Option<String>,
        /// Remove the link password (the share becomes unprotected). Mutually
        /// exclusive with `--password`.
        #[arg(long)]
        clear_password: bool,
        /// New relative expiry (seconds from now). Mutually exclusive with
        /// `--expires-at` / `--clear-expires`.
        #[arg(long, conflicts_with_all = ["expires_at", "clear_expires"])]
        expires: Option<u64>,
        /// New absolute expiry datetime. Mutually exclusive with `--expires` /
        /// `--clear-expires`.
        #[arg(long, conflicts_with = "clear_expires")]
        expires_at: Option<String>,
        /// Remove the expiry (the share becomes durable again).
        #[arg(long)]
        clear_expires: bool,
    },
    /// Delete a File Share (revokes the link, cascades its grants; the bound
    /// file is never touched). Requires membership.
    Delete {
        /// File Share ID.
        fileshare_id: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Manage named-people grants on a File Share.
    #[command(subcommand)]
    Grants(FileshareGrantsCommands),
    /// Download the bound file (or a historical version) to disk. Can run
    /// anonymously (tier-dependent); supply `--password` for a protected link.
    Download {
        /// File Share ID.
        fileshare_id: String,
        /// Output file path. Defaults to the bound file's name.
        #[arg(long, short)]
        output: Option<String>,
        /// Download a specific historical version by its version id (instead of
        /// the current bound file). NOTE: when `--output` is omitted the default
        /// filename still derives from the bound file's CURRENT name, not the
        /// historical version's name — pass `--output` to control it.
        #[arg(long)]
        version: Option<String>,
        /// Link password (visible in `ps`/history — prefer
        /// `FASTIO_FILESHARE_PASSWORD`, read when omitted).
        #[arg(long)]
        password: Option<String>,
    },
    /// List the bound file's versions. Can run anonymously (tier-dependent).
    Versions {
        /// File Share ID.
        fileshare_id: String,
        /// Link password (read from `FASTIO_FILESHARE_PASSWORD` when omitted).
        #[arg(long)]
        password: Option<String>,
    },
    /// Download a generated preview asset for the bound file. Downloads the
    /// PRIMARY preview asset only (after at most one redirect); multi-file
    /// previews (HLS playlists, paged documents) yield the primary asset —
    /// sub-assets are NOT fetched. Can run anonymously (tier-dependent).
    Preview {
        /// File Share ID.
        fileshare_id: String,
        /// Preview type to fetch (e.g. `thumbnail`, `image`, `pdf`, `mp4`,
        /// `hls_stream`). Passed through to the server verbatim.
        #[arg(long = "type")]
        preview_type: String,
        /// Output file path. Defaults to `<fileshare-id>.<type>` (a preview is a
        /// DERIVED asset, so the bound file's name is not used).
        #[arg(long, short)]
        output: Option<String>,
        /// Link password (read from `FASTIO_FILESHARE_PASSWORD` when omitted).
        #[arg(long)]
        password: Option<String>,
    },
    /// Replace the bound file's content with a local file (write-back). Requires
    /// an `edit` grant on the File Share (workspace membership is not required).
    Upload {
        /// File Share ID.
        fileshare_id: String,
        /// Path to the local file whose content replaces the bound file.
        file: String,
        /// Compare-and-swap precondition (server-enforced): the bound file's
        /// current version id, sent so the server can reject the replace on a
        /// version conflict. When the server detects a mismatch it reports
        /// `CONFLICT_VERSION_MISMATCH` and the command surfaces it as a
        /// version-conflict error carrying the current version id.
        #[arg(long)]
        if_version: Option<String>,
        /// Link password (visible in `ps`/history — prefer
        /// `FASTIO_FILESHARE_PASSWORD`, read when omitted).
        #[arg(long)]
        password: Option<String>,
        /// Override the uploaded file name (defaults to the local file's name).
        #[arg(long)]
        name: Option<String>,
        /// Skip the confirmation prompt (the write creates a new version).
        #[arg(long)]
        yes: bool,
    },
    /// Long-poll for activity on a File Share (workspace members only). Mirrors
    /// `fastio event poll`.
    Activity {
        /// File Share ID.
        fileshare_id: String,
        /// Last activity timestamp for incremental polling.
        #[arg(long)]
        lastactivity: Option<String>,
        /// Max seconds the server will hold the connection (1-95).
        #[arg(long)]
        wait: Option<u32>,
        /// Return only events newer than `--lastactivity`.
        #[arg(long)]
        updated: bool,
    },
    /// Mint a short-lived realtime-channel WebSocket token for a File Share
    /// (workspace members only). The token is REDACTED from stdout; pass
    /// `--token-file` to capture it (written 0600).
    WsToken {
        /// File Share ID.
        fileshare_id: String,
        /// Write the minted token to this path (created 0600). When omitted the
        /// token is redacted from output and a warning is printed.
        #[arg(long)]
        token_file: Option<std::path::PathBuf>,
    },
}

/// File Share grant subcommands.
///
/// `Debug` is implemented manually on [`FileshareCommands`] (these variants
/// carry no secrets, but they are reached through that manual impl).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum FileshareGrantsCommands {
    /// List a File Share's named-people grants (no pagination; first 1000).
    List {
        /// File Share ID.
        fileshare_id: String,
    },
    /// Grant (or raise) a user's capability on a File Share. Supply exactly one
    /// of `--user` or `--email`.
    Add {
        /// File Share ID.
        fileshare_id: String,
        /// Grantee's 19-digit user profile id. Mutually exclusive with
        /// `--email`.
        #[arg(long, conflicts_with = "email")]
        user: Option<String>,
        /// Grantee's email address. An unregistered email becomes a pending
        /// invitation. Mutually exclusive with `--user`.
        #[arg(long)]
        email: Option<String>,
        /// Capability to grant.
        #[arg(long, value_parser = ["view", "download", "edit"])]
        capability: String,
    },
    /// Revoke a user's grant on a File Share (idempotent). Supply exactly one of
    /// `--user` or `--email`.
    Remove {
        /// File Share ID.
        fileshare_id: String,
        /// Grantee's 19-digit user profile id. Mutually exclusive with
        /// `--email`.
        #[arg(long, conflicts_with = "email")]
        user: Option<String>,
        /// Grantee's email address. Mutually exclusive with `--user`.
        #[arg(long)]
        email: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

/// Manual `Debug` for [`FileshareCommands`] that REDACTS every `--password`
/// value so a secret can never leak into a debug rendering (logs, panics).
///
/// `#[derive(Debug)]` would print the `Option<String>` password verbatim. Each
/// variant is rendered field-by-field with the `password` field replaced by a
/// fixed `Some(<redacted>)` / `None` marker; all other fields are shown as-is.
impl fmt::Debug for FileshareCommands {
    #[allow(clippy::too_many_lines)] // a flat field-by-field render over every variant
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render an Option<password> as a redacted marker, preserving only
        // whether a value was present.
        fn pw(p: Option<&String>) -> &'static str {
            match p {
                Some(_) => "Some(<redacted>)",
                None => "None",
            }
        }
        match self {
            Self::Create {
                workspace,
                node,
                title,
                access_option,
                password,
                expires,
                expires_at,
            } => f
                .debug_struct("Create")
                .field("workspace", workspace)
                .field("node", node)
                .field("title", title)
                .field("access_option", access_option)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .field("expires", expires)
                .field("expires_at", expires_at)
                .finish(),
            Self::List {
                workspace,
                offset,
                limit,
            } => f
                .debug_struct("List")
                .field("workspace", workspace)
                .field("offset", offset)
                .field("limit", limit)
                .finish(),
            Self::Info {
                fileshare_id,
                password,
            } => f
                .debug_struct("Info")
                .field("fileshare_id", fileshare_id)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .finish(),
            Self::Update {
                fileshare_id,
                title,
                access_option,
                password,
                clear_password,
                expires,
                expires_at,
                clear_expires,
            } => f
                .debug_struct("Update")
                .field("fileshare_id", fileshare_id)
                .field("title", title)
                .field("access_option", access_option)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .field("clear_password", clear_password)
                .field("expires", expires)
                .field("expires_at", expires_at)
                .field("clear_expires", clear_expires)
                .finish(),
            Self::Delete { fileshare_id, yes } => f
                .debug_struct("Delete")
                .field("fileshare_id", fileshare_id)
                .field("yes", yes)
                .finish(),
            Self::Grants(c) => f.debug_tuple("Grants").field(c).finish(),
            Self::Download {
                fileshare_id,
                output,
                version,
                password,
            } => f
                .debug_struct("Download")
                .field("fileshare_id", fileshare_id)
                .field("output", output)
                .field("version", version)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .finish(),
            Self::Versions {
                fileshare_id,
                password,
            } => f
                .debug_struct("Versions")
                .field("fileshare_id", fileshare_id)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .finish(),
            Self::Preview {
                fileshare_id,
                preview_type,
                output,
                password,
            } => f
                .debug_struct("Preview")
                .field("fileshare_id", fileshare_id)
                .field("preview_type", preview_type)
                .field("output", output)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .finish(),
            Self::Upload {
                fileshare_id,
                file,
                if_version,
                password,
                name,
                yes,
            } => f
                .debug_struct("Upload")
                .field("fileshare_id", fileshare_id)
                .field("file", file)
                .field("if_version", if_version)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .field("name", name)
                .field("yes", yes)
                .finish(),
            Self::Activity {
                fileshare_id,
                lastactivity,
                wait,
                updated,
            } => f
                .debug_struct("Activity")
                .field("fileshare_id", fileshare_id)
                .field("lastactivity", lastactivity)
                .field("wait", wait)
                .field("updated", updated)
                .finish(),
            Self::WsToken {
                fileshare_id,
                token_file,
            } => f
                .debug_struct("WsToken")
                .field("fileshare_id", fileshare_id)
                .field("token_file", token_file)
                .finish(),
        }
    }
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
    /// Clear stored credentials for the current profile (local only).
    Logout,
    /// Sign out server-side: invalidate every revocable (browser) session token,
    /// then clear local credentials.
    Signout,
    /// Invalidate ALL of your login sessions everywhere (strict superset of
    /// sign-out), then clear local credentials.
    #[command(name = "invalidate-all")]
    InvalidateAll,
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
        /// Create an AI-agent account (sets `account_type` to "agent"
        /// permanently).
        #[arg(long)]
        agent: bool,
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
///
/// `Debug` is implemented manually (see `impl fmt::Debug for TwoFaCommands`) so
/// the one-time `token` / `code` auth secrets carried by `Disable`, `Verify`,
/// and `VerifySetup` are redacted — including through the `AuthCommands` Debug
/// tree, which delegates to this impl.
#[derive(Subcommand)]
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
        /// Agent or application name for tracking (max 128 chars).
        #[arg(long = "agent-name")]
        agent_name: Option<String>,
        /// Expiration datetime (strtotime-compatible, e.g.
        /// "2026-12-31 23:59:59 UTC"); must be in the future. Omit for no
        /// expiration.
        #[arg(long)]
        expires: Option<String>,
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
        /// New agent or application name (max 128 chars; empty string clears).
        #[arg(long = "agent-name")]
        agent_name: Option<String>,
        /// New expiration datetime (strtotime-compatible); empty string clears
        /// an existing expiration.
        #[arg(long)]
        expires: Option<String>,
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
    /// Rename a session's display labels (device name and/or agent name).
    Rename {
        /// Session ID.
        session_id: String,
        /// New device name (max 128 chars; empty string clears to null).
        #[arg(long = "device-name")]
        device_name: Option<String>,
        /// New agent name (max 128 chars; empty string clears to null).
        #[arg(long = "agent-name")]
        agent_name: Option<String>,
    },
    /// Revoke a single session.
    Revoke {
        /// Session ID.
        session_id: String,
    },
    /// Revoke all sessions.
    #[command(name = "revoke-all")]
    RevokeAll {
        /// Keep this session active while revoking all others (pass the session
        /// ID to preserve, e.g. your current session).
        #[arg(long = "exclude-current")]
        exclude_current: Option<String>,
    },
}

// ─── User ────────────────────────────────────────────────────────────────────

/// User subcommands.
///
/// `Debug` is implemented manually (not derived) so the password-change
/// `password` / `current_password` fields on `Update` are never rendered
/// verbatim through the `Cli` Debug tree (CLAUDE.md coding-standard #9).
#[derive(Subcommand)]
#[non_exhaustive]
pub enum UserCommands {
    /// Get current user profile.
    Info,
    /// Update user profile. Supports name, phone, and password changes.
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
        /// Numeric phone country code, e.g. "1" for US (requires 2FA disabled;
        /// send with --phone-number).
        #[arg(long = "phone-country")]
        phone_country: Option<String>,
        /// Numeric phone number (requires 2FA disabled; send with
        /// --phone-country).
        #[arg(long = "phone-number")]
        phone_number: Option<String>,
        /// New password (requires --current-password if the account already
        /// has one).
        #[arg(long)]
        password: Option<String>,
        /// Current password, required to change the password.
        #[arg(long = "current-password")]
        current_password: Option<String>,
    },
    /// Change the account email address (request + confirm).
    #[command(subcommand, name = "email-change")]
    EmailChange(UserEmailChangeCommands),
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

/// Email-change subcommands.
///
/// `Debug` is implemented manually (not derived) so the `current_password`
/// proof and the one-time confirmation `token` are never rendered verbatim
/// through the `Cli` Debug tree (CLAUDE.md coding-standard #9).
#[derive(Subcommand)]
#[non_exhaustive]
pub enum UserEmailChangeCommands {
    /// Request an email-address change. Sends a confirmation link to the new
    /// address; the change applies only after `confirm`.
    Request {
        /// New email address.
        #[arg(long = "new-email")]
        new_email: String,
        /// Current password (required if the account already has one).
        #[arg(long = "current-password")]
        current_password: Option<String>,
    },
    /// Confirm a pending email-address change with the token from the
    /// confirmation link.
    Confirm {
        /// The one-time confirmation token.
        #[arg(long)]
        token: String,
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
///
/// `Update` carries the full documented org-settings surface, so that variant
/// is large; boxing a clap subcommand payload is non-idiomatic here.
///
/// `Debug` is implemented manually (not derived) so the `transfer-claim` bearer
/// `token` (a capability that grants org-ownership claim) is never rendered
/// verbatim through the `Cli` Debug tree (CLAUDE.md coding-standard #9).
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
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
        /// New display name (pass `null` to clear).
        #[arg(long)]
        name: Option<String>,
        /// New domain.
        #[arg(long)]
        domain: Option<String>,
        /// New description (pass `null` or empty to clear).
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
        /// Brand accent color as a JSON string (pass `null` to clear).
        #[arg(long)]
        accent_color: Option<String>,
        /// Background color as a JSON string (pass `null` to clear).
        #[arg(long)]
        background_color: Option<String>,
        /// Background display mode.
        #[arg(long)]
        background_mode: Option<String>,
        /// Enable or disable the brand background.
        #[arg(long)]
        use_background: Option<bool>,
        /// Facebook profile URL.
        #[arg(long)]
        facebook_url: Option<String>,
        /// Twitter/X profile URL.
        #[arg(long)]
        twitter_url: Option<String>,
        /// Instagram profile URL.
        #[arg(long)]
        instagram_url: Option<String>,
        /// `YouTube` channel URL.
        #[arg(long)]
        youtube_url: Option<String>,
        /// Member-management permission level.
        #[arg(long)]
        perm_member_manage: Option<String>,
        /// Authorized email domain for auto-join.
        #[arg(long)]
        perm_authorized_domains: Option<String>,
        /// Custom owner-defined properties as a JSON string (`null` clears).
        #[arg(long)]
        owner_defined: Option<String>,
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
        /// Join permission (server has no default).
        #[arg(
            long,
            default_value = "Member or above",
            value_parser = ["Member or above", "Admin or above", "Only Org Owners"],
        )]
        perm_join: String,
        /// Member-management permission (server has no default).
        #[arg(
            long,
            default_value = "Admin or above",
            value_parser = ["Member or above", "Admin or above"],
        )]
        perm_member_manage: String,
        /// Enable AI intelligence (indexing) on the new workspace.
        #[arg(long)]
        intelligence: bool,
    },
}

impl fmt::Debug for OrgCommands {
    #[allow(clippy::too_many_lines)] // a flat field-by-field render over every variant
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List { limit, offset } => f
                .debug_struct("List")
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::Create {
                name,
                domain,
                description,
                industry,
                billing_email,
            } => f
                .debug_struct("Create")
                .field("name", name)
                .field("domain", domain)
                .field("description", description)
                .field("industry", industry)
                .field("billing_email", billing_email)
                .finish(),
            Self::Info { org_id } => f.debug_struct("Info").field("org_id", org_id).finish(),
            Self::Update {
                org_id,
                name,
                domain,
                description,
                industry,
                billing_email,
                homepage_url,
                accent_color,
                background_color,
                background_mode,
                use_background,
                facebook_url,
                twitter_url,
                instagram_url,
                youtube_url,
                perm_member_manage,
                perm_authorized_domains,
                owner_defined,
            } => f
                .debug_struct("Update")
                .field("org_id", org_id)
                .field("name", name)
                .field("domain", domain)
                .field("description", description)
                .field("industry", industry)
                .field("billing_email", billing_email)
                .field("homepage_url", homepage_url)
                .field("accent_color", accent_color)
                .field("background_color", background_color)
                .field("background_mode", background_mode)
                .field("use_background", use_background)
                .field("facebook_url", facebook_url)
                .field("twitter_url", twitter_url)
                .field("instagram_url", instagram_url)
                .field("youtube_url", youtube_url)
                .field("perm_member_manage", perm_member_manage)
                .field("perm_authorized_domains", perm_authorized_domains)
                .field("owner_defined", owner_defined)
                .finish(),
            Self::Delete { org_id, confirm } => f
                .debug_struct("Delete")
                .field("org_id", org_id)
                .field("confirm", confirm)
                .finish(),
            Self::Billing(c) => f.debug_tuple("Billing").field(c).finish(),
            Self::Members(c) => f.debug_tuple("Members").field(c).finish(),
            Self::Transfer {
                org_id,
                new_owner_id,
            } => f
                .debug_struct("Transfer")
                .field("org_id", org_id)
                .field("new_owner_id", new_owner_id)
                .finish(),
            Self::Discover { limit, offset } => f
                .debug_struct("Discover")
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::PublicDetails { org_id } => f
                .debug_struct("PublicDetails")
                .field("org_id", org_id)
                .finish(),
            Self::Limits { org_id } => f.debug_struct("Limits").field("org_id", org_id).finish(),
            Self::Invitations(c) => f.debug_tuple("Invitations").field(c).finish(),
            Self::TransferToken(c) => f.debug_tuple("TransferToken").field(c).finish(),
            // Redact the bearer transfer-claim token (CLAUDE.md #9).
            Self::TransferClaim { token: _ } => f
                .debug_struct("TransferClaim")
                .field("token", &format_args!("<redacted>"))
                .finish(),
            Self::DiscoverAll { limit, offset } => f
                .debug_struct("DiscoverAll")
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::DiscoverAvailable { limit, offset } => f
                .debug_struct("DiscoverAvailable")
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::DiscoverCheckDomain { domain } => f
                .debug_struct("DiscoverCheckDomain")
                .field("domain", domain)
                .finish(),
            Self::DiscoverExternal { limit, offset } => f
                .debug_struct("DiscoverExternal")
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::Workspaces {
                org_id,
                limit,
                offset,
            } => f
                .debug_struct("Workspaces")
                .field("org_id", org_id)
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::Shares {
                org_id,
                limit,
                offset,
            } => f
                .debug_struct("Shares")
                .field("org_id", org_id)
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::OrgAsset(c) => f.debug_tuple("OrgAsset").field(c).finish(),
            Self::CreateWorkspace {
                org_id,
                name,
                folder_name,
                description,
                perm_join,
                perm_member_manage,
                intelligence,
            } => f
                .debug_struct("CreateWorkspace")
                .field("org_id", org_id)
                .field("name", name)
                .field("folder_name", folder_name)
                .field("description", description)
                .field("perm_join", perm_join)
                .field("perm_member_manage", perm_member_manage)
                .field("intelligence", intelligence)
                .finish(),
        }
    }
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
        /// Role: admin or member.
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
        /// New role: admin or member.
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
        /// New role: admin or member.
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
        /// New display name (pass `null` to clear).
        #[arg(long)]
        name: Option<String>,
        /// New description (pass `null` or empty to clear).
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
        /// Who can self-join the workspace (permission phrase).
        #[arg(long)]
        perm_join: Option<String>,
        /// Who can manage members (permission phrase).
        #[arg(long)]
        perm_member_manage: Option<String>,
        /// Brand accent color as a JSON string (pass `null` to clear).
        #[arg(long)]
        accent_color: Option<String>,
        /// Primary background color as a JSON string (pass `null` to clear).
        #[arg(long)]
        background_color1: Option<String>,
        /// Secondary background color as a JSON string (pass `null` to clear).
        #[arg(long)]
        background_color2: Option<String>,
        /// Custom owner-defined properties as a JSON string (`null` clears).
        #[arg(long)]
        owner_defined: Option<String>,
    },
    /// Delete a workspace. Permanent and irreversible.
    Delete {
        /// Workspace ID.
        workspace_id: String,
        /// Confirmation string (must match workspace folder name or ID).
        #[arg(long)]
        confirm: String,
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
        /// Always create a new folder (auto-renamed on a name collision)
        /// instead of returning an existing same-named folder.
        #[arg(long)]
        force: bool,
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
    /// Update a file or folder: rename, replace content, or set metadata
    /// title/short overrides. At least one field must be provided.
    Update {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID to update.
        node_id: String,
        /// New name.
        #[arg(long)]
        name: Option<String>,
        /// JSON-encoded content source (same shape as add-file's `from`),
        /// e.g. `{"type":"upload","upload":{"id":"<id>"}}`. Replacing content
        /// creates a new version.
        #[arg(long)]
        from: Option<String>,
        /// Custom title override (max 50 chars; pass `null` to clear).
        #[arg(long)]
        metadata_title: Option<String>,
        /// Custom short description override (max 2048 chars; `null` clears).
        #[arg(long)]
        metadata_short: Option<String>,
    },
    /// Add a file to a folder from a completed upload or by content hash.
    #[command(name = "add-file")]
    AddFile {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Filename for the new node.
        name: String,
        /// Parent folder node ID (defaults to root).
        #[arg(long)]
        parent: Option<String>,
        /// Completed upload session ID to attach (mutually exclusive with --hash).
        #[arg(long, conflicts_with_all = ["hash", "hash_type"])]
        upload_id: Option<String>,
        /// Content hash to deduplicate against (requires --hash-type).
        /// Share context only — not supported with --workspace (use --upload-id there).
        #[arg(long, requires = "hash_type")]
        hash: Option<String>,
        /// Hash algorithm for --hash.
        #[arg(long, value_parser = ["md5", "sha1", "sha256", "sha384"], requires = "hash")]
        hash_type: Option<String>,
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
        /// Filter by node type.
        #[arg(long = "type", value_parser = ["file", "folder", "link", "note"])]
        node_type: Option<String>,
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
}

/// File lock subcommands.
///
/// `Debug` is implemented manually (not derived) so the capability `lock_token`
/// and the free-form `client_info` are never rendered verbatim through the
/// `Cli` Debug tree (CLAUDE.md coding-standard #9).
#[derive(Subcommand)]
#[non_exhaustive]
pub enum FileLockCommands {
    /// Acquire a file lock.
    Acquire {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node ID.
        node_id: String,
        /// Lock duration in seconds (60-3600).
        #[arg(long, value_parser = clap::value_parser!(u32).range(60..=3600))]
        duration: Option<u32>,
        /// Client metadata as a JSON object, e.g.
        /// `{"device_name":"…","client_version":"…"}`.
        #[arg(long)]
        client_info: Option<String>,
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

impl fmt::Debug for FileLockCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render an Option<client_info> as a redacted marker, preserving only
        // whether a value was present.
        fn ci(c: Option<&String>) -> &'static str {
            match c {
                Some(_) => "Some(<redacted>)",
                None => "None",
            }
        }
        match self {
            Self::Acquire {
                workspace,
                node_id,
                duration,
                client_info,
            } => f
                .debug_struct("Acquire")
                .field("workspace", workspace)
                .field("node_id", node_id)
                .field("duration", duration)
                .field("client_info", &format_args!("{}", ci(client_info.as_ref())))
                .finish(),
            Self::Status { workspace, node_id } => f
                .debug_struct("Status")
                .field("workspace", workspace)
                .field("node_id", node_id)
                .finish(),
            Self::Release {
                workspace,
                node_id,
                lock_token: _,
            } => f
                .debug_struct("Release")
                .field("workspace", workspace)
                .field("node_id", node_id)
                .field("lock_token", &format_args!("<redacted>"))
                .finish(),
        }
    }
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID to upload into (alternative to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID to upload into (alternative to --workspace).
        #[arg(long)]
        share: Option<String>,
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
    WebList {
        /// Maximum number of jobs to return.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
        /// Filter by job status (the server validates against this exact set;
        /// upload.txt). Spelling is `canceled` (single `l`).
        #[arg(
            long,
            value_parser = [
                "pending",
                "queued",
                "downloading",
                "uploading",
                "complete",
                "failed",
                "canceled",
            ]
        )]
        status: Option<String>,
    },
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
    /// Get upload limits (optionally resolved in a target context).
    Limits {
        /// Limit-resolution action context: create or update.
        #[arg(long, value_parser = ["create", "update"])]
        action: Option<String>,
        /// Organization ID for limit resolution (used when no --action).
        #[arg(long)]
        org: Option<String>,
        /// Target workspace or share ID (required when --action is create or update).
        #[arg(long)]
        instance_id: Option<String>,
        /// Target folder `OpaqueId` or `root`.
        #[arg(long)]
        folder_id: Option<String>,
        /// File ID for update context (required when --action update, alongside --instance-id).
        #[arg(long)]
        file_id: Option<String>,
    },
    /// List supported upload hash algorithms.
    Algos,
    /// Get restricted file extensions.
    Extensions {
        /// Plan whose extension limits to return (defaults to the caller's plan).
        #[arg(long)]
        plan: Option<String>,
    },
    /// Upload a file via streaming (no exact size required upfront).
    Stream {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID to upload into (alternative to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID to upload into (alternative to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID (omit when downloading via a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID to download through (alternative to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID of the file to download.
        node_id: String,
        /// Output file path (auto-determined if omitted).
        #[arg(long, short)]
        output: Option<String>,
        /// Download a specific version (version `OpaqueId`) instead of the latest.
        #[arg(long)]
        version: Option<String>,
    },
    /// Download a folder as a ZIP archive.
    Folder {
        /// Workspace ID (omit when downloading via a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID to download through (alternative to --workspace).
        #[arg(long)]
        share: Option<String>,
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
///
/// `Create`/`Update` carry the full documented share-settings surface, so those
/// variants are large; boxing a clap subcommand payload is non-idiomatic here.
///
/// `Debug` is implemented MANUALLY (not derived) so plaintext `--password`
/// values on `Create`/`Update`/`PasswordAuth` can never leak into a debug
/// rendering (see the `impl fmt::Debug for ShareCommands` below).
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
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
        /// Share display title (2-80 chars).
        name: String,
        /// Workspace ID to create the share in.
        #[arg(long)]
        workspace: String,
        /// Share direction type (default: exchange). Note: the default
        /// `independent` storage mode always uses a Send portal regardless;
        /// the documented `exchange` default applies with
        /// `--storage-mode workspace_folder`.
        #[arg(long, value_parser = ["send", "receive", "exchange"])]
        share_type: Option<String>,
        /// Share description (10-500 chars).
        #[arg(long)]
        description: Option<String>,
        /// Access options.
        #[arg(long)]
        access_options: Option<String>,
        /// Who can manage invitations: owners or guests.
        #[arg(long, value_parser = ["owners", "guests"])]
        invite: Option<String>,
        /// Storage mode: independent (portal, default) or `workspace_folder`.
        #[arg(long, value_parser = ["independent", "workspace_folder"])]
        storage_mode: Option<String>,
        /// Backing workspace folder opaque ID (`workspace_folder` mode).
        #[arg(long)]
        folder_node_id: Option<String>,
        /// Create a new backing folder (`workspace_folder` mode, with --folder-name).
        #[arg(long)]
        create_folder: Option<bool>,
        /// Name for the new backing folder (with --create-folder).
        #[arg(long)]
        folder_name: Option<String>,
        /// URL-friendly custom name (auto-generated when omitted).
        #[arg(long)]
        custom_name: Option<String>,
        /// Password for share access (Send + 'Anyone with the link' only).
        #[arg(long)]
        password: Option<String>,
        /// Expiration datetime "YYYY-MM-DD HH:MM:SS" (portal mode only).
        #[arg(long)]
        expires: Option<String>,
        /// Notification preference.
        #[arg(long, value_parser = ["never", "notify_on_file_received", "notify_on_file_sent_or_received"])]
        notify: Option<String>,
        /// Enable comments.
        #[arg(long)]
        comments_enabled: Option<bool>,
        /// Enable guest AI chat.
        #[arg(long)]
        guest_chat_enabled: Option<bool>,
        /// Visual display mode: grid or list.
        #[arg(long, value_parser = ["grid", "list"])]
        display_type: Option<String>,
        /// Workspace visual style.
        #[arg(long)]
        workspace_style: Option<String>,
        /// Enable anonymous uploads.
        #[arg(long)]
        anonymous_uploads: Option<bool>,
        /// Enable AI intelligence features (default: false).
        #[arg(long)]
        intelligence: Option<bool>,
        /// Download security level (high, medium, or off).
        #[arg(long, value_parser = ["high", "medium", "off"])]
        download_security: Option<String>,
        /// Accent color (JSON color object).
        #[arg(long)]
        accent_color: Option<String>,
        /// Primary background color (JSON color object).
        #[arg(long)]
        background_color1: Option<String>,
        /// Secondary background color (JSON color object).
        #[arg(long)]
        background_color2: Option<String>,
        /// Background image selection (numeric).
        #[arg(long)]
        background_image: Option<i64>,
        /// Custom link #1 (JSON link object).
        #[arg(long)]
        link_1: Option<String>,
        /// Custom link #2 (JSON link object).
        #[arg(long)]
        link_2: Option<String>,
        /// Custom link #3 (JSON link object).
        #[arg(long)]
        link_3: Option<String>,
        /// Custom owner-defined properties (JSON or "null").
        #[arg(long)]
        owner_defined: Option<String>,
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
        /// New share display name.
        #[arg(long)]
        name: Option<String>,
        /// New display title (2-80 chars), or "null" to clear.
        #[arg(long)]
        title: Option<String>,
        /// New URL-friendly custom name, or "null" to clear.
        #[arg(long)]
        custom_name: Option<String>,
        /// New description, or "null"/"" to clear.
        #[arg(long)]
        description: Option<String>,
        /// Share direction type.
        #[arg(long, value_parser = ["send", "receive", "exchange"])]
        share_type: Option<String>,
        /// New access options.
        #[arg(long)]
        access_options: Option<String>,
        /// Who can manage invitations: owners or guests.
        #[arg(long, value_parser = ["owners", "guests"])]
        invite: Option<String>,
        /// Password (Send + 'Anyone with the link'); "null"/"" to clear.
        #[arg(long)]
        password: Option<String>,
        /// Expiration datetime (portal mode only), or "null" to clear.
        #[arg(long)]
        expires: Option<String>,
        /// Notification preference.
        #[arg(long, value_parser = ["never", "notify_on_file_received", "notify_on_file_sent_or_received"])]
        notify: Option<String>,
        /// Enable or disable downloads (legacy — prefer --download-security).
        #[arg(long)]
        download_enabled: Option<bool>,
        /// Enable or disable comments.
        #[arg(long)]
        comments_enabled: Option<bool>,
        /// Download security level (high, medium, or off).
        #[arg(long, value_parser = ["high", "medium", "off"])]
        download_security: Option<String>,
        /// Visual display mode: grid or list.
        #[arg(long, value_parser = ["grid", "list"])]
        display_type: Option<String>,
        /// Workspace visual style.
        #[arg(long)]
        workspace_style: Option<String>,
        /// Enable or disable guest AI chat.
        #[arg(long)]
        guest_chat_enabled: Option<bool>,
        /// Toggle AI indexing (intelligence).
        #[arg(long)]
        intelligence: Option<bool>,
        /// Enable or disable anonymous uploads.
        #[arg(long)]
        anonymous_uploads: Option<bool>,
        /// Accent color (JSON color object), or "null".
        #[arg(long)]
        accent_color: Option<String>,
        /// Primary background color (JSON color object), or "null".
        #[arg(long)]
        background_color1: Option<String>,
        /// Secondary background color (JSON color object), or "null".
        #[arg(long)]
        background_color2: Option<String>,
        /// Background image selection (numeric).
        #[arg(long)]
        background_image: Option<i64>,
        /// Custom link #1 (JSON link object), or "null".
        #[arg(long)]
        link_1: Option<String>,
        /// Custom link #2 (JSON link object), or "null".
        #[arg(long)]
        link_2: Option<String>,
        /// Custom link #3 (JSON link object), or "null".
        #[arg(long)]
        link_3: Option<String>,
        /// Custom owner-defined properties (JSON or "null").
        #[arg(long)]
        owner_defined: Option<String>,
        /// Remove the workspace share-link node (pass `null` — the only
        /// accepted value).
        #[arg(long)]
        share_link_node_id: Option<String>,
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
    /// Share file operations.
    #[command(subcommand)]
    Files(ShareFilesCommands),
    /// Share member operations.
    #[command(subcommand)]
    Members(ShareMembersCommands),
    /// Share invitation operations.
    #[command(subcommand)]
    Invitation(ShareInvitationCommands),
}

/// Manual `Debug` for [`ShareCommands`] that REDACTS every `--password` value so
/// a secret can never leak into a debug rendering (logs, panics). `Create` and
/// `Update` carry an `Option<String>` password; `PasswordAuth` carries a plain
/// `String` password.
///
/// `#[derive(Debug)]` would print these passwords verbatim, and `Cli`'s manual
/// `Debug` recurses into the active command — so the derive is removed and each
/// variant is rendered field-by-field with the `password` field replaced by a
/// fixed redaction marker; all other fields are shown as-is.
impl fmt::Debug for ShareCommands {
    #[allow(clippy::too_many_lines)] // a flat field-by-field render over every variant
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render an Option<password> as a redacted marker, preserving only
        // whether a value was present.
        fn pw(p: Option<&String>) -> &'static str {
            match p {
                Some(_) => "Some(<redacted>)",
                None => "None",
            }
        }
        match self {
            Self::List { limit, offset } => f
                .debug_struct("List")
                .field("limit", limit)
                .field("offset", offset)
                .finish(),
            Self::Create {
                name,
                workspace,
                share_type,
                description,
                access_options,
                invite,
                storage_mode,
                folder_node_id,
                create_folder,
                folder_name,
                custom_name,
                password,
                expires,
                notify,
                comments_enabled,
                guest_chat_enabled,
                display_type,
                workspace_style,
                anonymous_uploads,
                intelligence,
                download_security,
                accent_color,
                background_color1,
                background_color2,
                background_image,
                link_1,
                link_2,
                link_3,
                owner_defined,
            } => f
                .debug_struct("Create")
                .field("name", name)
                .field("workspace", workspace)
                .field("share_type", share_type)
                .field("description", description)
                .field("access_options", access_options)
                .field("invite", invite)
                .field("storage_mode", storage_mode)
                .field("folder_node_id", folder_node_id)
                .field("create_folder", create_folder)
                .field("folder_name", folder_name)
                .field("custom_name", custom_name)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .field("expires", expires)
                .field("notify", notify)
                .field("comments_enabled", comments_enabled)
                .field("guest_chat_enabled", guest_chat_enabled)
                .field("display_type", display_type)
                .field("workspace_style", workspace_style)
                .field("anonymous_uploads", anonymous_uploads)
                .field("intelligence", intelligence)
                .field("download_security", download_security)
                .field("accent_color", accent_color)
                .field("background_color1", background_color1)
                .field("background_color2", background_color2)
                .field("background_image", background_image)
                .field("link_1", link_1)
                .field("link_2", link_2)
                .field("link_3", link_3)
                .field("owner_defined", owner_defined)
                .finish(),
            Self::Info { share_id } => f.debug_struct("Info").field("share_id", share_id).finish(),
            Self::Update {
                share_id,
                name,
                title,
                custom_name,
                description,
                share_type,
                access_options,
                invite,
                password,
                expires,
                notify,
                download_enabled,
                comments_enabled,
                download_security,
                display_type,
                workspace_style,
                guest_chat_enabled,
                intelligence,
                anonymous_uploads,
                accent_color,
                background_color1,
                background_color2,
                background_image,
                link_1,
                link_2,
                link_3,
                owner_defined,
                share_link_node_id,
            } => f
                .debug_struct("Update")
                .field("share_id", share_id)
                .field("name", name)
                .field("title", title)
                .field("custom_name", custom_name)
                .field("description", description)
                .field("share_type", share_type)
                .field("access_options", access_options)
                .field("invite", invite)
                .field("password", &format_args!("{}", pw(password.as_ref())))
                .field("expires", expires)
                .field("notify", notify)
                .field("download_enabled", download_enabled)
                .field("comments_enabled", comments_enabled)
                .field("download_security", download_security)
                .field("display_type", display_type)
                .field("workspace_style", workspace_style)
                .field("guest_chat_enabled", guest_chat_enabled)
                .field("intelligence", intelligence)
                .field("anonymous_uploads", anonymous_uploads)
                .field("accent_color", accent_color)
                .field("background_color1", background_color1)
                .field("background_color2", background_color2)
                .field("background_image", background_image)
                .field("link_1", link_1)
                .field("link_2", link_2)
                .field("link_3", link_3)
                .field("owner_defined", owner_defined)
                .field("share_link_node_id", share_link_node_id)
                .finish(),
            Self::Delete { share_id, confirm } => f
                .debug_struct("Delete")
                .field("share_id", share_id)
                .field("confirm", confirm)
                .finish(),
            Self::Archive { share_id } => f
                .debug_struct("Archive")
                .field("share_id", share_id)
                .finish(),
            Self::Unarchive { share_id } => f
                .debug_struct("Unarchive")
                .field("share_id", share_id)
                .finish(),
            Self::PasswordAuth { share_id, .. } => f
                .debug_struct("PasswordAuth")
                .field("share_id", share_id)
                .field("password", &format_args!("<redacted>"))
                .finish(),
            Self::GuestAuth { share_id } => f
                .debug_struct("GuestAuth")
                .field("share_id", share_id)
                .finish(),
            Self::PublicInfo { share_id } => f
                .debug_struct("PublicInfo")
                .field("share_id", share_id)
                .finish(),
            Self::Available => write!(f, "Available"),
            Self::CheckName { name } => f.debug_struct("CheckName").field("name", name).finish(),
            Self::Files(c) => f.debug_tuple("Files").field(c).finish(),
            Self::Members(c) => f.debug_tuple("Members").field(c).finish(),
            Self::Invitation(c) => f.debug_tuple("Invitation").field(c).finish(),
        }
    }
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
    /// Add a member (19-digit user ID) or send an invitation (email) to a share.
    Add {
        /// Share ID.
        share_id: String,
        /// Email address (invite) or 19-digit user ID (add existing user).
        email: String,
        /// Permission role: admin, member, guest, or view.
        #[arg(long, value_parser = ["admin", "member", "guest", "view"])]
        role: Option<String>,
        /// Notification preference (existing-user add).
        #[arg(long)]
        notify_options: Option<String>,
        /// Membership expiration "YYYY-MM-DD HH:MM:SS UTC"; "null"/"" to clear.
        #[arg(long)]
        expires: Option<String>,
        /// Resend notification email (60s cooldown after initial add).
        #[arg(long)]
        force_notification: Option<bool>,
        /// Custom message for the invitation email (email invite).
        #[arg(long)]
        message: Option<String>,
        /// Invitation expiration datetime (email invite).
        #[arg(long)]
        invitation_expires: Option<String>,
    },
    /// Update a member's permissions, notification preference, or expiration.
    Update {
        /// Share ID.
        share_id: String,
        /// Member user ID.
        member_id: String,
        /// New permission role: admin, member, guest, or view.
        #[arg(long, value_parser = ["admin", "member", "guest", "view"])]
        role: Option<String>,
        /// Notification preference.
        #[arg(long)]
        notify_options: Option<String>,
        /// Membership expiration "YYYY-MM-DD HH:MM:SS"; "null"/"" to clear.
        #[arg(long)]
        expires: Option<String>,
    },
    /// Get member details.
    Info {
        /// Share ID.
        share_id: String,
        /// Member user ID.
        member_id: String,
    },
    /// Transfer share ownership to another member (current owner → admin).
    Transfer {
        /// Share ID.
        share_id: String,
        /// Member user ID to transfer ownership to.
        member_id: String,
    },
    /// Leave a share (self-removal). Owners must transfer ownership first.
    Leave {
        /// Share ID.
        share_id: String,
    },
    /// Self-join a share (where the access option permits).
    Join {
        /// Share ID.
        share_id: String,
    },
    /// Remove a member from a share.
    Remove {
        /// Share ID.
        share_id: String,
        /// Member ID to remove.
        member_id: String,
    },
}

/// Share invitation subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum ShareInvitationCommands {
    /// List a share's invitations (optionally filtered by state).
    List {
        /// Share ID.
        share_id: String,
        /// Filter by state: pending, accepted, declined.
        #[arg(long, value_parser = ["pending", "accepted", "declined"])]
        state: Option<String>,
    },
    /// Update a share invitation (state, role, notification, or expiration).
    Update {
        /// Share ID.
        share_id: String,
        /// Invitation ID (numeric) or email address.
        invitation_id: String,
        /// New state: pending, accepted, declined.
        #[arg(long, value_parser = ["pending", "accepted", "declined"])]
        state: Option<String>,
        /// New permission role: admin, member, guest, or view.
        #[arg(long, value_parser = ["admin", "member", "guest", "view"])]
        role: Option<String>,
        /// Notification preference.
        #[arg(long)]
        notify_options: Option<String>,
        /// Membership expiration datetime.
        #[arg(long)]
        expires: Option<String>,
    },
    /// Revoke (delete) a share invitation.
    Delete {
        /// Share ID.
        share_id: String,
        /// Invitation ID (numeric) or email address.
        invitation_id: String,
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
        /// Sort order: asc or desc (default asc).
        #[arg(long, value_parser = ["asc", "desc"])]
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
        /// Anchoring reference as a JSON object string (or `@file.json`), e.g.
        /// `{"type":"page","page":3}`.
        #[arg(long)]
        reference: Option<String>,
        /// Arbitrary metadata as a JSON object string (or `@file.json`).
        #[arg(long)]
        properties: Option<String>,
        /// Inline-attach a single object to the new comment (object ID).
        /// Mutually exclusive with `--target-ids`.
        #[arg(long, conflicts_with = "target_ids")]
        target_id: Option<String>,
        /// Inline-attach multiple objects to the new comment (comma-separated
        /// object IDs, ≤25). Mutually exclusive with `--target-id`.
        #[arg(long, value_delimiter = ',')]
        target_ids: Vec<String>,
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
    /// Edit a comment's text (author-only; works for any comment by ID).
    Edit {
        /// Comment ID.
        comment_id: String,
        /// New comment text.
        text: String,
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
        /// Sort order: asc or desc (default asc).
        #[arg(long, value_parser = ["asc", "desc"])]
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
    /// Bulk soft-delete up to 100 comments by ID (not recursive).
    #[command(name = "bulk-delete")]
    BulkDelete {
        /// Comma-separated comment IDs (max 100).
        #[arg(long, value_delimiter = ',', required = true)]
        comment_ids: Vec<String>,
    },
    /// List the objects attached to a comment (hydrated, access-gated).
    Attachments {
        /// Comment ID.
        comment_id: String,
    },
    /// Attach one or more objects to a comment (atomic; idempotent; ≤25 total;
    /// author-only).
    Attach {
        /// Comment ID.
        comment_id: String,
        /// Attach a single object (object ID). Mutually exclusive with
        /// `--target-ids`.
        #[arg(long, conflicts_with = "target_ids")]
        target_id: Option<String>,
        /// Attach multiple objects (comma-separated object IDs, ≤25). Mutually
        /// exclusive with `--target-id`.
        #[arg(long, value_delimiter = ',')]
        target_ids: Vec<String>,
    },
    /// Detach a single object from a comment (no batch detach — call once per
    /// object; author-only).
    Detach {
        /// Comment ID.
        comment_id: String,
        /// Object ID to detach.
        #[arg(long)]
        target_id: String,
    },
}

// ─── Event ──────────────────────────────────────────────────────────────────

/// Event subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum EventCommands {
    /// List/search activity events.
    ///
    /// One of `--workspace` / `--share` / `--user-id` / `--org-id` /
    /// `--parent-event-id` is required by the server. `--parent-event-id`
    /// cannot be combined with filters other than `--acknowledged` / `--limit`
    /// / `--offset` (the server enforces this).
    List {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Filter by acting user profile ID (19-digit).
        #[arg(long)]
        user_id: Option<String>,
        /// Filter by organization ID (19-digit).
        #[arg(long)]
        org_id: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Filter by subcategory.
        #[arg(long)]
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children (parent event ID).
        #[arg(long)]
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event (19-digit; distinct from
        /// `--user-id`).
        #[arg(long)]
        calling_user_id: Option<String>,
        /// Filter by related object (file/folder) ID.
        #[arg(long)]
        object_id: Option<String>,
        /// Audit-log read filter: `external_audit_log` or `external`.
        #[arg(long, value_parser = ["external_audit_log", "external"])]
        visibility: Option<String>,
        /// Filter by acknowledgment status (true or false).
        #[arg(long)]
        acknowledged: Option<bool>,
        /// Lower bound for event creation time (ISO-8601 or
        /// `YYYY-MM-DD HH:MM:SS`).
        #[arg(long)]
        created_min: Option<String>,
        /// Upper bound for event creation time (same format as `--created-min`;
        /// must be greater than `--created-min`).
        #[arg(long)]
        created_max: Option<String>,
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
    ///
    /// Accepts every filter `event list` does (the summarize endpoint shares the
    /// search filter set) plus the summarize-only `--user-context`. As with
    /// `list`, one of `--workspace` / `--share` / `--user-id` / `--org-id` /
    /// `--parent-event-id` is required by the server.
    Summarize {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Filter by acting user profile ID (19-digit).
        #[arg(long)]
        user_id: Option<String>,
        /// Filter by organization ID (19-digit).
        #[arg(long)]
        org_id: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Filter by subcategory.
        #[arg(long)]
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children (parent event ID).
        #[arg(long)]
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event (19-digit; distinct from
        /// `--user-id`).
        #[arg(long)]
        calling_user_id: Option<String>,
        /// Filter by related object (file/folder) ID.
        #[arg(long)]
        object_id: Option<String>,
        /// Audit-log read filter: `external_audit_log` or `external`.
        #[arg(long, value_parser = ["external_audit_log", "external"])]
        visibility: Option<String>,
        /// Filter by acknowledgment status (true or false).
        #[arg(long)]
        acknowledged: Option<bool>,
        /// Lower bound for event creation time (ISO-8601 or
        /// `YYYY-MM-DD HH:MM:SS`).
        #[arg(long)]
        created_min: Option<String>,
        /// Upper bound for event creation time (same format as `--created-min`;
        /// must be greater than `--created-min`).
        #[arg(long)]
        created_max: Option<String>,
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

// ─── Dashboard ──────────────────────────────────────────────────────────────

/// Dashboard subcommands (per-workspace actionable card feed).
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum DashboardCommands {
    /// Get the calling member's ranked, paginated card feed for a workspace.
    Get {
        /// Workspace ID (19-digit) or folder name.
        #[arg(long)]
        workspace: String,
        /// Cards per page (1–200; server default 50).
        #[arg(long)]
        limit: Option<u32>,
        /// Cards to skip for pagination (server default 0).
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Dismiss a card permanently, or snooze it until a future time.
    ///
    /// Per-member and out-of-band: this only hides the card from your own feed
    /// — it never advances, resolves, or changes the underlying card subject.
    /// Pass `--snooze-until` to snooze instead of permanently dismissing.
    Dismiss {
        /// Card key from the feed (e.g. `obligation:123…`). URL-encoding is
        /// handled for you.
        card_key: String,
        /// Workspace ID (19-digit).
        #[arg(long)]
        workspace: String,
        /// Snooze the card until this UTC time (`YYYY-MM-DD HH:MM:SS UTC`); must
        /// be in the future. Omit for a permanent dismiss.
        #[arg(long)]
        snooze_until: Option<String>,
    },
    /// Undismiss (or un-snooze) a card, restoring it to your feed.
    ///
    /// Idempotent — undismissing a card that was never dismissed succeeds
    /// silently. Reverses `dashboard dismiss`.
    Undismiss {
        /// Card key to restore (URL-encoding is handled for you).
        card_key: String,
        /// Workspace ID (19-digit).
        #[arg(long)]
        workspace: String,
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
        #[arg(long, value_parser = ["bin", "thumbnail", "image", "hlsstream", "pdf", "spreadsheet", "audio", "mp4"])]
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
    /// Request an image transformation URL (resize, crop, format conversion).
    ///
    /// Returns `{transform_name, token, read_url}` (the two-step model): fetch
    /// `read_url` to get the transformed bytes. `read_url` and `token` are
    /// secret-bearing read capabilities — do not log or share them.
    Transform {
        /// Storage node ID.
        node_id: String,
        /// Transform name (must be "image", the only valid value).
        #[arg(long, default_value = "image", value_parser = ["image"])]
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
        /// Output format: png, jpg, or jpeg.
        #[arg(long, value_parser = ["png", "jpg", "jpeg"])]
        output_format: Option<String>,
        /// Size preset: `IconTiny`, `IconSmall`, `IconMedium`, or Preview
        /// (case-insensitive).
        #[arg(long, value_parser = parse_preview_size)]
        size: Option<String>,
        /// Crop rectangle width (all four crop flags required together).
        #[arg(long)]
        crop_width: Option<u32>,
        /// Crop rectangle height.
        #[arg(long)]
        crop_height: Option<u32>,
        /// Crop rectangle x offset.
        #[arg(long)]
        crop_x: Option<u32>,
        /// Crop rectangle y offset.
        #[arg(long)]
        crop_y: Option<u32>,
        /// Rotation in degrees: 0, 90, 180, or 270.
        #[arg(long, value_parser = parse_preview_rotate)]
        rotate: Option<u32>,
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
///
/// `Debug` is implemented manually (not derived) so the capability `lock_token`
/// and the free-form `client_info` are never rendered verbatim through the
/// `Cli` Debug tree (CLAUDE.md coding-standard #9).
#[derive(Subcommand)]
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
        /// Lock duration in seconds (60-3600).
        #[arg(long, value_parser = clap::value_parser!(u32).range(60..=3600))]
        duration: Option<u32>,
        /// Client metadata as a JSON object, e.g.
        /// `{"device_name":"…","client_version":"…"}`.
        #[arg(long)]
        client_info: Option<String>,
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

impl fmt::Debug for LockCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render an Option<client_info> as a redacted marker, preserving only
        // whether a value was present.
        fn ci(c: Option<&String>) -> &'static str {
            match c {
                Some(_) => "Some(<redacted>)",
                None => "None",
            }
        }
        match self {
            Self::Acquire {
                context_type,
                context_id,
                node_id,
                duration,
                client_info,
            } => f
                .debug_struct("Acquire")
                .field("context_type", context_type)
                .field("context_id", context_id)
                .field("node_id", node_id)
                .field("duration", duration)
                .field("client_info", &format_args!("{}", ci(client_info.as_ref())))
                .finish(),
            Self::Status {
                context_type,
                context_id,
                node_id,
            } => f
                .debug_struct("Status")
                .field("context_type", context_type)
                .field("context_id", context_id)
                .field("node_id", node_id)
                .finish(),
            Self::Release {
                context_type,
                context_id,
                node_id,
                lock_token: _,
            } => f
                .debug_struct("Release")
                .field("context_type", context_type)
                .field("context_id", context_id)
                .field("node_id", node_id)
                .field("lock_token", &format_args!("<redacted>"))
                .finish(),
            Self::Heartbeat {
                context_type,
                context_id,
                node_id,
                lock_token: _,
            } => f
                .debug_struct("Heartbeat")
                .field("context_type", context_type)
                .field("context_id", context_id)
                .field("node_id", node_id)
                .field("lock_token", &format_args!("<redacted>"))
                .finish(),
        }
    }
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

// ─── Identifier inspection ───────────────────────────────────────────────────

/// Offline `OpaqueId` inspection subcommands.
///
/// Pure, local classification — no auth, no network. Treats every id as opaque
/// and reads only the self-describing length + type prefix per the documented
/// type-prefix → entity map.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum IdCommands {
    /// Classify one or more Fast.io identifiers and print their entity type,
    /// family, and surfacing tier.
    Info {
        /// One or more ids to inspect (raw or hyphenated; mixed lengths OK).
        #[arg(required = true)]
        ids: Vec<String>,
    },
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
                agent,
            } => f
                .debug_struct("Signup")
                .field("email", email)
                .field("password", &"[REDACTED]")
                .field("first_name", first_name)
                .field("last_name", last_name)
                .field("agent", agent)
                .finish(),
            Self::PasswordReset {
                code: _,
                password1: _,
                password2: _,
            } => f
                .debug_struct("PasswordReset")
                .field("code", &"[REDACTED]")
                .field("password1", &"[REDACTED]")
                .field("password2", &"[REDACTED]")
                .finish(),
            Self::Logout => write!(f, "Logout"),
            Self::Signout => write!(f, "Signout"),
            Self::InvalidateAll => write!(f, "InvalidateAll"),
            Self::Status => write!(f, "Status"),
            Self::Verify { email, code: _ } => f
                .debug_struct("Verify")
                .field("email", email)
                .field("code", &"[REDACTED]")
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
            Self::PasswordResetCheck { code: _ } => f
                .debug_struct("PasswordResetCheck")
                .field("code", &"[REDACTED]")
                .finish(),
            #[allow(unreachable_patterns)]
            _ => write!(f, "AuthCommands(<unknown variant>)"),
        }
    }
}

impl fmt::Debug for TwoFaCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup { channel } => f.debug_struct("Setup").field("channel", channel).finish(),
            Self::Verify { code: _ } => f
                .debug_struct("Verify")
                .field("code", &"[REDACTED]")
                .finish(),
            Self::Disable { token: _ } => f
                .debug_struct("Disable")
                .field("token", &"[REDACTED]")
                .finish(),
            Self::Status => write!(f, "Status"),
            Self::Send { channel } => f.debug_struct("Send").field("channel", channel).finish(),
            Self::VerifySetup { token: _ } => f
                .debug_struct("VerifySetup")
                .field("token", &"[REDACTED]")
                .finish(),
            #[allow(unreachable_patterns)]
            _ => write!(f, "TwoFaCommands(<unknown variant>)"),
        }
    }
}

impl fmt::Debug for UserCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => write!(f, "Info"),
            Self::Update {
                first_name,
                last_name,
                display_name,
                phone_country,
                phone_number,
                password: _,
                current_password: _,
            } => f
                .debug_struct("Update")
                .field("first_name", first_name)
                .field("last_name", last_name)
                .field("display_name", display_name)
                .field("phone_country", phone_country)
                .field("phone_number", phone_number)
                .field("password", &"[REDACTED]")
                .field("current_password", &"[REDACTED]")
                .finish(),
            Self::EmailChange(cmds) => f.debug_tuple("EmailChange").field(cmds).finish(),
            Self::Avatar(cmds) => f.debug_tuple("Avatar").field(cmds).finish(),
            Self::Settings(cmds) => f.debug_tuple("Settings").field(cmds).finish(),
            Self::Search { query } => f.debug_struct("Search").field("query", query).finish(),
            Self::Close { confirmation } => f
                .debug_struct("Close")
                .field("confirmation", confirmation)
                .finish(),
            Self::Details { user_id } => {
                f.debug_struct("Details").field("user_id", user_id).finish()
            }
            Self::Profiles => write!(f, "Profiles"),
            Self::Allowed => write!(f, "Allowed"),
            Self::OrgLimits => write!(f, "OrgLimits"),
            Self::Shares => write!(f, "Shares"),
            Self::Invitations(cmds) => f.debug_tuple("Invitations").field(cmds).finish(),
            Self::Asset(cmds) => f.debug_tuple("Asset").field(cmds).finish(),
            Self::Autosync { state } => f.debug_struct("Autosync").field("state", state).finish(),
            Self::Pin => write!(f, "Pin"),
            Self::Phone {
                country_code,
                phone_number,
            } => f
                .debug_struct("Phone")
                .field("country_code", country_code)
                .field("phone_number", phone_number)
                .finish(),
            #[allow(unreachable_patterns)]
            _ => write!(f, "UserCommands(<unknown variant>)"),
        }
    }
}

impl fmt::Debug for UserEmailChangeCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request {
                new_email,
                current_password: _,
            } => f
                .debug_struct("Request")
                .field("new_email", new_email)
                .field("current_password", &"[REDACTED]")
                .finish(),
            Self::Confirm { token: _ } => f
                .debug_struct("Confirm")
                .field("token", &"[REDACTED]")
                .finish(),
            #[allow(unreachable_patterns)]
            _ => write!(f, "UserEmailChangeCommands(<unknown variant>)"),
        }
    }
}

#[cfg(test)]
mod ripley_alias_tests {
    use super::{
        Cli, Commands, OrgBillingCommands, OrgCommands, RipleyCommands, SearchCommands,
        SignCommands, SignDocumentCommands, SignEnvelopeCommands,
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
        // The headline `ask` verb IS visible.
        assert!(
            help.contains("ask"),
            "`ask` should be visible in ripley help"
        );
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

    // ── Sign workspace-only migration parse guards ───────────────────────

    #[test]
    fn sign_envelope_get_requires_workspace() {
        // The workspace-only migration made `--workspace` mandatory everywhere.
        // `get` without it must be rejected; with it, it parses.
        let missing = Cli::try_parse_from(["fastio", "sign", "envelope", "get", "env1"]);
        assert!(
            missing.is_err(),
            "`sign envelope get` must require --workspace"
        );

        let cli = Cli::try_parse_from([
            "fastio",
            "sign",
            "envelope",
            "get",
            "--workspace",
            "ws1",
            "env1",
        ])
        .expect("`sign envelope get --workspace ws1 env1` should parse");
        match cli.command {
            Commands::Sign(SignCommands::Envelope(SignEnvelopeCommands::Get {
                workspace,
                envelope_id,
            })) => {
                assert_eq!(workspace, "ws1");
                assert_eq!(envelope_id, "env1");
            }
            other => panic!("expected Sign Envelope Get, got {other:?}"),
        }
    }

    #[test]
    fn sign_rejects_legacy_parent_type_and_parent_id_flags() {
        // The old org/workspace dual-parent surface (--parent-type/--parent-id)
        // was removed; both flags must now be unknown args.
        let parent_type = Cli::try_parse_from([
            "fastio",
            "sign",
            "envelope",
            "list",
            "--parent-type",
            "workspace",
            "--parent-id",
            "ws1",
        ]);
        assert!(
            parent_type.is_err(),
            "legacy --parent-type/--parent-id must be rejected"
        );
    }

    #[test]
    fn sign_document_preview_parses() {
        let cli = Cli::try_parse_from([
            "fastio",
            "sign",
            "document",
            "preview",
            "--workspace",
            "ws1",
            "env1",
            "doc1",
            "-o",
            "./preview.pdf",
        ])
        .expect("`sign document preview` should parse");
        match cli.command {
            Commands::Sign(SignCommands::Document(SignDocumentCommands::Preview {
                workspace,
                envelope_id,
                document_id,
                output,
            })) => {
                assert_eq!(workspace, "ws1");
                assert_eq!(envelope_id, "env1");
                assert_eq!(document_id, "doc1");
                assert_eq!(output, "./preview.pdf");
            }
            other => panic!("expected Sign Document Preview, got {other:?}"),
        }
    }

    #[test]
    fn sign_envelope_delete_no_longer_parses() {
        // Envelopes are voided, never deleted — the `delete` subcommand was
        // removed and must not parse.
        let res = Cli::try_parse_from([
            "fastio",
            "sign",
            "envelope",
            "delete",
            "--workspace",
            "ws1",
            "env1",
        ]);
        assert!(
            res.is_err(),
            "`sign envelope delete` must not parse (use `void`)"
        );
    }

    #[test]
    fn sign_envelope_list_filter_flags_parse() {
        let cli = Cli::try_parse_from([
            "fastio",
            "sign",
            "envelope",
            "list",
            "--workspace",
            "ws1",
            "--status",
            "draft,sent",
            "--created-after",
            "2026-06-01 00:00:00 UTC",
            "--created-before",
            "2026-06-30 23:59:59 UTC",
            "--limit",
            "50",
            "--offset",
            "10",
        ])
        .expect("`sign envelope list` filter flags should parse");
        match cli.command {
            Commands::Sign(SignCommands::Envelope(SignEnvelopeCommands::List {
                workspace,
                status,
                created_after,
                created_before,
                limit,
                offset,
            })) => {
                assert_eq!(workspace, "ws1");
                assert_eq!(status.as_deref(), Some("draft,sent"));
                assert_eq!(created_after.as_deref(), Some("2026-06-01 00:00:00 UTC"));
                assert_eq!(created_before.as_deref(), Some("2026-06-30 23:59:59 UTC"));
                assert_eq!(limit, Some(50));
                assert_eq!(offset, Some(10));
            }
            other => panic!("expected Sign Envelope List, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod fileshare_parse_tests {
    use super::{Cli, Commands, FileshareCommands, FileshareGrantsCommands};
    use clap::Parser;

    /// Helper: parse argv into a [`FileshareCommands`], panicking on a parse
    /// error with the clap message (so the cause is visible).
    fn parse(args: &[&str]) -> FileshareCommands {
        let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("parse failed: {e}"));
        match cli.command {
            Commands::Fileshare(c) => c,
            other => panic!("expected Fileshare, got {other:?}"),
        }
    }

    #[test]
    fn create_parses_with_all_flags() {
        let c = parse(&[
            "fastio",
            "fileshare",
            "create",
            "--workspace",
            "ws1",
            "--node",
            "node1",
            "--title",
            "Q3",
            "--access-option",
            "anyone_with_link",
            "--password",
            "pw",
            "--expires",
            "3600",
        ]);
        match c {
            FileshareCommands::Create {
                workspace,
                node,
                title,
                access_option,
                expires,
                ..
            } => {
                assert_eq!(workspace, "ws1");
                assert_eq!(node, "node1");
                assert_eq!(title.as_deref(), Some("Q3"));
                assert_eq!(access_option.as_deref(), Some("anyone_with_link"));
                assert_eq!(expires, Some(3600));
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn fs_alias_is_removed() {
        // P2F-7: the `fs` alias was removed (scope drift + `files` collision). The
        // canonical `fileshare` name still routes; `fs` must no longer parse.
        let c = parse(&["fastio", "fileshare", "list", "--workspace", "ws1"]);
        assert!(matches!(c, FileshareCommands::List { .. }));
        assert!(
            Cli::try_parse_from(["fastio", "fs", "list", "--workspace", "ws1"]).is_err(),
            "the `fs` alias must be gone"
        );
    }

    #[test]
    fn create_requires_workspace_and_node() {
        // Missing --node.
        assert!(
            Cli::try_parse_from(["fastio", "fileshare", "create", "--workspace", "ws1"]).is_err()
        );
        // Missing --workspace.
        assert!(Cli::try_parse_from(["fastio", "fileshare", "create", "--node", "n1"]).is_err());
    }

    #[test]
    fn create_rejects_both_expiry_inputs() {
        // --expires conflicts_with --expires-at at the clap layer.
        assert!(
            Cli::try_parse_from([
                "fastio",
                "fileshare",
                "create",
                "--workspace",
                "ws1",
                "--node",
                "n1",
                "--expires",
                "60",
                "--expires-at",
                "2026-12-31 00:00:00",
            ])
            .is_err()
        );
    }

    #[test]
    fn create_rejects_bad_access_option() {
        // The value_parser allowlist rejects an unknown tier.
        assert!(
            Cli::try_parse_from([
                "fastio",
                "fileshare",
                "create",
                "--workspace",
                "ws1",
                "--node",
                "n1",
                "--access-option",
                "public",
            ])
            .is_err()
        );
    }

    #[test]
    fn update_password_conflicts_with_clear_password() {
        assert!(
            Cli::try_parse_from([
                "fastio",
                "fileshare",
                "update",
                "fs1",
                "--password",
                "pw",
                "--clear-password",
            ])
            .is_err()
        );
    }

    #[test]
    fn update_expiry_intents_conflict() {
        // expires / expires-at / clear-expires are pairwise exclusive at clap.
        for pair in [
            ["--expires", "60", "--clear-expires"].as_slice(),
            ["--expires-at", "2026-12-31 00:00:00", "--clear-expires"].as_slice(),
            ["--expires", "60", "--expires-at"].as_slice(),
        ] {
            let mut args = vec!["fastio", "fileshare", "update", "fs1"];
            args.extend_from_slice(pair);
            // The last pair needs a value for --expires-at to reach the conflict
            // check; append one so the only failure is the conflict.
            if pair.last() == Some(&"--expires-at") {
                args.push("2026-12-31 00:00:00");
            }
            assert!(
                Cli::try_parse_from(&args).is_err(),
                "expiry intents must conflict: {args:?}"
            );
        }
    }

    #[test]
    fn grants_add_parses_and_requires_capability() {
        let c = parse(&[
            "fastio",
            "fileshare",
            "grants",
            "add",
            "fs1",
            "--user",
            "u1",
            "--capability",
            "edit",
        ]);
        match c {
            FileshareCommands::Grants(FileshareGrantsCommands::Add {
                fileshare_id,
                user,
                capability,
                ..
            }) => {
                assert_eq!(fileshare_id, "fs1");
                assert_eq!(user.as_deref(), Some("u1"));
                assert_eq!(capability, "edit");
            }
            other => panic!("expected Grants Add, got {other:?}"),
        }
        // --capability is required on add.
        assert!(
            Cli::try_parse_from([
                "fastio",
                "fileshare",
                "grants",
                "add",
                "fs1",
                "--user",
                "u1",
            ])
            .is_err()
        );
    }

    #[test]
    fn grants_add_user_conflicts_with_email_and_rejects_bad_capability() {
        // --user conflicts_with --email.
        assert!(
            Cli::try_parse_from([
                "fastio",
                "fileshare",
                "grants",
                "add",
                "fs1",
                "--user",
                "u1",
                "--email",
                "a@b.com",
                "--capability",
                "view",
            ])
            .is_err()
        );
        // An unknown capability is rejected by the value_parser allowlist.
        assert!(
            Cli::try_parse_from([
                "fastio",
                "fileshare",
                "grants",
                "add",
                "fs1",
                "--user",
                "u1",
                "--capability",
                "admin",
            ])
            .is_err()
        );
    }

    #[test]
    fn grants_remove_user_conflicts_with_email() {
        assert!(
            Cli::try_parse_from([
                "fastio",
                "fileshare",
                "grants",
                "remove",
                "fs1",
                "--user",
                "u1",
                "--email",
                "a@b.com",
            ])
            .is_err()
        );
    }

    #[test]
    fn download_versions_preview_info_parse() {
        assert!(matches!(
            parse(&["fastio", "fileshare", "info", "fs1"]),
            FileshareCommands::Info { .. }
        ));
        assert!(matches!(
            parse(&["fastio", "fileshare", "versions", "fs1", "--password", "pw"]),
            FileshareCommands::Versions { .. }
        ));
        match parse(&[
            "fastio",
            "fileshare",
            "download",
            "fs1",
            "--output",
            "out.bin",
            "--version",
            "v7",
        ]) {
            FileshareCommands::Download {
                fileshare_id,
                output,
                version,
                ..
            } => {
                assert_eq!(fileshare_id, "fs1");
                assert_eq!(output.as_deref(), Some("out.bin"));
                assert_eq!(version.as_deref(), Some("v7"));
            }
            other => panic!("expected Download, got {other:?}"),
        }
        // Preview requires --type.
        assert!(Cli::try_parse_from(["fastio", "fileshare", "preview", "fs1"]).is_err());
        match parse(&["fastio", "fileshare", "preview", "fs1", "--type", "pdf"]) {
            FileshareCommands::Preview {
                fileshare_id,
                preview_type,
                ..
            } => {
                assert_eq!(fileshare_id, "fs1");
                assert_eq!(preview_type, "pdf");
            }
            other => panic!("expected Preview, got {other:?}"),
        }
    }

    #[test]
    fn upload_activity_wstoken_parse() {
        match parse(&[
            "fastio",
            "fileshare",
            "upload",
            "fs1",
            "./new.bin",
            "--if-version",
            "v3",
            "--name",
            "new.bin",
            "--yes",
        ]) {
            FileshareCommands::Upload {
                fileshare_id,
                file,
                if_version,
                name,
                yes,
                ..
            } => {
                assert_eq!(fileshare_id, "fs1");
                assert_eq!(file, "./new.bin");
                assert_eq!(if_version.as_deref(), Some("v3"));
                assert_eq!(name.as_deref(), Some("new.bin"));
                assert!(yes);
            }
            other => panic!("expected Upload, got {other:?}"),
        }
        // upload requires a file positional.
        assert!(Cli::try_parse_from(["fastio", "fileshare", "upload", "fs1"]).is_err());

        assert!(matches!(
            parse(&[
                "fastio",
                "fileshare",
                "activity",
                "fs1",
                "--wait",
                "30",
                "--updated",
            ]),
            FileshareCommands::Activity { .. }
        ));
        match parse(&[
            "fastio",
            "fileshare",
            "ws-token",
            "fs1",
            "--token-file",
            "/tmp/tok",
        ]) {
            FileshareCommands::WsToken {
                fileshare_id,
                token_file,
            } => {
                assert_eq!(fileshare_id, "fs1");
                assert_eq!(
                    token_file.as_deref(),
                    Some(std::path::Path::new("/tmp/tok"))
                );
            }
            other => panic!("expected WsToken, got {other:?}"),
        }
    }

    #[test]
    fn debug_redacts_password_values() {
        // The manual Debug impl must NEVER render a password value.
        let c = parse(&[
            "fastio",
            "fileshare",
            "create",
            "--workspace",
            "ws1",
            "--node",
            "n1",
            "--password",
            "super-secret-pw",
        ]);
        let dbg = format!("{c:?}");
        assert!(
            !dbg.contains("super-secret-pw"),
            "Debug must not leak the password: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "Debug must show the redaction marker: {dbg}"
        );
        // A present-vs-absent distinction is still legible.
        let none = parse(&["fastio", "fileshare", "info", "fs1"]);
        assert!(format!("{none:?}").contains("None"));
    }
}

#[cfg(test)]
mod share_debug_tests {
    use super::{Cli, Commands, ShareCommands};
    use clap::Parser;

    /// Helper: parse argv into a [`ShareCommands`], panicking on a parse error
    /// with the clap message (so the cause is visible).
    fn parse(args: &[&str]) -> ShareCommands {
        let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("parse failed: {e}"));
        match cli.command {
            Commands::Share(c) => c,
            other => panic!("expected Share, got {other:?}"),
        }
    }

    #[test]
    fn debug_redacts_password_values() {
        // The manual Debug impl must NEVER render a password value, on any of
        // the three password-bearing variants (Create/Update/PasswordAuth).
        const SECRET: &str = "super-secret-pw";

        let create = parse(&[
            "fastio",
            "share",
            "create",
            "My Share",
            "--workspace",
            "ws1",
            "--password",
            SECRET,
        ]);
        let dbg = format!("{create:?}");
        assert!(
            !dbg.contains(SECRET),
            "Create Debug must not leak the password: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "Create Debug must show the redaction marker: {dbg}"
        );

        let update = parse(&["fastio", "share", "update", "sh1", "--password", SECRET]);
        let dbg = format!("{update:?}");
        assert!(
            !dbg.contains(SECRET),
            "Update Debug must not leak the password: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "Update Debug must show the redaction marker: {dbg}"
        );

        let auth = parse(&["fastio", "share", "password-auth", "sh1", SECRET]);
        let dbg = format!("{auth:?}");
        assert!(
            !dbg.contains(SECRET),
            "PasswordAuth Debug must not leak the password: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "PasswordAuth Debug must show the redaction marker: {dbg}"
        );

        // The leak path is `Cli` Debug → command: assert the full render is
        // also clean (this is the surface a panic/log would actually print).
        let cli = Cli::try_parse_from(["fastio", "share", "password-auth", "sh1", SECRET])
            .expect("password-auth should parse");
        assert!(
            !format!("{cli:?}").contains(SECRET),
            "Cli Debug must not leak the share password through the command field"
        );

        // A present-vs-absent distinction is still legible on the Options.
        let none = parse(&["fastio", "share", "update", "sh1", "--title", "New"]);
        assert!(format!("{none:?}").contains("None"));
    }
}

#[cfg(test)]
mod lock_org_debug_tests {
    use super::{Cli, Commands, FilesCommands};
    use clap::Parser;

    /// Parse argv into a [`Cli`], panicking on a parse error with the clap
    /// message (so the cause is visible).
    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    #[test]
    fn file_lock_release_debug_redacts_token() {
        // The manual Debug must NEVER render the capability lock_token, on the
        // enum directly OR through the full `Cli` Debug path (the surface a
        // panic/log would actually print).
        const SECRET: &str = "lock-tok-SECRET";
        let parsed = cli(&[
            "fastio",
            "files",
            "lock",
            "release",
            "--workspace",
            "ws1",
            "n1",
            "--lock-token",
            SECRET,
        ]);
        let inner = match &parsed.command {
            Commands::Files(FilesCommands::Lock(c)) => c,
            other => panic!("expected Files(Lock), got {other:?}"),
        };
        let dbg = format!("{inner:?}");
        assert!(!dbg.contains(SECRET), "lock_token must not leak: {dbg}");
        assert!(
            dbg.contains("<redacted>"),
            "must show the redaction marker: {dbg}"
        );
        let full = format!("{parsed:?}");
        assert!(
            !full.contains(SECRET),
            "Cli Debug must not leak the lock_token: {full}"
        );
        assert!(full.contains("<redacted>"));
    }

    #[test]
    fn file_lock_acquire_debug_redacts_client_info() {
        const INFO: &str = "device-fingerprint-SECRET";
        let parsed = cli(&[
            "fastio",
            "files",
            "lock",
            "acquire",
            "--workspace",
            "ws1",
            "n1",
            "--client-info",
            INFO,
        ]);
        let full = format!("{parsed:?}");
        assert!(!full.contains(INFO), "client_info must not leak: {full}");
        assert!(
            full.contains("Some(<redacted>)"),
            "a present client_info must show the redaction marker: {full}"
        );
        // A present-vs-absent distinction is still legible.
        let none = cli(&[
            "fastio",
            "files",
            "lock",
            "acquire",
            "--workspace",
            "ws1",
            "n1",
        ]);
        assert!(format!("{none:?}").contains("client_info: None"));
    }

    #[test]
    fn lock_release_and_heartbeat_debug_redact_token() {
        // Both the `lock release` and `lock heartbeat` variants carry the
        // capability lock_token and must redact it.
        const SECRET: &str = "lock-tok-SECRET";
        for action in ["release", "heartbeat"] {
            let parsed = cli(&[
                "fastio",
                "lock",
                action,
                "--context-id",
                "ws1",
                "n1",
                "--lock-token",
                SECRET,
            ]);
            let inner = match &parsed.command {
                Commands::Lock(c) => c,
                other => panic!("expected Lock, got {other:?}"),
            };
            let dbg = format!("{inner:?}");
            assert!(
                !dbg.contains(SECRET),
                "{action} lock_token must not leak: {dbg}"
            );
            assert!(
                dbg.contains("<redacted>"),
                "{action} must show the redaction marker: {dbg}"
            );
            assert!(
                !format!("{parsed:?}").contains(SECRET),
                "Cli Debug must not leak the {action} lock_token"
            );
        }
    }

    #[test]
    fn lock_acquire_debug_redacts_client_info() {
        const INFO: &str = "device-fingerprint-SECRET";
        let parsed = cli(&[
            "fastio",
            "lock",
            "acquire",
            "--context-id",
            "ws1",
            "n1",
            "--client-info",
            INFO,
        ]);
        let full = format!("{parsed:?}");
        assert!(!full.contains(INFO), "client_info must not leak: {full}");
        assert!(
            full.contains("Some(<redacted>)"),
            "a present client_info must show the redaction marker: {full}"
        );
        let none = cli(&["fastio", "lock", "acquire", "--context-id", "ws1", "n1"]);
        assert!(format!("{none:?}").contains("client_info: None"));
    }

    #[test]
    fn org_transfer_claim_debug_redacts_token() {
        // The bearer transfer-claim token (grants org-ownership claim) must
        // never render verbatim, on the enum or through the full Cli Debug path.
        const SECRET: &str = "transfer-bearer-SECRET";
        let parsed = cli(&["fastio", "org", "transfer-claim", SECRET]);
        let inner = match &parsed.command {
            Commands::Org(c) => c,
            other => panic!("expected Org, got {other:?}"),
        };
        let dbg = format!("{inner:?}");
        assert!(
            !dbg.contains(SECRET),
            "transfer-claim token must not leak: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "must show the redaction marker: {dbg}"
        );
        let full = format!("{parsed:?}");
        assert!(
            !full.contains(SECRET),
            "Cli Debug must not leak the transfer-claim token: {full}"
        );
        assert!(full.contains("<redacted>"));

        // A non-secret OrgCommands variant still renders its fields normally.
        let listed = cli(&["fastio", "org", "info", "1234567890123456789"]);
        assert!(format!("{listed:?}").contains("1234567890123456789"));
    }

    // `OrgCommands` is a large hand-written Debug; this guards against a
    // formatting regression on a non-secret field while we are here.
    #[test]
    fn org_create_workspace_debug_renders_fields() {
        let parsed = cli(&[
            "fastio",
            "org",
            "create-workspace",
            "1234567890123456789",
            "My Workspace",
        ]);
        let dbg = format!("{parsed:?}");
        assert!(dbg.contains("CreateWorkspace"));
        assert!(dbg.contains("My Workspace"));
    }
}

#[cfg(test)]
mod auth_user_secret_debug_tests {
    use super::{Cli, Commands};
    use clap::Parser;

    /// Helper: parse argv into a [`Cli`], panicking on a parse error with the
    /// clap message (so the cause is visible).
    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    /// `auth signup --password` must never render the password verbatim — on the
    /// `AuthCommands` enum or through the full `Cli` Debug tree — while the
    /// non-secret `agent` flag still renders.
    #[test]
    fn auth_signup_debug_redacts_password_renders_agent() {
        const SECRET: &str = "signup-pw-SECRET";
        let parsed = cli(&[
            "fastio",
            "auth",
            "signup",
            "--email",
            "a@b.c",
            "--password",
            SECRET,
            "--agent",
        ]);
        let inner = match &parsed.command {
            Commands::Auth(c) => format!("{c:?}"),
            other => panic!("expected Auth, got {other:?}"),
        };
        assert!(!inner.contains(SECRET), "password must not leak: {inner}");
        assert!(inner.contains("[REDACTED]"), "must redact: {inner}");
        assert!(
            inner.contains("agent: true"),
            "non-secret agent flag must render: {inner}"
        );
        let full = format!("{parsed:?}");
        assert!(
            !full.contains(SECRET),
            "Cli Debug must not leak the password: {full}"
        );
    }

    /// `user update --password/--current-password` must redact both secrets on
    /// the `UserCommands` enum and through the full `Cli` Debug tree, while a
    /// non-secret field (phone) still renders.
    #[test]
    fn user_update_debug_redacts_password_secrets() {
        const NEW_PW: &str = "new-pw-SECRET";
        const CUR_PW: &str = "current-pw-SECRET";
        let parsed = cli(&[
            "fastio",
            "user",
            "update",
            "--password",
            NEW_PW,
            "--current-password",
            CUR_PW,
            "--phone-country",
            "1",
            "--phone-number",
            "5551234567",
        ]);
        let full = format!("{parsed:?}");
        assert!(!full.contains(NEW_PW), "new password must not leak: {full}");
        assert!(
            !full.contains(CUR_PW),
            "current password must not leak: {full}"
        );
        assert!(full.contains("[REDACTED]"), "must redact: {full}");
        // Non-secret phone fields still render.
        assert!(full.contains("5551234567"), "phone must render: {full}");
    }

    /// `user email-change request --current-password` and `... confirm --token`
    /// must redact their secrets through the full `Cli` Debug tree.
    #[test]
    fn user_email_change_debug_redacts_secrets() {
        const CUR_PW: &str = "ec-current-pw-SECRET";
        const TOKEN: &str = "ec-confirm-token-SECRET";
        let req = cli(&[
            "fastio",
            "user",
            "email-change",
            "request",
            "--new-email",
            "new@example.com",
            "--current-password",
            CUR_PW,
        ]);
        let req_dbg = format!("{req:?}");
        assert!(
            !req_dbg.contains(CUR_PW),
            "current password must not leak: {req_dbg}"
        );
        assert!(req_dbg.contains("[REDACTED]"), "must redact: {req_dbg}");
        // The non-secret new email still renders.
        assert!(
            req_dbg.contains("new@example.com"),
            "new email must render: {req_dbg}"
        );

        let conf = cli(&[
            "fastio",
            "user",
            "email-change",
            "confirm",
            "--token",
            TOKEN,
        ]);
        let conf_dbg = format!("{conf:?}");
        assert!(
            !conf_dbg.contains(TOKEN),
            "confirmation token must not leak: {conf_dbg}"
        );
        assert!(conf_dbg.contains("[REDACTED]"), "must redact: {conf_dbg}");
    }

    /// `auth 2fa disable --token` and `auth 2fa verify-setup --token` carry
    /// one-time auth tokens that must be redacted through the full `Cli` Debug
    /// tree (`AuthCommands` delegates to the manual `TwoFaCommands` Debug). The
    /// non-secret `Setup --channel` flag must still render.
    #[test]
    fn two_fa_token_debug_redacts_through_full_cli() {
        const DISABLE_TOKEN: &str = "2fa-disable-token-SECRET";
        const SETUP_TOKEN: &str = "2fa-verify-setup-token-SECRET";
        let disable = cli(&["fastio", "auth", "2fa", "disable", "--token", DISABLE_TOKEN]);
        let disable_dbg = format!("{disable:?}");
        assert!(
            !disable_dbg.contains(DISABLE_TOKEN),
            "2fa disable token must not leak: {disable_dbg}"
        );
        assert!(
            disable_dbg.contains("[REDACTED]"),
            "must redact: {disable_dbg}"
        );

        let verify_setup = cli(&[
            "fastio",
            "auth",
            "2fa",
            "verify-setup",
            "--token",
            SETUP_TOKEN,
        ]);
        let setup_dbg = format!("{verify_setup:?}");
        assert!(
            !setup_dbg.contains(SETUP_TOKEN),
            "2fa verify-setup token must not leak: {setup_dbg}"
        );
        assert!(setup_dbg.contains("[REDACTED]"), "must redact: {setup_dbg}");

        // Non-secret 2FA channel still renders.
        let setup = cli(&["fastio", "auth", "2fa", "setup", "--channel", "totp"]);
        let setup_chan_dbg = format!("{setup:?}");
        assert!(
            setup_chan_dbg.contains("totp"),
            "non-secret 2fa channel must render: {setup_chan_dbg}"
        );
    }

    /// `auth 2fa verify --code` carries a one-time 2FA code that must be redacted
    /// through the full `Cli` Debug tree.
    #[test]
    fn two_fa_verify_code_debug_redacts_through_full_cli() {
        const CODE: &str = "2fa-verify-code-SECRET";
        let parsed = cli(&["fastio", "auth", "2fa", "verify", "--code", CODE]);
        let dbg = format!("{parsed:?}");
        assert!(!dbg.contains(CODE), "2fa verify code must not leak: {dbg}");
        assert!(dbg.contains("[REDACTED]"), "must redact: {dbg}");
    }

    /// `auth password-reset <code>` and `auth password-reset-check <code>` carry
    /// one-time reset codes that must be redacted through the full `Cli` Debug
    /// tree.
    #[test]
    fn auth_password_reset_code_debug_redacts() {
        const RESET_CODE: &str = "pw-reset-code-SECRET";
        const CHECK_CODE: &str = "pw-reset-check-code-SECRET";
        let reset = cli(&[
            "fastio",
            "auth",
            "password-reset",
            RESET_CODE,
            "--new-password",
            "np",
            "--confirm-password",
            "np",
        ]);
        let reset_dbg = format!("{reset:?}");
        assert!(
            !reset_dbg.contains(RESET_CODE),
            "password-reset code must not leak: {reset_dbg}"
        );
        assert!(reset_dbg.contains("[REDACTED]"), "must redact: {reset_dbg}");

        let check = cli(&["fastio", "auth", "password-reset-check", CHECK_CODE]);
        let check_dbg = format!("{check:?}");
        assert!(
            !check_dbg.contains(CHECK_CODE),
            "password-reset-check code must not leak: {check_dbg}"
        );
        assert!(check_dbg.contains("[REDACTED]"), "must redact: {check_dbg}");
    }

    /// `auth verify --email --code` carries a one-time verification code that
    /// must be redacted through the full `Cli` Debug tree, while the non-secret
    /// email still renders.
    #[test]
    fn auth_verify_code_debug_redacts_renders_email() {
        const CODE: &str = "auth-verify-code-SECRET";
        let parsed = cli(&[
            "fastio",
            "auth",
            "verify",
            "--email",
            "v@example.com",
            "--code",
            CODE,
        ]);
        let dbg = format!("{parsed:?}");
        assert!(!dbg.contains(CODE), "verify code must not leak: {dbg}");
        assert!(dbg.contains("[REDACTED]"), "must redact: {dbg}");
        assert!(
            dbg.contains("v@example.com"),
            "non-secret email must render: {dbg}"
        );
    }
}
