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
// This comment is the single documented reference for the recipe.
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
    /// about your content and generate AI shares. To find things, use `fastio
    /// search` instead. (The former `ai` group; `ai` still works as a hidden
    /// alias.)
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
    /// App installations registered to your account.
    #[command(subcommand)]
    Apps(AppsCommands),
    /// Cloud import and sync.
    #[command(subcommand)]
    Import(ImportCommands),
    /// Agent Intents — announce what you are doing so peers see it before
    /// they collide.
    ///
    /// Short-lived, workspace-scoped, and deliberately NOT memory: the content
    /// is agent-authored and untrusted. Allocate EARLY — a slot is occupancy,
    /// so taking one before you know your topic is the intended use, not a
    /// placeholder.
    #[command(subcommand)]
    Intents(IntentsCommands),

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

    /// Metadata extraction and search.
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
        /// Optional comma-separated allow-list of tools to enable (default: all).
        /// Only the named tools are advertised and callable; unknown names are
        /// warned about and ignored, and an all-unknown list is rejected.
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

/// The search-mode flags shared by every command that searches files.
///
/// Flattened into `files search`, `search workspace`, `search share`, and
/// `workspace search` so the surface cannot drift between them. Every flag is
/// optional; passing none reproduces today's behavior exactly
/// (`search_in=both`, `name_match=auto`).
///
/// `--filename-only` / `--content-only` / `--glob` are CLI-only shorthands for
/// the canonical `--search-in` / `--name-match` values — they never appear on
/// the wire as anything else.
// The four bools are independent command-line switches, not a state machine:
// clap models a `--flag` as a bool, and collapsing them into enums would remove
// the shorthands rather than clarify them. Conflicts are enforced declaratively
// by `conflicts_with`, and `to_params` resolves them to two wire values.
#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args, Debug, Clone, Default)]
pub struct SearchModeArgs {
    /// Where to search: filename, content, or both [default: both].
    ///
    /// `content` searches the AI's understanding of the file (its generated
    /// summary plus semantic matches) — it is NOT a raw-text grep. `filename`
    /// is the literal, pattern-driven mode.
    #[arg(
        long,
        value_parser = ["filename", "content", "both"],
        conflicts_with_all = ["filename_only", "content_only"],
    )]
    pub search_in: Option<String>,

    /// Search filenames only (shorthand for --search-in filename).
    #[arg(long, conflicts_with = "content_only")]
    pub filename_only: bool,

    /// Search file contents only (shorthand for --search-in content).
    #[arg(long)]
    pub content_only: bool,

    /// How the filename is matched [default: auto].
    ///
    /// `auto` is today's layered relevance ranking. `exact`, `prefix`,
    /// `contains`, and `glob` match literally — under those modes `*` and `?`
    /// are ordinary characters except in `glob`, where they are wildcards.
    #[arg(
        long,
        value_parser = ["auto", "exact", "prefix", "contains", "glob"],
        conflicts_with = "glob",
    )]
    pub name_match: Option<String>,

    /// Treat the query as a shell-style glob (shorthand for --name-match glob).
    ///
    /// `*` matches any run of characters and `?` exactly one, against the whole
    /// filename including spaces — e.g. `'Quarterly*.pdf'`, `'report-*.xlsx'`.
    #[arg(long)]
    pub glob: bool,

    /// Match filenames case-sensitively (applies to the precise modes only).
    #[arg(long)]
    pub case_sensitive: bool,
}

impl SearchModeArgs {
    /// Resolve the flags — canonical and shorthand — into the wire parameters.
    #[must_use]
    pub fn to_params(&self) -> fastio_cli::api::types::SearchModeParams {
        let search_in = if self.filename_only {
            Some("filename")
        } else if self.content_only {
            Some("content")
        } else {
            self.search_in.as_deref()
        };
        let name_match = if self.glob {
            Some("glob")
        } else {
            self.name_match.as_deref()
        };
        fastio_cli::api::types::SearchModeParams::new()
            .search_in(search_in)
            .name_match(name_match)
            // Only sent when explicitly requested: `false` is the server
            // default, so an absent flag must leave the request untouched.
            .case_sensitive(self.case_sensitive.then_some(true))
    }
}

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
        /// Add extracted metadata facts to file/note items in the files bucket
        /// (workspace only; ignored on shares).
        #[arg(long)]
        details: bool,
        /// Search-mode flags. These scope the FILES bucket only — the metadata
        /// and comments buckets ignore them.
        #[command(flatten)]
        modes: SearchModeArgs,
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
        /// Add extracted metadata facts to file/note items in the files bucket
        /// (workspace only — a share accepts the flag and never returns facts).
        #[arg(long)]
        details: bool,
        /// Search-mode flags. These scope the FILES bucket only.
        #[command(flatten)]
        modes: SearchModeArgs,
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
        ///
        /// A field may carry a `validation` object constraining the signer's
        /// input: `min_length` / `max_length` / `pattern` on text-style fields,
        /// `date_min` / `date_max` on date fields. It is rejected server-side on
        /// signature / initial / checkbox fields.
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
        ///
        /// A field may carry a `validation` object constraining the signer's
        /// input: `min_length` / `max_length` / `pattern` on text-style fields,
        /// `date_min` / `date_max` on date fields. It is rejected server-side on
        /// signature / initial / checkbox fields.
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
        /// `hlsstream`). Passed through to the server verbatim, so an
        /// unrecognized value reaches the API rather than being caught here —
        /// the documented set is bin, thumbnail, image, hlsstream, pdf,
        /// spreadsheet, audio, mp4. Note `hlsstream` has no underscore.
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
        ///
        /// Pass the version you actually READ. Take it from a pinned download
        /// (`download file --version <id>`), not from a separate lookup: an
        /// unpinned read can return different bytes than the version you name
        /// here, and the server checks only that the id is current. A base that
        /// is present but wrong PASSES the check and overwrites the very change
        /// this flag exists to protect — worse than omitting it, which at least
        /// fails visibly.
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
        /// Label this agent instance on the resulting credential (browser/PKCE
        /// login only). Defaults to `$FASTIO_AGENT_NAME`.
        ///
        /// The OAuth client id is a fixed constant, so without this every agent
        /// signing in through the CLI shares one identity. Set a distinct name
        /// per agent process to keep them separable.
        #[arg(long)]
        agent_name: Option<String>,
    },
    /// Clear stored credentials for the current profile (local only).
    Logout,
    /// Sign out server-side: invalidate every revocable (browser) session token,
    /// then clear local credentials. Best-effort when the stored credential is
    /// already dead (revoked key, lapsed or expired session): local credentials
    /// are still cleared, reported via `server_signout_completed: false`. A 401
    /// for a `--token`/env bearer or under a foreign `--api-base` stays fatal —
    /// stored credentials are never wiped on another credential's behalf.
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
    /// [deprecated] Always succeeds — this is NOT an availability check.
    ///
    /// Per the published API docs, the endpoint no longer does any account
    /// lookup and returns a uniform 202 / `result: true` for any well-formed
    /// address, so a success here says nothing about whether the email is
    /// registered. It exists only so existing callers keep receiving a success
    /// response. To handle an already-registered email, just call signup — it
    /// notifies the existing account and returns the same success as a new
    /// signup.
    #[command(name = "email-check")]
    EmailCheck {
        /// Email to check. Any well-formed address returns success.
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
        /// Agent or application name for tracking (max 128 characters).
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
        /// New agent or application name (max 128 characters; empty string clears).
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
        /// New device name (max 128 characters; empty string clears to null).
        #[arg(long = "device-name")]
        device_name: Option<String>,
        /// New agent name (max 128 characters; empty string clears to null).
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
/// verbatim through the `Cli` Debug tree (secrets must never appear in
/// Debug output).
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
    /// Close (soft-delete) this account. IRREVERSIBLE — rehearse with --dryrun.
    Close {
        /// Your own email address. The server requires it to match the
        /// account's current address; it is the confirmation check, not a
        /// free-form confirmation word.
        email_address: String,
        /// Check eligibility WITHOUT closing the account.
        #[arg(long)]
        dryrun: bool,
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
/// through the `Cli` Debug tree (secrets must never appear in Debug output).
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
/// verbatim through the `Cli` Debug tree (secrets must never appear in
/// Debug output).
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
        /// Show ARCHIVED workspaces instead of active ones (server default: false).
        #[arg(long)]
        archived: Option<bool>,
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
        /// AI indexing on the new workspace. Omit to take the platform default,
        /// which is ON; pass `--intelligence false` to opt out.
        ///
        /// Bare `--intelligence` still means true, so the flag keeps working the
        /// way it always has — the change is that OMITTING it no longer sends
        /// `false`.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        intelligence: Option<bool>,
        /// Automatic metadata extraction for newly uploaded files. An OPT-OUT
        /// layered under --intelligence: omit to take the platform default,
        /// which is ON, and pass `--metadata-extraction false` to withhold
        /// automatic extraction. Explicit per-file extraction requests are
        /// unaffected.
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        metadata_extraction: Option<bool>,
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
            // Redact the bearer transfer-claim token.
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
                archived,
            } => f
                .debug_struct("Workspaces")
                .field("org_id", org_id)
                .field("limit", limit)
                .field("offset", offset)
                .field("archived", archived)
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
                metadata_extraction,
            } => f
                .debug_struct("CreateWorkspace")
                .field("org_id", org_id)
                .field("name", name)
                .field("folder_name", folder_name)
                .field("description", description)
                .field("perm_join", perm_join)
                .field("perm_member_manage", perm_member_manage)
                .field("intelligence", intelligence)
                .field("metadata_extraction", metadata_extraction)
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
        /// Email address (sends an invitation) OR a 19-digit user ID (adds the
        /// existing user directly).
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
        /// Show ARCHIVED workspaces instead of active ones.
        ///
        /// Requires `--org` — the filter is only accepted on the org-scoped
        /// listing (see the published API docs), where the server defaults it
        /// to false. Without `--org` the unfiltered listing already returns
        /// every workspace with an `archived` flag on each row.
        #[arg(long, requires = "org")]
        archived: Option<bool>,
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
        /// Automatic metadata extraction for newly uploaded files. An OPT-OUT
        /// layered under --intelligence: omit to take the platform default,
        /// which is on, and pass `false` to withhold automatic extraction.
        /// Explicit per-file extraction requests are unaffected.
        #[arg(long)]
        metadata_extraction: Option<bool>,
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
        /// Automatic metadata extraction for newly uploaded files. An OPT-OUT
        /// layered under --intelligence: it can withhold extraction, never
        /// enable it where intelligence or the plan does not allow it. Unlike
        /// --intelligence it deletes nothing and is not rate-limited. Explicit
        /// per-file extraction requests are unaffected.
        #[arg(long)]
        metadata_extraction: Option<bool>,
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
    /// List background jobs — active AND recently finished (poll after an
    /// async metadata extract).
    ///
    /// Finished jobs stay listed for about an hour, which is what makes
    /// polling work: a terminal `completed` / `errored` is observable rather
    /// than vanishing at the moment it matters. Absence is genuinely
    /// ambiguous and its meaning depends on elapsed time — shortly after an
    /// enqueue it means "not visible yet", and long after a finish it means
    /// "aged out, result unknown". Neither is a terminal state, so poll for
    /// an explicit one rather than inferring anything from a missing entry.
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
        /// Search-mode flags (files bucket only).
        #[command(flatten)]
        modes: SearchModeArgs,
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
    /// Accept ONE invitation, or every pending invitation when no id is given.
    ///
    /// Per-id accept is supported, through the same route `decline` uses, so
    /// passing an id accepts exactly that one invitation and nothing else.
    Accept {
        /// Invitation ID. Omit to accept ALL pending invitations.
        #[arg(requires_all = ["entity_type", "entity_id"])]
        invitation_id: Option<String>,
        /// Entity type: workspace or share. Required with an invitation ID.
        #[arg(long, value_parser = ["workspace", "share"])]
        entity_type: Option<String>,
        /// Entity ID. Required with an invitation ID.
        #[arg(long)]
        entity_id: Option<String>,
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
    /// Accept or decline ONE invitation using the key from its invite email.
    ///
    /// The `/user/invitations/` surface `accept` uses has no per-id accept, so
    /// this key-based route is the only way to act on a single invitation.
    Join {
        /// Entity type: workspace, share, or org.
        ///
        /// `org` is valid here and NOT on `decline`/`delete`: the key-based join
        /// route is documented for organizations, while the
        /// `/user/invitations/` state updates those commands use are documented
        /// for workspace and share only.
        #[arg(value_parser = ["workspace", "share", "org"])]
        entity_type: String,
        /// Entity ID (19-digit).
        entity_id: String,
        /// Invitation key from the invite link.
        invitation_key: String,
        /// Whether to accept or decline.
        #[arg(value_parser = ["accept", "decline"])]
        action: String,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// One or more storage node IDs (positional).
        #[arg(required = true, num_args = 1..)]
        node_ids: Vec<String>,
    },
    /// Create a new folder.
    #[command(name = "create-folder")]
    CreateFolder {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID to move.
        node_id: String,
        /// Destination folder node ID.
        #[arg(long)]
        to: String,
    },
    /// Copy a file or folder.
    Copy {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID to copy.
        node_id: String,
        /// Destination folder node ID.
        #[arg(long)]
        to: String,
    },
    /// Rename a file or folder.
    Rename {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Compare-and-swap precondition: the version id this update was
        /// derived from. The server rejects the update when that is no longer
        /// current, so a concurrent writer's change is not silently replaced.
        ///
        /// Mirrors `fileshare upload --if-version`. Pass the version you
        /// actually READ — take it from a pinned download
        /// (`download file --version <id>`), never from a separate lookup: the
        /// server checks only that the id is CURRENT, so a base that is present
        /// but wrong passes the check and overwrites the change this flag
        /// exists to protect.
        ///
        /// Two traps when rebasing: `node.version` is ABSENT from the `terse`
        /// output tier, so do not source a base from `--detail terse`; and an
        /// identical-byte replace is a NO-OP that does NOT advance the version,
        /// so a successful write is not proof the version moved.
        ///
        /// A stale base is rejected with a `conflict_version_mismatch` carrying
        /// the current version id. Before re-applying, check whether your change
        /// is already present: a retried write-back can report a conflict naming
        /// your own committed write.
        #[arg(long)]
        if_version: Option<String>,
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
        ///
        /// **You send `hash_type` and you read `hash_algo` back.** The request
        /// field is `hash_type`; the same value is returned on the node as
        /// `hash_algo`, so searching a response for `hash_type` finds nothing
        /// and that absence says nothing about the file. The asymmetry is
        /// deliberate — the API translates one to the other on write. (Request
        /// spelling verified here against the documented content-source shape;
        /// the translation itself is not independently verified by this CLI.)
        #[arg(long, value_parser = ["md5", "sha1", "sha256", "sha384"], requires = "hash")]
        hash_type: Option<String>,
    },
    /// Delete a file or folder (move to trash).
    ///
    /// **If the node carries `import_state`, this may also delete the copy at
    /// the cloud provider.** A file inside a two-way cloud-sync graft is
    /// mirrored, so removing it here can remove it there — and that includes a
    /// node you MOVED OUT of the graft, because a move does not clear the
    /// import marker (measured; see `import refresh`).
    ///
    /// **NOT INDEPENDENTLY VERIFIED BY THIS CLI.** It is stated because the
    /// costs are lopsided: if it
    /// is right, checking saves a file at the provider; if it is wrong, you
    /// checked for nothing. **When the provider's copy matters, look at
    /// `files info <node> --workspace <id>` first, and if `import_state` is
    /// present, verify at the provider after deleting.**
    ///
    /// **The shape that actually catches people is a duplicate.** Move a file
    /// out of a graft and the sync re-imports the provider's copy as a new node
    /// (measured — see `import refresh`), so you end up with two: the stray you
    /// moved, and a fresh one back inside the graft. Both still carry an import
    /// marker, and the tidy-up instinct is to delete one.
    ///
    /// **The obvious reading is backwards.** (Guard dated 2026-08-23; not
    /// independently verified by this CLI): the delete
    /// write-back resolves a remote path from the trashed node's ORIGINAL
    /// parent, and only proceeds when that lands under the graft. So the
    /// moved-out stray resolves to nothing and is REFUSED — nothing reaches the
    /// provider — while **the copy sitting inside the graft, including the
    /// re-imported duplicate, normally resolves and PROPAGATES.** The
    /// tidy-looking one is the dangerous one.
    ///
    /// **The gate turns on whether the path RESOLVES, not on where the node
    /// sits**, and a failure to resolve is reported to conflate "not under this
    /// graft" with an ancestor that could not be loaded and with a depth bound
    /// being hit. So a refusal is not proof of anything about the node's
    /// location, and location is not a promise about what the delete will do.
    ///
    /// **Do not lean on that direction blind.** It rests on a guard that is
    /// recent and environment-specific, and where the guard is absent the older
    /// behaviour — a moved-out delete reaching the provider — is what applies.
    /// **Confirm at the provider before deleting either copy.** Being
    /// over-cautious costs you a duplicate left in place; being wrong costs
    /// someone's cloud file.
    ///
    /// **Afterwards, `import list-writebacks` tells you which way it went.**
    /// The gate is reported to sit immediately before the write-back is
    /// enqueued, so a delete that passed leaves a row and one that was refused
    /// leaves none. Measured here: rows appear on that listing promptly — an
    /// upload took the source from 26 to 27 within seconds. **This is a
    /// post-mortem, not a safety check** — it answers what already happened,
    /// and by then the provider's copy is gone or it is not.
    Delete {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID to delete.
        node_id: String,
    },
    /// Restore a file or folder from trash.
    Restore {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID to restore.
        node_id: String,
    },
    /// Permanently delete a trashed file or folder.
    Purge {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID to permanently delete.
        node_id: String,
    },
    /// List items in the trash.
    Trash {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID.
        node_id: String,
    },
    /// Search for files in a workspace or share by filename, contents, or both.
    ///
    /// Filename matching is literal and pattern-driven (see --name-match /
    /// --glob). Content matching covers the AI-generated summary plus semantic
    /// results; instance intelligence gates the semantic half only, so
    /// previously summarized files stay searchable with intelligence off.
    ///
    /// This is the FLAT file list. For one query across files, metadata and
    /// comments together, use `fastio search workspace` / `fastio search share`.
    Search {
        /// Workspace ID. Mutually exclusive with --share; one is required.
        #[arg(long, conflicts_with = "share", required_unless_present = "share")]
        workspace: Option<String>,
        /// Share ID — search files in a share instead of a workspace.
        ///
        /// Workspace-backed (folder) shares do not support search and answer
        /// "Search is not available for Shared Folders". Note --filters is a
        /// workspace-only feature: a share ignores it and still answers 200, so
        /// a warning is printed when the response shows it did not run.
        #[arg(long)]
        share: Option<String>,
        /// Search query, or the pattern when a precise --name-match is used.
        query: String,
        /// Search-mode flags (filename / content / both, and how the filename
        /// is matched).
        #[command(flatten)]
        modes: SearchModeArgs,
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
        /// Narrow to files whose extracted metadata satisfies every clause
        /// (workspace only). A JSON array of `{"field","operator","value"}`
        /// objects, or `@path` to read it from a file. Operators: `=` `!=` `<`
        /// `<=` `>` `>=` `in` `exists` `not_exists` `confidence_gte` (`exists`
        /// / `not_exists` take no value). Combines with --scope (intersection);
        /// the server currently refuses it alongside --folder-scope.
        #[arg(long)]
        filters: Option<String>,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID. `add-link` is workspace-only — the published API docs
        /// list it under "Workspace-Only Features", and `share_id` below
        /// already means "the share to link IN", not the storage context.
        #[arg(long)]
        workspace: String,
        /// Parent folder node ID.
        parent: String,
        /// Share ID to link.
        share_id: String,
    },
    /// Transfer a node to another workspace.
    ///
    /// **If this fails with a 5xx, do NOT simply run it again — check the
    /// destination workspace first.** A lost acknowledgement looks exactly
    /// like a failure from here: the request may have been received and
    /// carried out with only the answer going missing, and this command
    /// cannot tell the two apart. Re-running it in that case is a SECOND
    /// transfer, not a retry of the first.
    ///
    /// **Reported from the platform side 2026-08-26 and NOT measured here:
    /// the endpoint carries no idempotency**, and for a FOLDER a repeat is
    /// said to produce a second copy of the whole tree alongside the first
    /// (`X`, `X (1)`, `X (2)`) rather than replacing it. Treat a 5xx here as
    /// "outcome unknown" and go and look; a generic hint about server
    /// trouble may appear beneath the error, and it is not advice to retry
    /// this particular command.
    Transfer {
        /// Workspace ID. Left workspace-only deliberately: `files transfer`
        /// is a known-broken, fenced item (wrong field name + missing required
        /// field, with a duplicate-tree hazard), and widening its context while
        /// that decision is open would muddy it.
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
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
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace).
        #[arg(long)]
        share: Option<String>,
        /// Node ID.
        node_id: String,
    },
    /// Read a file's extracted text as ordered chunks (the text the platform
    /// indexed for search and AI).
    ///
    /// `files read` returns the file's RAW BYTES; this returns the extracted
    /// TEXT with chunk addresses, so it is the way to read a PDF's words
    /// rather than its container.
    ///
    /// The unit is a chunk, not a page: a chunk can span two pages or split
    /// one, so --page returns whole chunks that OVERLAP that page and only
    /// works on formats the response reports as `page_addressable`. A chunk's
    /// address is its `position`.
    ///
    /// Pick at most ONE window: --query (relevance over this file's own
    /// chunks, full text, no byte budget), --page, or --chunk-from/--chunk-to
    /// (positions). With none, the read starts at the beginning of the file.
    /// Walk onwards with --cursor, passing the previous response's
    /// `next_cursor` verbatim; it is done when that is null.
    ///
    /// To locate and quote a passage: run `files search --detail standard`,
    /// take the hit's `best_chunk.position` P and `best_chunk.indexed_version_id`,
    /// read it here with `--chunk-from P-1 --chunk-to P+1`, then confirm the
    /// `indexed_version_id` in this response matches before quoting — if it
    /// differs the file was re-indexed and the search should be re-run. A hit
    /// whose `best_chunk.position` is null has no address (transcripts, or a
    /// passage the index could not resolve): read it with --query instead.
    ///
    /// Chunk verbosity follows the global --detail flag: `--detail terse`
    /// returns the chunk map (positions, page ranges, `chars`) with no text.
    ///
    /// `indexed: false` is a normal answer, not a failure: that version has no
    /// extracted text (yet, or ever).
    ///
    /// --nodes scores up to 10 files against one --query in a single request.
    /// It is WORKSPACE ONLY and its scores are comparable only within one
    /// file, never across files.
    Content {
        /// Workspace ID (omit when targeting a share).
        #[arg(long, required_unless_present = "share", conflicts_with = "share")]
        workspace: Option<String>,
        /// Share ID (alternative storage context to --workspace). Requires
        /// DOWNLOAD permission on the share, not merely view.
        #[arg(long)]
        share: Option<String>,
        /// Node ID of the single file or note to read. Omit when using --nodes.
        #[arg(required_unless_present = "nodes", conflicts_with = "nodes")]
        node_id: Option<String>,
        /// Score 1-10 files against --query in one request (comma-separated or
        /// repeated). WORKSPACE ONLY, and --query is required with it.
        // `value_delimiter` only — deliberately NOT `num_args = 1..`, which
        // would also accept space-separated values and let this flag swallow
        // the NODE_ID positional that follows it.
        #[arg(
            long,
            value_delimiter = ',',
            requires = "query",
            conflicts_with_all = ["share", "page", "chunk_from", "chunk_to", "cursor"],
        )]
        nodes: Option<Vec<String>>,
        /// Relevance mode: rank this file's own chunks against the query and
        /// return the best ones with full text and a score (max 512 chars).
        #[arg(long, conflicts_with_all = ["page", "chunk_from", "chunk_to", "cursor"])]
        query: Option<String>,
        /// Return the chunks overlapping this 1-based page (only where the
        /// response reports `page_addressable`).
        #[arg(long, conflicts_with_all = ["chunk_from", "chunk_to"])]
        page: Option<u32>,
        /// Start of an inclusive `position` range (below 10000).
        #[arg(long)]
        chunk_from: Option<u32>,
        /// End of that inclusive `position` range (requires --chunk-from).
        #[arg(long, requires = "chunk_from")]
        chunk_to: Option<u32>,
        /// Continue a walk: the previous response's `next_cursor`, verbatim.
        #[arg(long)]
        cursor: Option<String>,
        /// Chunks per response, 1-20 (per FILE with --nodes).
        #[arg(long)]
        limit: Option<u32>,
        /// UTF-8 byte budget over the returned text, 1024-262144 (per FILE
        /// with --nodes). Not applied in relevance mode.
        #[arg(long)]
        max_bytes: Option<u32>,
    },
}

/// File lock subcommands.
///
/// `Debug` is implemented manually (not derived) so the capability `lock_token`
/// and the free-form `client_info` are never rendered verbatim through the
/// `Cli` Debug tree (secrets must never appear in Debug output).
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
        /// Write the returned lock token to this path (created 0600). When
        /// omitted the token is redacted from output and a warning is printed.
        /// You need the token for `lock release --lock-token`.
        #[arg(long)]
        lock_token_file: Option<std::path::PathBuf>,
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
                lock_token_file,
            } => f
                .debug_struct("Acquire")
                .field("workspace", workspace)
                .field("node_id", node_id)
                .field("duration", duration)
                .field("client_info", &format_args!("{}", ci(client_info.as_ref())))
                // The PATH is not the secret; the token written there is, and it
                // never passes through this enum.
                .field("lock_token_file", lock_token_file)
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
    ///
    /// **Uploading into a GRAFTED folder is not the same as uploading into
    /// ordinary storage.** Each file must additionally be written back to the
    /// cloud provider, and write-backs serialise PER SOURCE — one at a time,
    /// however many files you sent. A batch of 200 does not push 200 files to
    /// the provider in parallel; it forms a queue.
    ///
    /// **Success here means the bytes reached Fastio, NOT the provider.** The
    /// upload response does NOT carry `import_state` — read the node instead:
    ///
    /// Read it with `fastio files info <node_id> --workspace <id>`.
    ///
    /// Measured 2026-08-22: `import_state` is present at `--detail standard`
    /// and `full`, and ABSENT at `terse`. This CLI defaults to the server's
    /// `full` shape, so you will normally see it without asking — but if you
    /// pass `--detail terse`, its absence means nothing about the file. Then
    /// read `import_state.status`:
    ///
    /// `completed` — the write-back ran; the bytes ARE at the provider.
    ///
    /// `pending` / `uploading` — still queued or in flight. Wait.
    ///
    /// `failed` / `conflict` / `canceled` — it never reached the provider.
    /// Re-upload it.
    ///
    /// **A stamp reading `pending` or `uploading` has THREE possible meanings,
    /// and this status alone cannot tell them apart.** All reported from the
    /// platform side, not measured here:
    ///
    /// - genuinely queued — a write-back behind a saturated workspace can sit
    ///   at `uploading` for HOURS with its timestamp frozen, and that is
    ///   healthy;
    /// - genuinely stranded — if the worker process dies the row is left with
    ///   no error set and nothing to move it;
    /// - **already finished.** The node stamp is NOT reconciled against a
    ///   write-back that has since completed, so it can outlive the work it
    ///   describes. A snapshot on 2026-08-25 found 21 such nodes, 18 of
    ///   them from write-backs that had **completed successfully** — bytes at
    ///   the provider while the status still said in-flight. Treat those counts
    ///   as one environment on one day; the shape is the durable part.
    ///
    /// **So when it matters, read the write-back ROW rather than the node
    /// stamp:** `import list-writebacks` shows the row's own status, and a
    /// terminal row beats an in-flight stamp. Re-uploading also remains safe
    /// whichever of the three you are in, if you would rather not diagnose it.
    ///
    /// Measured 2026-08-22, one file uploaded into a Dropbox graft on a
    /// `read_write` source: `pending` → `uploading` → `completed`, read back
    /// through the command above. The three failure values are NOT from that
    /// run — they were not provoked, so treat them as the documented set
    /// rather than as something this CLI has watched happen.
    ///
    /// **`synced` is NOT the success value for a file you uploaded, and it is
    /// NOT evidence the file came from the provider either.** `completed` is a
    /// STAMP; `synced` is the fall-through DEFAULT for a node carrying no
    /// stamped write-back status. **So do not script them as peer values: one
    /// is an assertion, the other is its absence.**
    ///
    /// A file YOU uploaded can read `synced` — the node is marked imported
    /// before the write-back is enqueued, so if that enqueue never happens or
    /// bails (coalesced against a live write-back, lock not acquired, job
    /// creation failing), the node ends up imported, unstamped, and therefore
    /// `synced`. **Reading it as success passes a file that was never pushed**,
    /// which is the danger — not the waiting. A stamped `completed`, by
    /// contrast, is sticky: it stays `completed` even after later provider-side
    /// changes pull new bytes over the node. *(Mechanism not independently
    /// verified here; what is measured here is that this CLI's own uploads
    /// reached `completed` and never showed `synced`.)*
    ///
    /// A file still queued is not stalled. Do not re-upload it, and do not
    /// reach for `import refresh` — that competes for the same per-source lock
    /// and makes the queue drain slower.
    ///
    /// **Watch `import_state.status` on the node — it is the per-file
    /// answer.** `import list-writebacks` gives the source-level queue view;
    /// note that the route behind it returns an empty list for a hyphenated
    /// source id (this CLI strips them, but a direct API call does not — see
    /// that command's help), and an empty array with a 200 reads exactly like
    /// "nothing outstanding".
    // A same-filename upload REPLACES the existing node — measured 2026-08-23:
    // a second `upload file` of `collide.txt` returned the SAME `new_file_id`,
    // left one node, and advanced its version. Documented on the
    // command itself because `--if-version` CANNOT guard it: a mint carries no
    // node id, so there is no referent for a version precondition.
    File {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// One or more local files to upload.
        ///
        /// A file whose name already exists in the destination folder REPLACES
        /// that node (a new version of it) rather than creating a second entry.
        /// `--if-version` cannot protect this: the request carries no node id,
        /// so there is nothing for a version precondition to refer to. Check the
        /// folder first if you must not overwrite.
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
        /// Filename to save as (1-255 chars). REQUIRED by the API — nothing
        /// derives it from the URL, so omitting it sends a request missing a
        /// required parameter.
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
        /// see the published API docs). Spelling is `canceled` (single `l`).
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
        ///
        /// Pin the read whenever the bytes will be edited and written back
        /// under a compare-and-swap precondition. Reading unpinned and then
        /// naming a version separately is unsafe: the two can disagree, and
        /// because the server only checks that the id is CURRENT, a base that
        /// is present but wrong passes the check and silently overwrites the
        /// intervening change.
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
        /// AI indexing. Omit to take the platform default, which is ON.
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
        /// Entity type: workspace, share, or fileshare.
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            crate::commands::comment::VALID_ENTITY_TYPES
        ))]
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
        /// Entity type: workspace, share, or fileshare.
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            crate::commands::comment::VALID_ENTITY_TYPES
        ))]
        entity_type: String,
        /// Entity ID (workspace or share ID).
        #[arg(long)]
        entity_id: String,
        /// Anchoring reference as a JSON object string (or `@file.json`), e.g.
        /// `{"type":"document","page":3}`.
        ///
        /// `type` is required and selects which anchor fields are valid:
        /// `document` (`page`, `text_snippet`) · `video`/`audio` (`timestamp`) ·
        /// `image` (region) · `general` (none). An unrecognized `type` is
        /// rejected — `page` is an ANCHOR FIELD, not a type.
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
        /// Entity type: workspace, share, or fileshare.
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            crate::commands::comment::VALID_ENTITY_TYPES
        ))]
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
        /// Entity type: workspace or share. `fileshare` parses but is rejected
        /// with guidance — a File Share comment always targets a specific node,
        /// so it has no container-scoped listing (see the published API docs).
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            crate::commands::comment::VALID_ENTITY_TYPES
        ))]
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
        /// Emoji to react with — a single emoji CHARACTER, e.g. 👍 or ❤️.
        ///
        /// Max 2 UTF-8 characters and it must match the Unicode emoji ranges.
        /// Shortcodes like `thumbsup` are NOT translated and are rejected by the
        /// server (`1605`: "Invalid emoji character" / "Only single emoji
        /// allowed"). One reaction per user per comment; re-reacting replaces.
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
    ///
    /// **When paging, stop on an EMPTY page — never on a short one.** A page
    /// smaller than `--limit` does not mean you reached the end here: rows you
    /// are not permitted to see are removed AFTER the page is cut, so a page
    /// can arrive part-empty with plenty more behind it. The usual "stop when
    /// fewer than limit come back" loop exits early and looks like it
    /// finished, which is worse than an error because nothing reports it.
    ///
    /// For the same reason, a small result is not a measure of how much
    /// happened. Narrowing with `--event` or `--category` spends the page on
    /// rows of the kind you asked for, rather than on whatever is newest.
    List {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Narrow by user profile ID (19-digit). **Do NOT read this as "events
        /// about this person" — it does not reliably identify anyone.**
        ///
        /// Reported from the platform side 2026-08-24 and NOT measurable here:
        /// the underlying column is populated by derivation rather than by any
        /// emitter, and the derivation tracks **which member a query happened
        /// to return first for that workspace** — not who the event concerns.
        /// Measured there across all event families: **135 event types
        /// affected, and on 107 of 136 workspaces the subject NEVER VARIES at
        /// all.** It is effectively a per-workspace constant.
        ///
        /// **Never present a `--user-id`-filtered list to someone as that
        /// person's activity**, and do not use it to answer who an event is
        /// about. It remains fine as an OPAQUE narrowing filter where the
        /// meaning of the match does not matter.
        ///
        /// It is also NOT the actor — that is `--calling-user-id`, with its own
        /// caveats. Measured 2026-08-22: this returns `node` events (the AI
        /// pipeline) where `--calling-user-id` returned none across 1492 rows;
        /// for FILE activity, which lives in category `workspace`, both return
        /// it.
        #[arg(long)]
        user_id: Option<String>,
        /// Filter by organization ID (19-digit).
        #[arg(long)]
        org_id: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        ///
        /// The `import_*` event family — source lifecycle and write-backs —
        /// is category `import`, subcategory `cloud_import`. `import` is the
        /// legacy name, kept because it is a persisted value on every
        /// historical row; the current product name `cloudsync` is NOT a
        /// category and matches nothing here, belonging instead to the
        /// activity-poll mechanism. Three spellings, one domain.
        ///
        /// That is the family, not "everything cloud sync touches" — events
        /// describing a synced FILE arriving are storage events under
        /// `workspace`, so a `category=import` query will not return them.
        /// For what a graft is doing, `import_*` is the family you want.
        ///
        /// **Import events need ADMIN-level access on the workspace.** Owner
        /// and admin receive them; member and viewer receive none — the rows
        /// are fetched and then dropped by a per-row permission check, so a
        /// member sees a clean empty result rather than an error.
        ///
        /// So an empty import result from a member or viewer token is not
        /// evidence that nothing happened, on a workspace that may be
        /// generating hundreds of these a day. Re-run with admin-level access
        /// before concluding a sync is idle. Other event families are
        /// unaffected: the same token reads them from the same workspace
        /// normally, which is what makes the empty result so easy to
        /// misread.
        ///
        /// **A member is not blind to sync activity, though.** Files arriving
        /// through a graft also emit `workspace_storage_file_sync_added` (and
        /// `_sync_updated` for changes), which carry member permission and sit
        /// under category `workspace`, not `import` — so they come back for a
        /// member token that gets nothing from the import family. Watch those
        /// for "did anything land"; the import family is for what a SOURCE is
        /// doing. **Removals emit neither** — no sync-delete counterpart
        /// exists, so a file disappearing from a graft is invisible on this
        /// route to every caller.
        ///
        /// The query parameter is `workspace_id`; sending `workspace` fails
        /// loudly rather than returning an empty result. This CLI sends the
        /// correct one, so a wrong parameter is not a cause of an empty
        /// result here.
        ///
        /// **`node` is NOT where your file activity lives — `workspace` is.**
        /// Enumerated 2026-08-22 rather than inferred from the names:
        /// every `node` row was `node_ai_summary_created` (the AI pipeline),
        /// while file operations were `workspace_storage_file_added` /
        /// `_deleted` / `_moved` / `_sync_added` / `_sync_updated` under
        /// `workspace`. **So `--category node` answers a question about the AI
        /// pipeline**, and asking it for uploads and deletes returns rows that
        /// look plausible and are about something else.
        ///
        /// The category has FOUR members, all AI-pipeline, and this API can
        /// only ever show you ONE of them: `node_ai_summary_created` is
        /// audit-log visible while `node_ai_state_set`, `node_ai_added_to_rag`
        /// and `node_ai_removed_from_rag` are internal-visibility and reach no
        /// caller. (The full member list is not enumerable through the API —
        /// which is the point: **enumerating this category through the API
        /// cannot see three quarters of it.**)
        ///
        /// One workspace, one sample; other categories seen there were
        /// `import`, `metadata`, `share`, `user`, `apps`, `ai`, `email`, `org`,
        /// `invitation` and `workflow`, whose contents were NOT enumerated —
        /// so treat those names as labels you have not opened.
        #[arg(long)]
        category: Option<String>,
        /// Filter by subcategory.
        #[arg(long)]
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children (parent event ID).
        #[arg(long)]
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event (19-digit). **Not a
        /// synonym for `--user-id` — they answer different questions.**
        ///
        /// **This filter returns NO `node` events.** Measured 2026-08-22, one
        /// workspace, paged to an EMPTY page: 1492 rows across `import`,
        /// `metadata`, `share`, `user`, `workflow` and `workspace` —
        /// and not one `node` row, where `--user-id` for the same user returned
        /// 421 rows including them.
        ///
        /// **That is mostly correct behaviour, not lost file activity.**
        /// Measured on the same data: every `node` row was
        /// `node_ai_summary_created` — the AI pipeline, which has no acting
        /// user to match. **Your file activity is category `workspace`**
        /// (`workspace_storage_file_added` / `_deleted` / `_moved` /
        /// `_sync_added` / `_sync_updated`), and this filter returns those.
        ///
        /// **The caution that survives: an absent actor does NOT mean nobody
        /// was responsible.** Whole event families carry no actor even when a
        /// specific person caused them — sync, import and write-back lifecycle
        /// events, and authorizing or revoking a storage provider, which is
        /// about as deliberate a user action as exists. The split runs by event
        /// TYPE, not by "was a human involved": the FILE a sync lands is
        /// attributed, the SYNC that landed it is not. So this filter is the
        /// wrong tool for "everything that happened in this workspace", while
        /// still answering "what did this person do" for file activity.
        /// *(Attribution split reported from the platform side and corrected
        /// there on 2026-08-23; not measurable from this CLI.)*
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
    /// Get an AI-powered summary of events. SPENDS AI CREDITS.
    ///
    /// Accepts every filter `event list` does (the summarize endpoint shares the
    /// search filter set) plus the summarize-only `--user-context`. As with
    /// `list`, one of `--workspace` / `--share` / `--user-id` / `--org-id` /
    /// `--parent-event-id` is required by the server.
    ///
    /// **The summary describes the events YOU can see, and says so nowhere.**
    /// It is generated from the same page `event list` would return — after
    /// rows you lack access to have been dropped — so on a workspace where
    /// your access is partial it narrates a fraction of what happened, in
    /// confident prose, with no marker that anything was withheld. The
    /// reported date range is derived from the surviving rows too, so it can
    /// assert a period it never saw.
    ///
    /// The credits are spent either way: a thinned sample costs the same as a
    /// complete one. Before relying on a summary, check that `event list` with
    /// the same filters returns what you expect — and remember a short page is
    /// not the end of the data (see `event list`).
    Summarize {
        /// Filter by workspace ID.
        #[arg(long)]
        workspace: Option<String>,
        /// Filter by share ID.
        #[arg(long)]
        share: Option<String>,
        /// Narrow by user profile ID (19-digit). **Do NOT read this as "events
        /// about this person" — it does not reliably identify anyone.**
        ///
        /// Reported from the platform side 2026-08-24 and NOT measurable here:
        /// the underlying column is populated by derivation rather than by any
        /// emitter, and the derivation tracks **which member a query happened
        /// to return first for that workspace** — not who the event concerns.
        /// Measured there across all event families: **135 event types
        /// affected, and on 107 of 136 workspaces the subject NEVER VARIES at
        /// all.** It is effectively a per-workspace constant.
        ///
        /// **Never present a `--user-id`-filtered list to someone as that
        /// person's activity**, and do not use it to answer who an event is
        /// about. It remains fine as an OPAQUE narrowing filter where the
        /// meaning of the match does not matter.
        ///
        /// It is also NOT the actor — that is `--calling-user-id`, with its own
        /// caveats. Measured 2026-08-22: this returns `node` events (the AI
        /// pipeline) where `--calling-user-id` returned none across 1492 rows;
        /// for FILE activity, which lives in category `workspace`, both return
        /// it.
        #[arg(long)]
        user_id: Option<String>,
        /// Filter by organization ID (19-digit).
        #[arg(long)]
        org_id: Option<String>,
        /// Filter by event name.
        #[arg(long)]
        event: Option<String>,
        /// Filter by category.
        ///
        /// The `import_*` event family — source lifecycle and write-backs —
        /// is category `import`, subcategory `cloud_import`. `import` is the
        /// legacy name, kept because it is a persisted value on every
        /// historical row; the current product name `cloudsync` is NOT a
        /// category and matches nothing here, belonging instead to the
        /// activity-poll mechanism. Three spellings, one domain.
        ///
        /// That is the family, not "everything cloud sync touches" — events
        /// describing a synced FILE arriving are storage events under
        /// `workspace`, so a `category=import` query will not return them.
        /// For what a graft is doing, `import_*` is the family you want.
        ///
        /// **Import events need ADMIN-level access on the workspace.** Owner
        /// and admin receive them; member and viewer receive none — the rows
        /// are fetched and then dropped by a per-row permission check, so a
        /// member sees a clean empty result rather than an error.
        ///
        /// So an empty import result from a member or viewer token is not
        /// evidence that nothing happened, on a workspace that may be
        /// generating hundreds of these a day. Re-run with admin-level access
        /// before concluding a sync is idle. Other event families are
        /// unaffected: the same token reads them from the same workspace
        /// normally, which is what makes the empty result so easy to
        /// misread.
        ///
        /// **A member is not blind to sync activity, though.** Files arriving
        /// through a graft also emit `workspace_storage_file_sync_added` (and
        /// `_sync_updated` for changes), which carry member permission and sit
        /// under category `workspace`, not `import` — so they come back for a
        /// member token that gets nothing from the import family. Watch those
        /// for "did anything land"; the import family is for what a SOURCE is
        /// doing. **Removals emit neither** — no sync-delete counterpart
        /// exists, so a file disappearing from a graft is invisible on this
        /// route to every caller.
        ///
        /// The query parameter is `workspace_id`; sending `workspace` fails
        /// loudly rather than returning an empty result. This CLI sends the
        /// correct one, so a wrong parameter is not a cause of an empty
        /// result here.
        ///
        /// **`node` is NOT where your file activity lives — `workspace` is.**
        /// Enumerated 2026-08-22 rather than inferred from the names:
        /// every `node` row was `node_ai_summary_created` (the AI pipeline),
        /// while file operations were `workspace_storage_file_added` /
        /// `_deleted` / `_moved` / `_sync_added` / `_sync_updated` under
        /// `workspace`. **So `--category node` answers a question about the AI
        /// pipeline**, and asking it for uploads and deletes returns rows that
        /// look plausible and are about something else.
        ///
        /// The category has FOUR members, all AI-pipeline, and this API can
        /// only ever show you ONE of them: `node_ai_summary_created` is
        /// audit-log visible while `node_ai_state_set`, `node_ai_added_to_rag`
        /// and `node_ai_removed_from_rag` are internal-visibility and reach no
        /// caller. (The full member list is not enumerable through the API —
        /// which is the point: **enumerating this category through the API
        /// cannot see three quarters of it.**)
        ///
        /// One workspace, one sample; other categories seen there were
        /// `import`, `metadata`, `share`, `user`, `apps`, `ai`, `email`, `org`,
        /// `invitation` and `workflow`, whose contents were NOT enumerated —
        /// so treat those names as labels you have not opened.
        #[arg(long)]
        category: Option<String>,
        /// Filter by subcategory.
        #[arg(long)]
        subcategory: Option<String>,
        /// Drill into a serial/batch parent event's children (parent event ID).
        #[arg(long)]
        parent_event_id: Option<String>,
        /// Filter by the user who triggered the event (19-digit). **Not a
        /// synonym for `--user-id` — they answer different questions.**
        ///
        /// **This filter returns NO `node` events.** Measured 2026-08-22, one
        /// workspace, paged to an EMPTY page: 1492 rows across `import`,
        /// `metadata`, `share`, `user`, `workflow` and `workspace` —
        /// and not one `node` row, where `--user-id` for the same user returned
        /// 421 rows including them.
        ///
        /// **That is mostly correct behaviour, not lost file activity.**
        /// Measured on the same data: every `node` row was
        /// `node_ai_summary_created` — the AI pipeline, which has no acting
        /// user to match. **Your file activity is category `workspace`**
        /// (`workspace_storage_file_added` / `_deleted` / `_moved` /
        /// `_sync_added` / `_sync_updated`), and this filter returns those.
        ///
        /// **The caution that survives: an absent actor does NOT mean nobody
        /// was responsible.** Whole event families carry no actor even when a
        /// specific person caused them — sync, import and write-back lifecycle
        /// events, and authorizing or revoking a storage provider, which is
        /// about as deliberate a user action as exists. The split runs by event
        /// TYPE, not by "was a human involved": the FILE a sync lands is
        /// attributed, the SYNC that landed it is not. So this filter is the
        /// wrong tool for "everything that happened in this workspace", while
        /// still answering "what did this person do" for file activity.
        /// *(Attribution split reported from the platform side and corrected
        /// there on 2026-08-23; not measurable from this CLI.)*
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
        /// Card key from the feed (e.g. `mention:123…`). URL-encoding is
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
        /// Scope to files: comma-separated `nodeId` or `nodeId:versionId` pairs.
        /// The version is optional and auto-resolves to the current one. May be
        /// combined with `--folders-scope` and `--files-attach`.
        #[arg(long)]
        files_scope: Option<String>,
        /// Scope to folders: comma-separated `nodeId` entries. A `:depth` suffix
        /// is accepted but ignored. Shares the server's reference budget with
        /// `--files-scope`; may be combined with it and with `--files-attach`.
        #[arg(long)]
        folders_scope: Option<String>,
        /// Attach files: comma-separated `nodeId` or `nodeId:versionId` pairs.
        /// The version is optional and auto-resolves. Sent separately from the
        /// scope flags and may be combined with them.
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
        /// Replay guard for this turn (max 64 chars).
        ///
        /// One is generated automatically. Pass a PREVIOUS turn's key to replay
        /// that turn instead of creating — and billing — a second one; a failed
        /// send prints the key it used, for exactly this purpose.
        #[arg(long)]
        idempotency_key: Option<String>,
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
        /// Scope the chat to files: comma-separated `nodeId` or
        /// `nodeId:versionId` pairs. The version is optional and auto-resolves
        /// to the current one. May be combined with the other scope/attach flags.
        #[arg(long)]
        files_scope: Option<String>,
        /// Scope the chat to folders: comma-separated `nodeId` entries. A
        /// `:depth` suffix is accepted but ignored. Shares the server's
        /// reference budget with `--files-scope`.
        #[arg(long)]
        folders_scope: Option<String>,
        /// Attach files to the chat: comma-separated `nodeId` or
        /// `nodeId:versionId` pairs. The version is optional and auto-resolves.
        /// Sent separately from the scope flags and may be combined with them.
        #[arg(long)]
        files_attach: Option<String>,
        /// [deprecated] Scope to file node IDs (comma-separated); prefer
        /// `--files-scope`. Bare node IDs are accepted — the version
        /// auto-resolves to the current one server-side.
        #[arg(long, value_delimiter = ',', hide = true)]
        node_ids: Option<Vec<String>>,
        /// [deprecated] Folder ID to scope to; mapped to `folders_scope=<id>`.
        /// Prefer `--folders-scope`.
        #[arg(long, hide = true)]
        folder_id: Option<String>,
        /// [deprecated] No longer maps to a chat parameter; accepted but ignored.
        #[arg(long, hide = true)]
        intelligence: Option<bool>,
        /// Replay guard for this turn (max 64 chars).
        ///
        /// One is generated automatically. Pass a PREVIOUS turn's key to replay
        /// that turn instead of creating — and billing — a second one; a failed
        /// send prints the key it used, for exactly this purpose.
        #[arg(long)]
        idempotency_key: Option<String>,
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
        /// Maximum number of messages to show.
        ///
        /// The endpoint returns the whole chat and accepts no server-side limit,
        /// so this trims the rendered list here.
        #[arg(long)]
        limit: Option<u32>,
        /// Skip this many messages (sent as a path segment, per the API).
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
    /// List the app installations registered to your account.
    ///
    /// These are installation RECORDS, not a catalog of available apps: each
    /// row is an app that registered itself against your account, with its
    /// version, platform, install/uninstall times and last heartbeat. The
    /// `app_id` is caller-defined (e.g. `com.example.desktop`), so what appears
    /// here is whatever your apps have reported.
    ///
    /// Requires authentication. Registering, removing, and heartbeating an
    /// installation are separate platform operations the CLI does not currently
    /// surface.
    ///
    /// `details`, `launch`, and `tool-apps` were removed in 2026.8: they called
    /// a `/apps/` path prefix that has never existed on the server, so they
    /// failed on every invocation. They described a widget catalog that has no
    /// REST API at all — app widgets are an MCP-server concept, never a
    /// documented HTTP surface — so there is nothing to re-point them at.
    List,
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
    /// Provision a new provider identity (async; returns `provisioning`).
    ///
    /// Provisioning never completes inline, and **no provider completes by
    /// polling alone**. All four — `google_drive`, `box`, `dropbox`,
    /// `onedrive_business` — connect the caller's OWN cloud account through a
    /// browser consent screen. The identity comes back `provisioning` with an
    /// `authorize_url` a human must open, and `properties.oauth_pending: true`
    /// marking it as awaiting that grant.
    ///
    /// `identity_email` is a synthetic placeholder until the consent finishes
    /// (`provisioning-pending`, `dropbox-oauth-pending`, …) — treat ANY pending
    /// value as "no address yet" rather than matching one string. After the
    /// grant it becomes the connected account's real address.
    ///
    /// This CLI never handles the redirect: open the `authorize_url`, complete
    /// consent in the browser, then poll `import identity-details` until the
    /// status is `active` or `error`. **A `provisioning` identity with
    /// `oauth_pending` and no `authorize_url` is terminal, not slow** — the URL
    /// is issued only on this response and never on a poll, so waiting cannot
    /// help. **Re-provisioning is the fix, but not immediately**: within about
    /// 30 minutes it returns the SAME row with `already_exists` and still no
    /// URL (see below), so revoke first if you need to recover now.
    ///
    /// `already_exists: true` in the response means you were handed an EXISTING
    /// identity rather than a new one, and it comes with NO fresh
    /// `authorize_url`. If that row is already `active` you are done — **unless
    /// you passed `--account-type`**, which is recorded only where a connect
    /// actually begins: an existing row keeps the account type it was connected
    /// with, so the value you passed is dropped and the response still reads as
    /// a success. One connection exists per user per workspace per provider and
    /// `work` and `personal` share the single `onedrive_business` provider, so
    /// switching means revoking that connection first and provisioning again.
    /// The CLI prints a note on stderr when it detects this. If it is
    /// still `provisioning` — an abandoned browser tab — you cannot retry
    /// immediately: the row becomes re-provisionable after roughly 30 minutes,
    /// or revoke it and provision again to recover sooner.
    ///
    /// There is no address to share a folder with on any provider — the
    /// connected account IS the access. (An older model had `google_drive` and
    /// `box` background-provisioned as service accounts with an address to
    /// share with. That model is gone. If you find a document describing it,
    /// the document is behind — this was verified against the live API.)
    ///
    /// **The response's `instructions` field is the authoritative per-provider
    /// setup wording — read it rather than relying on the summary here.** It
    /// comes from the platform and changes with the provider integrations,
    /// while this help text is a copy that can fall behind them. It is
    /// returned ONLY by provision-identity — `identity-details` does not
    /// carry it — so capture it from the provision response. Anything telling
    /// you to share a folder with an address, or to grant a permission level
    /// to us, is the retired model above, whatever its source.
    ///
    /// A tenant that has not consented ends `error` with a reason and an
    /// admin-consent URL — "ask your IT admin to approve", which is a different
    /// action from a revoked or stale credential. A PERSONAL account has no
    /// tenant and no administrator: there the remedy is to connect again and
    /// approve the storage scope.
    ///
    /// **Every provider requires full workspace Member.** The floor is no
    /// longer provider-dependent: all four connect by delegated OAuth, and
    /// the handler applies the Member check to every one of them, so a Viewer
    /// is refused for `dropbox`, `box` and `google_drive` exactly as for
    /// `onedrive_business`. The reason is worth knowing
    /// before you file a bug: an `onedrive_business` connection is a delegated
    /// grant to the connecting user's own Microsoft account, and the only
    /// thing it can be
    /// used for — creating a source — already requires Member, so a view-only
    /// caller would hand over a live credential they could never use.
    ///
    /// Up to 4 identities per user per workspace.
    #[command(name = "provision-identity")]
    ProvisionIdentity {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Cloud provider: `google_drive`, box, `onedrive_business`, dropbox.
        #[arg(long)]
        provider: String,
        /// Microsoft account family for `onedrive_business`: `work` or
        /// `personal`.
        ///
        /// Omitted means `work` — the SERVER's default, not this CLI's. A
        /// personal Microsoft account cannot complete a `work` consent, so
        /// connecting one requires `--account-type personal` explicitly.
        /// Ignored by the other three providers.
        #[arg(long)]
        account_type: Option<String>,
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
    /// Revoke a provider identity (async; may be transiently refused).
    ///
    /// Revocation is asynchronous: the identity returns `revoking` and reaches
    /// `revoked` in the background — poll `import identity-details`.
    ///
    /// A refusal is not necessarily a failure: while another lifecycle
    /// operation on the same connection is in flight, revoke is refused as
    /// BUSY. Retry after a moment rather than reporting the revoke as failed.
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
    ///
    /// **Two fields are missing from these rows and exist only on `import
    /// source-details`:** `owner_user_id` and `provider_name`. Measured
    /// 2026-08-23 — a row here carried 34 keys against the detail read's 36,
    /// and this listing is otherwise a strict subset (nothing appears here that
    /// is missing there).
    ///
    /// **`conflict_count` is now on BOTH**, and it is the one worth reading: it
    /// counts the write-backs sitting in `conflict`, which need a human
    /// decision. Measured the same day — a source with two such rows reports
    /// `2` on the listing and on the detail read alike.
    ///
    /// **It was details-only the day before**, and that is the useful part of
    /// this note rather than trivia: the shape of this response changed under a
    /// deploy between two measurements a few hours apart. **Trust the row in
    /// front of you over any field list, including this one, and never read a
    /// missing field as a zero.**
    #[command(name = "list-sources")]
    ListSources {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// DO NOT ASSUME THIS NARROWED ANYTHING — read the rows you got back.
        ///
        /// Measured 2026-08-21 and NOT applied: a workspace holding two sources
        /// in DIFFERENT statuses returned BOTH rows when filtered by either one,
        /// and an unknown value returned both as well rather than erroring. The
        /// endpoint is documented with no status parameter. Results therefore
        /// come back unfiltered while looking filtered — the failure that most
        /// resembles success, since every row is real and only the narrowing is
        /// imaginary.
        ///
        /// The flag is still sent, so filtering starts working the day the
        /// server implements it. That is why this says "read each row's
        /// `status`" rather than "the flag is broken": checking the rows is
        /// correct either way, and does not go stale on a platform fix.
        ///
        /// If you narrow the rows yourself, do NOT thin a single page and treat
        /// the remainder as a filtered page — `--limit`/`--offset` count
        /// UNFILTERED rows, so "12 synced on this page" is not "12 synced".
        ///
        /// Page first, concatenate, THEN filter — and when paging, **stop on an
        /// EMPTY page, never on a short one**, same rule as `event list`. The
        /// exception is an existence check ("is there a synced source at all?"),
        /// which can stop at the first match rather than traversing everything.
        #[arg(long)]
        status: Option<String>,
        /// Maximum number of results per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// List stored document libraries (drives) an identity can reach.
    ///
    /// `onedrive_business` only. Reads the STORED catalog and never enumerates
    /// — use `import refresh-drives` to build or rebuild it. An empty list is
    /// the normal first-connect state and means nothing on its own; read
    /// `drives_state` to see which empty it is.
    ///
    /// OWNER ONLY — a workspace admin cannot read someone else's catalog. The
    /// server relaxes to owner-or-admin for other providers, but this command
    /// is `onedrive_business`-only, so the reachable rule is always owner. A
    /// delegated catalog is the connecting user's own reachable content, which
    /// is why it is not an admin surface.
    #[command(name = "list-drives")]
    ListDrives {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
        /// Filter stored rows by the site path they were discovered through.
        ///
        /// This filters what is already stored; it cannot populate an empty
        /// catalog. To resolve a `requires_site_path` state, pass `--site-path`
        /// to `import refresh-drives` instead.
        #[arg(long)]
        site_path: Option<String>,
        /// Maximum number of drives per page.
        #[arg(long)]
        limit: Option<u32>,
        /// Offset for pagination.
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Rebuild an identity's drive catalog from the provider (async).
    ///
    /// The only call that talks to the provider. Returns no job id: poll
    /// `import list-drives` until `drives_state` leaves `refreshing`. Fails if
    /// a refresh is already in flight for the identity.
    ///
    /// OWNER ONLY, for the same reason as `import list-drives`: a workspace
    /// admin cannot refresh someone else's catalog.
    #[command(name = "refresh-drives")]
    RefreshDrives {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
        /// Site path to enumerate, e.g. `contoso.sharepoint.com:/sites/Marketing`.
        ///
        /// Required when `list-drives` reports `drives_state:
        /// requires_site_path` — that tenant will not enumerate its own sites,
        /// so the site must be named here.
        #[arg(long)]
        site_path: Option<String>,
    },
    /// Discover shared folders from a provider (async; returns a job).
    ///
    /// Returns a `job_id`, not the folder list. Poll it with
    /// `import job-details discovery --job-id <job_id>` — a discovery job has
    /// no import source, so `discovery` is the literal source id.
    ///
    /// A 200 here means QUEUED, not succeeded. The route validates the identity
    /// and enqueues; everything else — provider errors, credential problems,
    /// and workspace-access checks made when the job runs — surfaces in the
    /// polled job, never in this call. Scripts must branch on the job result,
    /// not on this exit code: a failed discovery still exits 0 here, so `&&`
    /// chaining will run the next command as if it had succeeded.
    Discover {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
        /// Drive ID to enumerate (`onedrive_business`).
        ///
        /// Pass the same drive the source will be created against, from
        /// `import list-drives`. Without it a multi-library identity
        /// enumerates its default library, so the discovered paths may not
        /// belong to the library the source is bound to.
        #[arg(long)]
        drive_id: Option<String>,
        /// Enumerate INSIDE this folder instead of the provider root.
        ///
        /// Omitted enumerates the root. The server returns ONE LEVEL per call
        /// and will not walk a tree it cannot bound, so drill down by
        /// discovering the root, picking a folder, then discovering again with
        /// that folder's `remote_path`.
        #[arg(long)]
        remote_path: Option<String>,
    },
    /// Create an import source (identity owner only).
    ///
    /// You must be the OWNER of the cloud identity and hold at least member
    /// permission on the workspace. An admin cannot graft in on another user's
    /// behalf — that path is refused with `1680` Access Denied.
    #[command(name = "create-source")]
    CreateSource {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Identity ID.
        #[arg(long)]
        identity_id: String,
        /// Remote folder path, recorded as given and NOT re-resolved later.
        ///
        /// The path is captured when the source is created; nothing tracks the
        /// folder afterwards. **Rename or move it at the provider and every
        /// subsequent sync fails**, because the stored path no longer resolves
        /// — the connection is still fine, so an error here is not necessarily
        /// about the account.
        ///
        /// **Renaming the folder back is the cheap fix.** Re-grafting is not:
        /// duplicate detection matches on the provider's folder id, and a
        /// failing source still RESERVES that folder while it sits in `error`
        /// — so pointing a new source at the folder's new path is refused as a
        /// duplicate of the broken one. Disconnect or delete the stale source
        /// first, then create the new one. (A `disconnected` or deleted source
        /// reserves nothing.)
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
        /// Drive ID to bind the source to (required for `onedrive_business`).
        ///
        /// Get it from `import list-drives`. Create-time only: the imported
        /// folder tree is built against this library, so changing it later is
        /// a delete-and-recreate rather than an update.
        #[arg(long)]
        drive_id: Option<String>,
        /// Storage folder to graft the import under.
        ///
        /// Defaults to the workspace's `Imports` folder. Must be a folder and
        /// must not sit inside an existing import tree. Create-time only —
        /// to relocate an existing source, move the imported folder instead.
        #[arg(long)]
        destination_node_id: Option<String>,
    },
    /// Get source details.
    ///
    /// `remote_folder_web_url` is always present and may be NULL — that is not
    /// missing data. It is built from the provider's folder id, which is
    /// captured when a source SYNCS, so null has two quite different causes:
    ///
    /// - **Not yet captured.** A source that has never synced — or last synced
    ///   before the capture existed — has no folder id, so no link. This
    ///   resolves itself on the next sync and applies to every provider.
    /// - **No link to build.** Only `google_drive` and `box` have an
    ///   id-addressed folder URL; for `dropbox` and `onedrive_business` there
    ///   is nothing to point at, so null is permanent.
    ///
    /// Treat the field as null-tolerant rather than inferring a provider from
    /// it: null does NOT mean "this provider has no links".
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
        /// `paused` stops syncing; `synced` asks for it back. These are the ONLY
        /// two accepted values: this flag drives the server's pause and resume
        /// actions, and no other status is settable through it.
        ///
        /// **A resume that returns success has not always resumed.** Until
        /// 2026-08-21 the server accepted the request, failed its internal state
        /// transition, discarded that failure and returned 200 anyway — leaving
        /// the source `paused` and off the sync schedule for good, with nothing
        /// in the response to say so. Confirmed and fixed upstream that day.
        ///
        /// A fix reaches you only where it is deployed, so re-read
        /// `import source-details` afterwards. **A source still reading
        /// `paused` after a 200 is exactly the shape this defect had** — that
        /// is the check that catches it. `last_sync_at` advancing is the
        /// stronger confirmation on top, because it only moves once the
        /// scheduler has actually picked the source up again, so a source that
        /// reads `synced` while `last_sync_at` never moves is stuck for some
        /// OTHER reason.
        ///
        /// This is the round trip to reach for when a sync should stop for a
        /// while — unlike `import disconnect` and `import delete-source`,
        /// which are both permanent for the source.
        ///
        /// **Each direction has a precondition on the source's CURRENT
        /// status.** Pause is accepted only from `synced` or `error`; resume
        /// only from `paused`. So a source that is mid-run cannot be paused
        /// until it settles, and re-pausing an already-paused source is
        /// REFUSED rather than being a silent no-op.
        #[arg(
            long,
            value_parser = clap::builder::PossibleValuesParser::new(
                fastio_cli::api::import::UPDATE_SOURCE_STATUS_VALUES.iter().copied(),
            ),
        )]
        status: Option<String>,
        /// Display name.
        #[arg(long)]
        remote_name: Option<String>,
        /// Access mode: `read_only` or `read_write`.
        #[arg(long)]
        access_mode: Option<String>,
    },
    /// Delete an import source (soft delete). Does NOT require disconnecting
    /// first — a HEALTHY, actively-synced source is deleted in one call.
    ///
    /// The server refuses only sources with work IN FLIGHT (syncing,
    /// discovering, disconnecting, pending); everything else — including a
    /// healthy `synced` graft — is accepted. Treat that refusal as TRANSIENT,
    /// not as a permission problem: retry once the run finishes. Read the
    /// rule as a denylist of in-flight states rather than an allowlist of
    /// safe ones, so it stays true if a status is added.
    ///
    /// **Unpushed local edits are discarded, not flushed** — any write-back that
    /// has not reached `completed` never reaches the provider once the source is
    /// deleted. **Drain the queue first, following the per-state list below** —
    /// that list is the authoritative version: the remedy depends on the state,
    /// and pushing is the WRONG move for most of them. Check
    /// `import list-writebacks`, or the nodes' own `import_state.status`,
    /// before you call this. **If you check through anything other than this
    /// CLI, read that command's help first:** the route returns an empty list
    /// for a hyphenated source id, and an empty queue is exactly the answer
    /// that would tell you it is safe to proceed.
    ///
    /// **Do not expect the queue to be tidied up.** Deleting the source cancels
    /// its queued write-backs — the contract lists a source being "flipped to
    /// `read_only`, disconnected or deleted" among the ways a job is cancelled
    /// for you — and `canceled` is FINAL: no endpoint accepts a job in that
    /// state, so **that job** cannot be resumed. Cancelling is a STATUS write,
    /// not a delete, so a row can outlive the work it described.
    ///
    /// **After the delete you cannot LIST those rows at all.**
    /// `import list-writebacks` is REFUSED once the source is gone: the route
    /// answers `1609 (Not Found)` / 404, whose own error table gives the cause
    /// as "Unknown source, or one already deleted". The record is therefore
    /// unreadable through this command afterwards, whether or not the rows
    /// survive internally — **so capture it BEFORE you delete.** No contract
    /// publishes a retention window for write-back rows, so do not count on
    /// one. For `import disconnect`, where the source is NOT deleted, the
    /// contract is silent and this CLI promises nothing either way. While the
    /// source still exists this CLI does show you those rows, sending the
    /// canonical source id so the route does not answer with an empty list.
    ///
    /// **Neither command preserves PENDING PUSHES, and `disconnect keep` does
    /// not either.** `keep` preserves the FILES — they become ordinary workspace
    /// storage — but it strips their import markers, so queued write-backs are
    /// cancelled and those edits never reach the provider. A `canceled`
    /// write-back after a disconnect is EXPECTED and FINAL: no endpoint accepts
    /// a canceled job, so **that job** cannot be resumed — and once the source
    /// is disconnected the write-back route is gone with it, so the content
    /// cannot be pushed afresh either. **Push it BEFORE you run this** (see the
    /// `canceled` entry below).
    ///
    /// **If you have unpushed edits you want at the provider, drain the queue
    /// FIRST.** Work it state by state — the remedies are NOT interchangeable,
    /// and there is no single command that drains everything. **Reading the
    /// queue needs only view access, but every remedy that CHANGES something —
    /// push, retry, resolve — needs the connected identity's owner or a
    /// workspace admin, and refuses anyone else with `1680`** (the pushing ones
    /// run under that member's own cloud credential). An ordinary member can
    /// therefore see the problem and be unable to fix it; do not read a refusal
    /// as "already drained".
    ///
    /// `pending` / `uploading` — **WAIT.** The job runs on its own. Do NOT
    /// reach for `import push-writeback` here: a push is refused while a live
    /// upload already covers that node, and since the refusal clears once the
    /// original finishes, a retry-until-it-works loop can enqueue a SECOND
    /// upload of the same file. (Coverage is per OPERATION — an `upload` and a
    /// `delete` for one file are independent jobs.)
    ///
    /// `conflict` — **`import resolve-conflict`, and the resolution is a real
    /// decision: do not default it.** `--resolution keep_local` re-queues your
    /// copy and pushes it out, **overwriting the changed copy at the provider**
    /// with the conflict check skipped for that attempt. `--resolution
    /// keep_remote` pulls the provider's version down over yours and writes
    /// nothing outward. **Each choice destroys the other side's change.**
    /// Only `keep_local` gets your edit to the provider, but reaching for it
    /// reflexively to clear the queue silently discards whatever changed at the
    /// provider — look at both copies first.
    ///
    /// `failed` — **read `error_message` and fix the cause FIRST**, then
    /// `import retry-writeback`. A retry re-queues the same transfer without
    /// changing it, so an unfixed cause — a permission the connected account
    /// does not have, or a provider that rejects the file — fails the same way.
    /// A missing write permission is a **permanent** failure until the
    /// permission is granted. **A `failed` row is unpushed too** — a queue with
    /// nothing `pending`, `uploading` or `conflict` left can still be holding
    /// edits that never reached the provider; checking only those three states
    /// is how you lose them.
    ///
    /// `canceled` — unpushed, and **that job** cannot be resumed: no endpoint
    /// accepts it. A canceled **upload** can SOMETIMES be re-created as a new
    /// job with `import push-writeback --node-id <node>` — but that route
    /// carries a long list of preconditions (the source still connected and
    /// `read_write`, the node still imported by THIS source, still under its
    /// folder, still holding its import metadata, which `disconnect keep`
    /// strips…). **Treat it as something to ATTEMPT and verify, never as a
    /// fallback you are entitled to** — read the result rather than assuming it
    /// worked. And "just try it" is not free: when a push is accepted it
    /// **queues real work** (a new `upload` job on the file's CURRENT bytes),
    /// so it is a write to reason about, not a probe. **Do it BEFORE this
    /// command**; the source merely
    /// surviving is not enough. A canceled **delete** cannot be re-created this
    /// way at all — a manual push is always an upload, never a delete.
    ///
    /// **Three ways a "queue looks clean" check lies to you.**
    ///
    /// (1) **You are reading ONE PAGE.** `list-writebacks` returns newest-first
    /// with no status filter and no sort control, and this CLI does not
    /// auto-paginate. Page explicitly — `--limit 200` with `--offset` 0, 200,
    /// 400, … — until a page comes back with **fewer than 200** rows. **Do
    /// NOT stop when a page is shorter than the limit you asked for:** `limit`
    /// is capped at 200 and is **silently clamped, never rejected** (and
    /// `--limit 0` yields 1), so `--limit 500` hands you 200 rows
    /// that look like a short final page while every older row — including a
    /// `failed` one — stays unseen. `pagination.total` counts the rows on THIS
    /// page, not the source's jobs, so it cannot settle it either.
    ///
    /// (2) **An edit can exist with NO ROW AT ALL, so an empty queue proves
    /// nothing on its own.** Two ways in: a local edit made while the source is
    /// `read_only` creates **no write-back row at all** (*inferred: the
    /// contract says a source only writes back while `access_mode` is
    /// `read_write`, and the explicit push route refuses a `read_only` source
    /// with `1660`*), so an all-`completed` queue does not prove that edit
    /// reached the provider; and a write-back never
    /// enqueued leaves the node `synced` and unstamped with nothing queued
    /// (*that second mechanism — the node being marked imported before the
    /// enqueue — is reported from the platform side, is not published in the
    /// contract, and is not measured here*). Check `access_mode` on the source
    /// and `import_state.status` on the nodes you care about.
    ///
    /// (3) **Checking and deleting are not atomic.** Editing an imported file
    /// queues a write-back automatically, so any job created after your last
    /// page that has not completed by the time this runs is cancelled with the
    /// rest. Quiesce whatever is writing before you rely on the check.
    ///
    /// Only once every page reads `completed` (plus any `canceled` you accept
    /// losing) should you **delete the source.**
    ///
    /// **Neither this command nor `import disconnect` runs the work**, so do
    /// not choose between them on work-preservation grounds, and neither is
    /// reversible: a disconnected source is equally permanent and cannot be
    /// resumed. The contract treats "disconnected or deleted" identically and
    /// does not publish when each marks its rows, so this help makes no claim
    /// about a timing difference between them.
    ///
    /// **To stop syncing only for a while, pause instead of either:**
    /// `import update-source --status paused`, resumed with `--status synced`.
    #[command(name = "delete-source")]
    DeleteSource {
        /// Source ID.
        source_id: String,
    },
    /// Stop syncing a source, keeping or deleting the imported files.
    ///
    /// `--action keep` leaves the already-imported files in workspace storage
    /// and strips their import markers, so they become ordinary files with no
    /// further link to the source. `delete` moves them to the TRASH rather
    /// than erasing them — which is a statement about the MECHANISM and
    /// nothing more. Do not read it as "so you can restore them": whether a
    /// trashed file actually comes back is a separate question, and for files
    /// inside a graft that exact inference was already wrong. Either way the
    /// sync stops. Owner or admin.
    ///
    /// **A `delete` can fail while the disconnect succeeds.** The response is
    /// still a success — the source really did disconnect — so check
    /// `data_deleted` before telling anyone their data is gone: `false` means
    /// the files are still present, and the accompanying `message` says so.
    /// It is `null` for `keep`, where nothing was to be deleted. Only an
    /// explicit `false` is a failure — a MISSING field means the server did
    /// not report one, not that the delete failed, so do not treat absence as
    /// bad news.
    ///
    /// **Capture that answer now — it is not retrievable afterwards.** The
    /// action you chose is recorded server-side, but no API response exposes
    /// it: `import source-details` and `list-sources` both omit the source's
    /// properties. So this response is the only place the outcome of a delete
    /// is visible, and running the command again cannot tell you what the
    /// first run did.
    ///
    /// **A disconnected source is PERMANENT — there is no resume.** It has no
    /// onward state: you cannot restart it, and reconnecting the provider
    /// account does not bring it back. Importing that folder again means
    /// creating a NEW source with `import create-source`.
    ///
    /// **If you want to stop syncing TEMPORARILY, do not use this — pause it:**
    /// `import update-source --status paused`, and resume later with
    /// `--status synced`. Pause is the only stop here that is meant to be
    /// undone — disconnect and pause read like the same action, and only one of
    /// them is designed to come back. **Verify the resume landed** by re-reading
    /// `import source-details`: see the note on `update-source --status`, which
    /// explains why a success response is not by itself proof.
    ///
    /// That caveat is a reason to CHECK a resume, not a reason to disconnect
    /// instead. Pause is the stop designed to be undone and disconnect is not,
    /// so pause remains the right choice when syncing should come back — only
    /// your trust in the success response changes.
    ///
    /// **Losing the CONNECTION is a different thing, and WHICH thing depends on
    /// who did it.** An owner or admin revoking the identity parks that
    /// identity's sources in `error` — **except any that were already `paused`,
    /// which stay `paused`.** That is deliberate rather than an oversight:
    /// `error` is on the sync schedule and `paused` is not, so promoting a
    /// paused source would leave the scheduler retrying it forever against a
    /// credential that no longer exists.
    ///
    /// A member being removed from the workspace — or from the org — is NOT the
    /// same path: their sources go to `disconnect_pending`, which has no
    /// transition back to any live state.
    ///
    /// **Neither status is a promise that a source will sync again.** Status is
    /// only one of the gates: the sync executors refuse a revoked or revoking
    /// identity whatever the source status says, and a `read_only` access mode
    /// blocks write-back on its own. A source can even land back on `error` —
    /// the schedulable side — after a FAILED disconnect and still never run. So
    /// read a recovered-looking status as "not blocked here", never as "will
    /// resume".
    ///
    /// Disconnecting a single source is still the irreversible option despite
    /// reading like the milder one.
    ///
    /// **A disconnect CANCELS queued write-backs — including with `keep`.**
    /// `keep` preserves the FILES (they become ordinary workspace storage) but
    /// strips their import markers, so pending pushes are cancelled and those
    /// edits never reach the provider. A `canceled` write-back is EXPECTED and
    /// FINAL: no endpoint accepts a canceled job, so **that job** cannot be
    /// resumed — and once the source is disconnected the write-back route is
    /// gone with it, so the content cannot be pushed afresh either. **Push it
    /// BEFORE you run this** (see the `canceled` entry below).
    /// Cancelling is a STATUS write rather than a delete, so the queue can
    /// still look populated afterwards — **do not read a surviving row as
    /// evidence the work might still run.**
    ///
    /// **If you have unpushed edits you want at the provider, drain the queue
    /// FIRST.** Work it state by state — the remedies are NOT interchangeable,
    /// and there is no single command that drains everything. **Reading the
    /// queue needs only view access, but every remedy that CHANGES something —
    /// push, retry, resolve — needs the connected identity's owner or a
    /// workspace admin, and refuses anyone else with `1680`** (the pushing ones
    /// run under that member's own cloud credential). An ordinary member can
    /// therefore see the problem and be unable to fix it; do not read a refusal
    /// as "already drained".
    ///
    /// `pending` / `uploading` — **WAIT.** The job runs on its own. Do NOT
    /// reach for `import push-writeback` here: a push is refused while a live
    /// upload already covers that node, and since the refusal clears once the
    /// original finishes, a retry-until-it-works loop can enqueue a SECOND
    /// upload of the same file. (Coverage is per OPERATION — an `upload` and a
    /// `delete` for one file are independent jobs.)
    ///
    /// `conflict` — **`import resolve-conflict`, and the resolution is a real
    /// decision: do not default it.** `--resolution keep_local` re-queues your
    /// copy and pushes it out, **overwriting the changed copy at the provider**
    /// with the conflict check skipped for that attempt. `--resolution
    /// keep_remote` pulls the provider's version down over yours and writes
    /// nothing outward. **Each choice destroys the other side's change.**
    /// Only `keep_local` gets your edit to the provider, but reaching for it
    /// reflexively to clear the queue silently discards whatever changed at the
    /// provider — look at both copies first.
    ///
    /// `failed` — **read `error_message` and fix the cause FIRST**, then
    /// `import retry-writeback`. A retry re-queues the same transfer without
    /// changing it, so an unfixed cause — a permission the connected account
    /// does not have, or a provider that rejects the file — fails the same way.
    /// A missing write permission is a **permanent** failure until the
    /// permission is granted. **A `failed` row is unpushed too** — a queue with
    /// nothing `pending`, `uploading` or `conflict` left can still be holding
    /// edits that never reached the provider; checking only those three states
    /// is how you lose them.
    ///
    /// `canceled` — unpushed, and **that job** cannot be resumed: no endpoint
    /// accepts it. A canceled **upload** can SOMETIMES be re-created as a new
    /// job with `import push-writeback --node-id <node>` — but that route
    /// carries a long list of preconditions (the source still connected and
    /// `read_write`, the node still imported by THIS source, still under its
    /// folder, still holding its import metadata, which `disconnect keep`
    /// strips…). **Treat it as something to ATTEMPT and verify, never as a
    /// fallback you are entitled to** — read the result rather than assuming it
    /// worked. And "just try it" is not free: when a push is accepted it
    /// **queues real work** (a new `upload` job on the file's CURRENT bytes),
    /// so it is a write to reason about, not a probe. **Do it BEFORE this
    /// command**; the source merely
    /// surviving is not enough. A canceled **delete** cannot be re-created this
    /// way at all — a manual push is always an upload, never a delete.
    ///
    /// **Three ways a "queue looks clean" check lies to you.**
    ///
    /// (1) **You are reading ONE PAGE.** `list-writebacks` returns newest-first
    /// with no status filter and no sort control, and this CLI does not
    /// auto-paginate. Page explicitly — `--limit 200` with `--offset` 0, 200,
    /// 400, … — until a page comes back with **fewer than 200** rows. **Do
    /// NOT stop when a page is shorter than the limit you asked for:** `limit`
    /// is capped at 200 and is **silently clamped, never rejected** (and
    /// `--limit 0` yields 1), so `--limit 500` hands you 200 rows
    /// that look like a short final page while every older row — including a
    /// `failed` one — stays unseen. `pagination.total` counts the rows on THIS
    /// page, not the source's jobs, so it cannot settle it either.
    ///
    /// (2) **An edit can exist with NO ROW AT ALL, so an empty queue proves
    /// nothing on its own.** Two ways in: a local edit made while the source is
    /// `read_only` creates **no write-back row at all** (*inferred: the
    /// contract says a source only writes back while `access_mode` is
    /// `read_write`, and the explicit push route refuses a `read_only` source
    /// with `1660`*), so an all-`completed` queue does not prove that edit
    /// reached the provider; and a write-back never
    /// enqueued leaves the node `synced` and unstamped with nothing queued
    /// (*that second mechanism — the node being marked imported before the
    /// enqueue — is reported from the platform side, is not published in the
    /// contract, and is not measured here*). Check `access_mode` on the source
    /// and `import_state.status` on the nodes you care about.
    ///
    /// (3) **Checking and disconnecting are not atomic.** Editing an imported
    /// file queues a write-back automatically, so any job created after your
    /// last page that has not completed by the time this runs is cancelled with
    /// the rest. Quiesce whatever is writing before you rely on the check.
    ///
    /// Only once every page reads `completed` (plus any `canceled` you accept
    /// losing) should you **disconnect.**
    ///
    /// Choosing between this and `import delete-source` on work-preservation
    /// grounds is wrong in both directions: **neither runs the work.** The
    /// contract treats "disconnected or deleted" identically and does not
    /// publish when each marks its rows, so this help makes no claim about a
    /// timing difference between them.
    Disconnect {
        /// Source ID.
        source_id: String,
        /// Action: `keep` (leave imported files) or `delete` (remove them).
        #[arg(long, value_parser = ["keep", "delete"])]
        action: String,
    },
    /// Trigger an immediate incremental sync of a source.
    ///
    /// **This is not a way to unstick a slow upload — it makes one slower.**
    /// Write-backs serialise per source, one running while the rest wait their
    /// turn, so a file whose `import_state.status` is still `pending` or
    /// `uploading` after a multi-file upload is usually QUEUED rather than
    /// stalled. This command does not drain that queue: it
    /// COMPETES with it for the same per-source lock and holds it for the whole
    /// sync run, so every waiting write-back defers again and the files you were
    /// waiting on finish LATER, not sooner.
    ///
    /// It is also a different operation from the one you probably want: it
    /// compares workspace storage against the REMOTE. Running it while your own
    /// uploads have not yet reached the provider means comparing against a
    /// mirror that does not contain them yet. Let the queue drain, and watch
    /// `import_state.status` on the nodes you uploaded — see `upload file` for
    /// the values. `import list-writebacks` shows the source-level queue, with
    /// the hyphenated-id caveat documented on that command.
    ///
    /// **A success here means the job was ENQUEUED, not that a sync ran.** This
    /// is asynchronous, and the response is returned before any work happens —
    /// so an accepted call is not evidence of a sync. Confirm one actually ran
    /// the same way you would confirm a resume: `last_sync_at` in
    /// `import source-details` advancing.
    ///
    /// **If a file uploaded into a graft is missing, it is in the TRASH rather
    /// than purged — but RESTORING IT IS NOT ENOUGH.** Restore does not
    /// re-upload: it puts the file back in the workspace without queueing any
    /// write-back, so the bytes still have not reached the provider. The file
    /// then looks present and correct in Fastio while the provider has never
    /// seen it — which is the counter-intuitive part, and the reason a restore
    /// alone leaves you worse off than an obvious failure would.
    ///
    /// **Restore it and then RE-UPLOAD it.** A fresh upload is what queues the
    /// write-back that actually gets the bytes to the provider, after which the
    /// file is genuinely in the mirror.
    ///
    /// **Moving it out of the graft folder does NOT detach it — do not use a
    /// move to take a file out of the import.** Measured 2026-08-22: a node
    /// moved from a graft into an ordinary folder KEPT its `import_state`, with
    /// the same `source_id` and a `graft_root_id` still naming the folder it no
    /// longer lives in, and the source's `file_count` still counted it. That
    /// survived a full incremental sync which verifiably ran (`last_sync_at`
    /// advanced). `is_imported` stays `true` as well, so neither the flag nor
    /// the block gives you a way to notice.
    ///
    /// **Worse: the move DUPLICATES the file.** The provider's copy is
    /// untouched by a local move, so the sync re-imports it into the graft as a
    /// NEW node. **Scope: `read_write` grafts.** An inner node of a `read_only`
    /// graft cannot be moved out at all — the move is refused — and moving the
    /// graft ROOT relocates the whole graft, which is supported and is not a
    /// detach. Measured on that same file: the node moved out kept
    /// `status: completed` — the stamp its own upload left — while a second
    /// node with the same name and size appeared inside the graft at the sync
    /// timestamp reading `status: synced`, which is the UNSTAMPED default
    /// rather than a claim about where those bytes came from. **One remote
    /// file, two workspace nodes, and only one of them is where you put it.**
    /// One source, one file, one sync cycle.
    ///
    /// **Do not go looking for a per-file DETACH here: there is none.**
    /// `import push-writeback` does target a single node, so the family is not
    /// entirely source-keyed — but nothing in it detaches one file from a live
    /// import. The only thing that stops a file being imported is
    /// `import disconnect`, and that covers the entire graft. Re-uploading is
    /// the remedy for a file whose bytes never reached the provider.
    Refresh {
        /// Source ID.
        source_id: String,
    },
    /// List jobs for a source.
    ///
    /// **Job properties and error text are relayed VERBATIM.** This CLI does
    /// not inspect, filter, or redact them — deliberately, because editing a
    /// platform error makes you debug our paraphrase instead of your problem.
    /// The consequence is that whatever the server puts there reaches your
    /// terminal: error text can carry absolute server paths, stack traces, and
    /// internal staging locations, and remote file and folder NAMES appear in
    /// job detail.
    ///
    /// So treat this output as sensitive. Redirecting it into a shared build
    /// log, or pasting it into an issue, publishes whatever it happened to
    /// contain — and stdout is exactly what gets redirected.
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
    ///
    /// **The route this calls returns an empty list when the source id carries
    /// display hyphens, and this CLI strips them for you.** Measured
    /// 2026-08-22: a source holding 26 write-back rows returned `total: 0` for
    /// `argw2-322y6-…` and all 26 for `argw2322y6…`. So pass the id in whatever
    /// form you have it — the command sends the canonical one either way.
    ///
    /// **It matters if you call the API directly, or through anything that has
    /// not done this.** A formatted id gets you an empty array and an HTTP 200,
    /// which reads exactly like "nothing outstanding". The sibling
    /// `import list-jobs` returns the same count for either form, so this is a
    /// property of this route, not of the id.
    ///
    /// Per-file, `import_state.status` on the node remains the more direct
    /// answer to "have my bytes reached the provider" (see `upload file`); this
    /// command is the source-level view of the queue.
    #[command(name = "list-writebacks")]
    ListWritebacks {
        /// Source ID.
        source_id: String,
        /// ACCEPTED AND IGNORED — read each row's `status` instead.
        ///
        /// Measured 2026-08-22, on a source holding write-backs in
        /// THREE states (23 `completed`, 2 `conflict`, 1 `canceled`): filtering
        /// by `completed`, `conflict`, `canceled` or `failed` each returned all
        /// 26 rows with the full mixed distribution, and a nonsense value
        /// returned the same 26 rather than erroring. **The results come back
        /// unfiltered while looking filtered**, exactly like the `list-sources`
        /// sibling.
        ///
        /// Two earlier attempts could not settle this and are worth naming, so
        /// nobody repeats them: both returned zero for every value, because the
        /// route was returning an empty list for a hyphenated source id. That
        /// zero was first written up here as "the sources available had no
        /// write-back rows" — an inference from a zero, and wrong. **A filter
        /// test against an endpoint returning nothing proves nothing**; it took
        /// the id fix to produce rows and a mixed-state source to discriminate.
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
    // REVISIT CONDITIONS for the HTTP 500 note in the doc comment below. It is
    // provisional, and this comment exists so it does not silently harden into
    // settled fact — the shape of item that outlives its own premise because
    // nobody remembers it was conditional.
    //
    // Root cause (reported from the platform side, 2026-08-23): the endpoint
    // writes the 34-char HYPHENATED node id into a write-back column that is
    // only 30 characters wide. The INSERT fails after every authorization
    // guard passes, so it surfaces as a 500. Same fault class as the
    // `list-writebacks` defect fixed in `writebacks_path` — formatted vs
    // canonical id — but on a WRITE, where it fails loudly instead of silently.
    //
    // That also means the note's stated bound is too NARROW: the node's
    // write-back state was never the variable, so an unstamped node would 500
    // too. It is left as written until measured, not relaxed on a source read.
    //
    // Remove or rewrite the note only when ALL THREE hold — not any one:
    //   1. MET 2026-08-24 by MEASUREMENT, not by report: the same probe that
    //      500'd on 08-22 now succeeds and the write-back completes. Note the
    //      condition as originally written made an ANNOUNCEMENT a precondition
    //      for a measurement — status files lag deploys, and a deploy is a
    //      thing you can just check. Check it, and
    //   2. CLOSED 2026-08-26 by the platform owners' EXPLICIT call — which is
    //      the one escape hatch this condition itself named, not a shortcut
    //      around it. They report the push endpoint canonicalizes the
    //      path id at entry (retry/cancel/resolve likewise), so id FORM was
    //      never the variable: the 500 was the too-short INSERT failing after
    //      every guard passed. That also settles why `push_writeback` needs no
    //      client-side canonicalize — a FILTER compared against a stored column
    //      (`list-writebacks`) and a PRIMARY-KEY lookup are different problems.
    //      CLOSED ON A SOURCE READ, NOT ON A MEASUREMENT HERE. Black-box
    //      verification was attempted and BLOCKED, not skipped: the test source
    //      went `disconnected` / `read_only` ("Identity owner removed from
    //      workspace"), so the access-mode guard (`121128`) now fires first and
    //      nothing downstream can be discriminated. If a read-write fixture
    //      returns, the honest confirmation is still worth taking.
    //      Superseded text: push measured against an UNSTAMPED node — not a
    //      `completed` one. Both measurements so far used `completed`. An
    //      unstamped node means the enqueue bailed, which is a race this CLI
    //      cannot provoke on demand. Also SEARCHED for an existing fixture
    //      rather than only trying to make one (2026-08-24): the reachable
    //      source holds 25 completed / 2 conflict / 1 canceled and no `failed`
    //      or unstamped row, and the conflicted nodes are not ours to push —
    //      a push over a conflicted node overwrites the provider's divergent
    //      copy. So this waits on a fixture, not on effort. The platform root cause (id length, not node state)
    //      would make this moot — but that is a SOURCE READ, and this condition
    //      exists precisely to not lift a data-loss-adjacent ceiling on one.
    //      Lifting it on that basis is the platform owners' explicit call, not
    //      an inference to make on their behalf, and
    //   3. that measurement is green. (Green for `completed` on 2026-08-24.)
    /// Push a file to remote storage — the one import command that names a
    /// single node rather than a whole source.
    ///
    /// **This returned HTTP 500 on 2026-08-22 and SUCCEEDED in the same
    /// environment on 2026-08-24** — same source, same node, same command, with
    /// a platform fix deployed in between. The cause was server-side and had
    /// nothing to do with your id, your permissions or your request.
    ///
    /// **Both dates are given because the fix reaches you only where it is
    /// deployed.**
    ///
    /// **A 500 here has at least TWO causes, and they call for opposite
    /// responses — so read the server's message, not the status code or the
    /// hint beneath it.** Measured 2026-08-25: pushing a node that is
    /// not an imported file returns **HTTP 500** with *"Node is not an
    /// imported file"*. That is a permanent refusal, not an outage — the node
    /// is the wrong one and no amount of retrying makes it an imported file.
    /// A 500 that does NOT name a precondition is the historical shape above,
    /// where re-uploading is a remedy this CLI has watched work.
    ///
    /// **A success here is a job QUEUED, not bytes delivered.** This command
    /// renders what the API returns and re-checks nothing, so confirm the
    /// outcome by reading the node's `import_state.status` (see `upload
    /// file`) or `list-writebacks`. That matters most for a case that is
    /// **unverified**: pushing an imported FOLDER is said to be accepted with
    /// HTTP 200 and to fail later in the executor, the endpoint having no
    /// file-type gate — but confirming the outcome covers it either way.
    ///
    /// **Measured on a node whose own write-back had already completed, both
    /// times.** Re-driving one that never completed is the case an agent
    /// actually reaches for, and it is NOT measured here — the state needed to
    /// test it is a race this CLI cannot provoke on demand.
    ///
    /// A node MOVED OUT of its graft is a different case and fails cleanly
    /// (`Node is not under this import source`) — moving a file out does not
    /// detach it, but it does put it beyond this command's reach. **That was
    /// measured in one direction only: a moved-out node produces that message.
    /// Do not run it backwards** — the same refusal is reported to cover an
    /// ancestor that could not be loaded and a depth bound being hit, so
    /// receiving it does not establish that your node was moved anywhere.
    #[command(name = "push-writeback")]
    PushWriteback {
        /// Source ID.
        source_id: String,
        /// Node ID.
        #[arg(long)]
        node_id: String,
    },
    /// Retry a failed write-back — and ONLY a failed one.
    ///
    /// **Measured 2026-08-25: a `completed` row is REFUSED**, with the
    /// server's own message — *"Only failed write-back jobs can be retried."*
    /// It errors rather than quietly doing nothing, which is the better of the
    /// two failure modes.
    ///
    /// **THIS refusal is permanent, not transient** (measured
    /// 2026-08-25: it arrives as an HTTP 409). Nothing is down — the
    /// row is simply in a state this command does not accept, so repeating
    /// the call unchanged will never succeed. **The server's message tells
    /// you only that the row is not `failed` — never which state it IS in**,
    /// so read the row itself (`list-writebacks`) if you need that. To make
    /// progress you need a `failed` row, not another attempt at this one.
    ///
    /// **Do NOT generalise that into "a 409 from this command is settled."**
    /// Reported from the platform side and NOT yet deployed when this was
    /// written: a retry can also be refused with 409 because another live
    /// write-back already covers the node — and that one is NOT settled, it
    /// clears by itself once the other write-back finishes. **Re-read the
    /// row before concluding any 409 here is final.**
    ///
    /// Reported from the platform side and NOT measured here: a successful
    /// retry resets the attempt count, clears the stored error, and clears the
    /// marker that had written the row off — then RE-ENQUEUES. So a success is
    /// the job being queued again, not the bytes having arrived; confirm that
    /// the way you would any other write-back, by reading the node's
    /// `import_state.status` (see `upload file`).
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

// ─── Agent Intents ───────────────────────────────────────────────────────────

/// Agent Intents subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum IntentsCommands {
    /// Allocate a slot. Takes no content — that is the point.
    ///
    /// Allocate when work STARTS, before you know what to write in it. An
    /// unfilled slot is a real signal to peers ("someone is starting something
    /// here"), not an incomplete write.
    ///
    /// THIS IS GET-OR-CREATE, NOT "make me a new slot". The key is your
    /// CREDENTIAL plus the scope, so with no --node-id (workspace-wide scope)
    /// you get back the workspace-wide slot that credential ALREADY holds —
    /// content and all, possibly one you never created and do not remember.
    ///
    /// COMPARE THE RETURNED `id` against the one you were holding BEFORE
    /// you overwrite or `intents release` it. Releasing an id you assumed was
    /// fresh destroys whatever that slot already carried, and nothing in the
    /// response warns you — releasing someone else's live intent returns the
    /// same clean success as releasing your own.
    ///
    /// So DO NOT "allocate a scratch slot then release it". That idiom
    /// silently deletes your own earlier workspace-wide intent.
    ///
    /// Re-allocating a slot you still hold is a pure HEARTBEAT: state,
    /// version, topic and sequence all hold while the expiry moves forward.
    /// Re-allocating one that already EXPIRED opens a new generation with a
    /// new `id` and `version` back to 0 — and nothing in the response marks
    /// that boundary either.
    Allocate {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Node this intent is about (optional scope hint).
        #[arg(long)]
        node_id: Option<String>,
        /// Intent verb. Server-validated closed set; unknown values are
        /// rejected, not ignored.
        #[arg(long)]
        intent: Option<String>,
    },
    /// Browse slots in this workspace — topics only, never message bodies.
    ///
    /// Read bodies with `intents get`, which batches many in one call.
    ///
    /// `cursor: null` does NOT mean end-of-list and is NOT the same as an
    /// empty result. The cursor only advances past the write-visibility
    /// boundary, so a page whose rows were all written in the last ~2 seconds
    /// returns `cursor: null` AND a full list.
    ///
    /// The ONLY end-of-list signal is an EMPTY list:
    ///   • rows present  -> more to read; if `cursor` is null, re-poll with the
    ///     cursor you already had. Do not stop.
    ///   • rows empty    -> end of list.
    ///
    /// A loop that stops on a null cursor truncates the feed, and does it most
    /// often on a BUSY workspace — where missing rows matter most.
    ///
    /// `sequence` is an OPAQUE token, not a count or a position. Values are
    /// not contiguous — do not render it or do arithmetic on it.
    List {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Keyset cursor from a previous page.
        ///
        /// Page size is fixed at up to 100 items server-side; this endpoint
        /// accepts NO page-size parameter, so there is deliberately no
        /// `--limit` here — one would be accepted and silently ignored.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Fill or refine a slot — and push its expiry forward.
    ///
    /// There is no renewal verb by design: FILLING IS THE HEARTBEAT, so
    /// liveness follows from doing the work rather than from claiming it. A
    /// slot you stop filling evaporates, which is intended.
    ///
    /// **A fill carrying ONLY `--version` is a valid pure KEEPALIVE** — it
    /// advances `version` and `expires_at` and leaves `state` alone, so an
    /// unfilled slot is not flipped to `filled` with a null topic. Use it to
    /// stay alive when you have nothing new to say.
    ///
    /// `--version` is REQUIRED and must be the version you actually read. A
    /// stale one is refused with 409 (`9667`). **Re-read the intent and decide
    /// again — do NOT re-send the same payload**: a blind retry overwrites
    /// whatever the peer wrote in between, which is the outcome the version
    /// check exists to prevent.
    Fill {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Intent ID returned by `intents allocate`.
        intent_id: String,
        /// Version you read from the intent.
        #[arg(long)]
        version: u64,
        /// One-line label — this is what `intents list` shows. Max 256
        /// characters (counted in characters, not bytes); tabs and newlines
        /// are rejected because it is a label.
        #[arg(long)]
        topic: Option<String>,
        /// Long-form body, max 8192 characters. Never returned by `list` —
        /// read it with `intents get`.
        #[arg(long)]
        message: Option<String>,
        /// Intent verb (server-validated closed set).
        ///
        /// A slot's SCOPE is fixed when it is allocated and cannot be changed
        /// by a fill — there is deliberately no `--node-id` here. The endpoint
        /// declares no such field, so one would be accepted and silently
        /// discarded (measured 2026-08-28: filling with a different
        /// `node_id` returned success with the scope UNCHANGED). Re-scope by
        /// releasing and allocating against the node you mean.
        #[arg(long)]
        intent: Option<String>,
    },
    /// Expand one or more intents, including their message bodies.
    ///
    /// Pass several IDs to read them in ONE request — that is what this exists
    /// for. Do not loop over `intents get` one ID at a time.
    Get {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// One or more intent IDs.
        #[arg(num_args = 1..)]
        intent_ids: Vec<String>,
    },
    /// Release a slot — the work is done.
    ///
    /// Advisory housekeeping, not a lock release. An intent you simply stop
    /// filling expires on its own, so releasing is a courtesy to peers reading
    /// the list rather than something that must succeed.
    ///
    /// Sibling agents of the same user may release each other's slots:
    /// ownership is the USER, not the agent label.
    Release {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// Intent ID.
        intent_id: String,
    },
}

// ─── Lock ────────────────────────────────────────────────────────────────────

/// File locking subcommands.
///
/// `Debug` is implemented manually (not derived) so the capability `lock_token`
/// and the free-form `client_info` are never rendered verbatim through the
/// `Cli` Debug tree (secrets must never appear in Debug output).
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
        /// Write the returned lock token to this path (created 0600). When
        /// omitted the token is redacted from output and a warning is printed.
        /// You need the token for `lock release --lock-token`.
        #[arg(long)]
        lock_token_file: Option<std::path::PathBuf>,
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
    ///
    /// A heartbeat renews for the duration the lock was ACQUIRED with — a lock
    /// taken for an hour is renewed for another hour, not shortened to a fixed
    /// amount. The renewal REPLACES the time still remaining rather than adding
    /// to it.
    ///
    /// **One exception: a lock you let LAPSE ENTIRELY is RECREATED by the
    /// heartbeat at the service DEFAULT lease, not at its original duration.**
    /// The expired lock's duration died with it and this endpoint accepts no
    /// `--duration`, so there is nothing to rebuild it from. **Read
    /// `expires_at` from the response rather than assuming**, and re-acquire
    /// with an explicit `--duration` if you need the longer lease back.
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
                lock_token_file,
            } => f
                .debug_struct("Acquire")
                .field("context_type", context_type)
                .field("context_id", context_id)
                .field("node_id", node_id)
                .field("duration", duration)
                .field("client_info", &format_args!("{}", ci(client_info.as_ref())))
                // The PATH is not the secret — the token that will be written
                // there is, and it never passes through this enum.
                .field("lock_token_file", lock_token_file)
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
        /// Records per page. The server clamps to 1-250 and then SNAPS to the
        /// nearest of 25, 100 or 250 — in both directions (62 or below -> 25,
        /// 63-175 -> 100, 176 or above -> 250), so asking for 50 returns 25 and
        /// asking for 200 returns 250. The response's `page_size` reports the
        /// size actually used.
        #[arg(long)]
        page_size: Option<u32>,
        /// Opaque cursor from a previous response's `cursor` field. Omit for
        /// the first page.
        #[arg(long)]
        cursor: Option<String>,
        /// Return only nodes with this MIME type.
        #[arg(long)]
        mimetype: Option<String>,
        /// Return only nodes with this file extension.
        #[arg(long)]
        extension: Option<String>,
        /// [deprecated] Ignored — this endpoint is cursor-paginated, not
        /// offset-paginated. Use --page-size/--cursor instead.
        #[arg(long, hide = true)]
        limit: Option<u32>,
        /// [deprecated] Ignored — this endpoint is cursor-paginated, not
        /// offset-paginated. Use --page-size/--cursor instead.
        #[arg(long, hide = true)]
        offset: Option<u32>,
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
    /// `workspace jobs-status` until the job reaches a TERMINAL state —
    /// `completed` OR `errored` (which carries `error_message`) — then read
    /// values from the metadata details endpoint (or pass --wait to do this
    /// automatically). Waiting only for "completed" never ends on a failed
    /// job. An entry MISSING from `jobs-status` is not a third terminal
    /// state and must not be read as one: terminal entries age out only
    /// after about an hour, so shortly after enqueueing, absence means the
    /// job is not visible yet. Keep polling until you see an explicit
    /// `completed` / `errored`, then stop on your own deadline and report
    /// the result as indeterminate — never as success or failure. An
    /// unscoped call for a file version already extracted answers `200`
    /// with `already_extracted` and no job — do not assume a `job_id` is
    /// always present.
    Extract {
        /// Workspace ID.
        #[arg(long)]
        workspace: String,
        /// File node ID.
        #[arg(long)]
        node_id: String,
        /// JSON-encoded array of field names. Omit for a full extraction.
        /// Naming fields makes the request EXCLUSIVE: exactly those fields
        /// are extracted and anything else the model returns is discarded
        /// rather than written. The file is still read in full, and a named
        /// field is written only if the document actually contains it. A
        /// scope is also a distinct unit of work, so an already-extracted
        /// file runs again for the fields you name — which SPENDS AI
        /// CREDITS. Every name must be a field the workspace has already
        /// produced (an unknown name is rejected, not ignored); max 50
        /// distinct names per request.
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
        /// Page size (1-100, default 100 server-side; combined with
        /// offset, must not exceed 10000).
        #[arg(long)]
        limit: Option<u32>,
        /// Skip-N offset (offset + limit may not exceed 10000).
        #[arg(long)]
        offset: Option<u32>,
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
            Self::Login {
                email,
                password: _,
                agent_name,
            } => f
                .debug_struct("Login")
                .field("email", email)
                .field("password", &"[REDACTED]")
                .field("agent_name", agent_name)
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
            Self::Close {
                email_address,
                dryrun,
            } => f
                .debug_struct("Close")
                .field("email_address", email_address)
                .field("dryrun", dryrun)
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

    /// Render one subcommand's `--help` by walking a path from the root.
    fn render_help(path: &[&str]) -> String {
        let mut cmd = Cli::command();
        for name in path {
            cmd = cmd
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("no such subcommand: {name}"))
                .clone();
        }
        cmd.render_long_help().to_string()
    }

    /// [`render_help`] with every run of whitespace collapsed to one space.
    ///
    /// Clap hard-wraps long help to the terminal width, so a phrase asserted
    /// verbatim can straddle a line break and fail for a reason that has nothing
    /// to do with the contract it guards. Normalizing lets a guard pin a phrase
    /// long enough to carry its own CONTEXT — "summary … 8192" rather than a
    /// bare "8192", which this help text is full of.
    fn normalized_help(path: &[&str]) -> String {
        render_help(path)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
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

    /// `files search` targets a workspace OR a share — exactly one, enforced by
    /// clap rather than at runtime.
    #[test]
    fn files_search_workspace_and_share_are_exclusive_and_one_is_required() {
        // --share alone parses, and leaves --workspace unset.
        let cli = Cli::try_parse_from(["fastio", "files", "search", "--share", "sh1", "q"])
            .expect("--share alone should parse");
        match cli.command {
            Commands::Files(super::FilesCommands::Search {
                workspace, share, ..
            }) => {
                assert_eq!(share.as_deref(), Some("sh1"));
                assert_eq!(workspace, None);
            }
            other => panic!("expected Files(Search), got {other:?}"),
        }

        // --workspace alone still parses — the pre-existing spelling is
        // unchanged, so this is not a breaking change for existing callers.
        let cli = Cli::try_parse_from(["fastio", "files", "search", "--workspace", "ws1", "q"])
            .expect("--workspace alone must keep working");
        match cli.command {
            Commands::Files(super::FilesCommands::Search {
                workspace, share, ..
            }) => {
                assert_eq!(workspace.as_deref(), Some("ws1"));
                assert_eq!(share, None);
            }
            other => panic!("expected Files(Search), got {other:?}"),
        }

        // Both at once is refused — they name different profiles, and silently
        // preferring one would search somewhere the caller did not ask for.
        assert!(
            Cli::try_parse_from([
                "fastio",
                "files",
                "search",
                "--workspace",
                "ws1",
                "--share",
                "sh1",
                "q",
            ])
            .is_err(),
            "--workspace and --share must conflict"
        );

        // Neither is refused too, so the mapper's target match stays total.
        assert!(
            Cli::try_parse_from(["fastio", "files", "search", "q"]).is_err(),
            "one of --workspace / --share must be required"
        );
    }

    #[test]
    fn files_search_parses_canonical_search_mode_flags() {
        let cli = Cli::try_parse_from([
            "fastio",
            "files",
            "search",
            "--workspace",
            "ws1",
            "*.pdf",
            "--search-in",
            "filename",
            "--name-match",
            "glob",
            "--case-sensitive",
        ])
        .expect("canonical search-mode flags should parse");
        match cli.command {
            Commands::Files(super::FilesCommands::Search { modes, .. }) => {
                let p = modes.to_params();
                assert_eq!(p.search_in.as_deref(), Some("filename"));
                assert_eq!(p.name_match.as_deref(), Some("glob"));
                assert_eq!(p.case_sensitive, Some(true));
            }
            other => panic!("expected Files(Search), got {other:?}"),
        }
    }

    #[test]
    fn search_mode_shorthands_resolve_to_canonical_wire_values() {
        // `--filename-only` / `--glob` are CLI sugar: they must reach the wire
        // as `search_in` / `name_match` and never as flags of their own.
        let cli = Cli::try_parse_from([
            "fastio",
            "files",
            "search",
            "--workspace",
            "ws1",
            "Quarterly*.pdf",
            "--filename-only",
            "--glob",
        ])
        .expect("shorthand flags should parse");
        match cli.command {
            Commands::Files(super::FilesCommands::Search { modes, .. }) => {
                let p = modes.to_params();
                assert_eq!(p.search_in.as_deref(), Some("filename"));
                assert_eq!(p.name_match.as_deref(), Some("glob"));
            }
            other => panic!("expected Files(Search), got {other:?}"),
        }
    }

    #[test]
    fn search_mode_flags_are_absent_by_default() {
        // Omitting every flag must produce an empty parameter set, which is what
        // keeps an existing invocation byte-identical on the wire.
        let cli = Cli::try_parse_from(["fastio", "files", "search", "--workspace", "ws1", "q"])
            .expect("plain search should parse");
        match cli.command {
            Commands::Files(super::FilesCommands::Search { modes, .. }) => {
                assert!(modes.to_params().is_empty());
            }
            other => panic!("expected Files(Search), got {other:?}"),
        }
    }

    #[test]
    fn conflicting_search_mode_flags_are_rejected() {
        for args in [
            vec!["--filename-only", "--content-only"],
            vec!["--filename-only", "--search-in", "content"],
            vec!["--glob", "--name-match", "exact"],
        ] {
            let mut argv = vec!["fastio", "files", "search", "--workspace", "ws1", "q"];
            argv.extend(args.iter().copied());
            assert!(
                Cli::try_parse_from(&argv).is_err(),
                "expected conflict rejection for {args:?}"
            );
        }
    }

    #[test]
    fn unified_search_accepts_search_mode_flags() {
        let cli = Cli::try_parse_from([
            "fastio",
            "search",
            "workspace",
            "ws1",
            "report",
            "--search-in",
            "content",
        ])
        .expect("unified search should accept search-mode flags");
        match cli.command {
            Commands::Search(SearchCommands::Workspace { modes, .. }) => {
                assert_eq!(modes.to_params().search_in.as_deref(), Some("content"));
            }
            other => panic!("expected Search(Workspace), got {other:?}"),
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
    /// Every share-capable `files` command must accept `--share`, and the
    /// two workspace-only ones must NOT.
    ///
    /// These storage routes are documented for shares as well as workspaces.
    /// Derived from the command table rather than a hand-written list, so a
    /// command added later is checked too.
    #[test]
    fn files_commands_expose_share_exactly_where_the_platform_does() {
        use clap::CommandFactory as _;

        // The published API docs list these under "Workspace-Only Features";
        // `transfer` is additionally a fenced, known-broken item.
        const WORKSPACE_ONLY: &[&str] = &["add-link", "transfer"];

        let cmd = Cli::command();
        let files = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "files")
            .expect("files subcommand");

        let mut checked = 0;
        for sub in files.get_subcommands() {
            let name = sub.get_name();
            let has_share = sub.get_arguments().any(|a| a.get_id() == "share");
            if WORKSPACE_ONLY.contains(&name) {
                assert!(
                    !has_share,
                    "`files {name}` is workspace-only and must NOT offer --share"
                );
            } else if sub.get_arguments().any(|a| a.get_id() == "workspace") {
                assert!(
                    has_share,
                    "`files {name}` takes a workspace, so it must also accept --share"
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 14,
            "expected the share-capable set to be substantial, saw {checked}"
        );
    }

    /// An invitation id must NEVER widen to accept-everything.
    ///
    /// An id naming one invitation must never fall back to accepting EVERY
    /// pending invitation — that would join every workspace and share that had
    /// invited you. Refusing the incomplete invocation is the safe direction:
    /// the user gets an error, not a silent mass-join.
    #[test]
    fn accepting_one_invitation_requires_naming_the_entity() {
        Cli::try_parse_from(["fastio", "invitation", "accept", "123"])
            .expect_err("an id without --entity-type/--entity-id must be REFUSED");

        let cli = Cli::try_parse_from([
            "fastio",
            "invitation",
            "accept",
            "123",
            "--entity-type",
            "share",
            "--entity-id",
            "456",
        ])
        .expect("a fully-specified single accept must parse");
        match cli.command {
            Commands::Invitation(super::InvitationCommands::Accept {
                invitation_id,
                entity_type,
                entity_id,
            }) => {
                assert_eq!(invitation_id.as_deref(), Some("123"));
                assert_eq!(entity_type.as_deref(), Some("share"));
                assert_eq!(entity_id.as_deref(), Some("456"));
            }
            other => panic!("expected Invitation(Accept), got {other:?}"),
        }

        // Accept-ALL stays reachable, and stays the no-argument form.
        let all = Cli::try_parse_from(["fastio", "invitation", "accept"])
            .expect("accept-all must still parse");
        match all.command {
            Commands::Invitation(super::InvitationCommands::Accept { invitation_id, .. }) => {
                assert!(invitation_id.is_none());
            }
            other => panic!("expected Invitation(Accept), got {other:?}"),
        }
    }

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

    // ── ripley surface parse tests ───────────────────────────────────────

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

    // ── billing parse tests ──────────────────────────────────────────────

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

    /// The provision help must keep the two claims that make it CORRECT — not
    /// the prose around them, which stays free to be rewritten.
    ///
    /// 1. It points at the platform-authored `instructions` field. That field
    ///    is authoritative for per-provider setup wording; this help is a copy
    ///    that can fall behind it. Delete the pointer and a reader is back to
    ///    trusting the copy, which is how the retired service-account model
    ///    survived in three repos at once.
    /// 2. It states the current model POSITIVELY — the connected account IS
    ///    the access. A blocklist of retired vocabulary is satisfied by copy
    ///    that says nothing at all, so the positive claim is what holds the
    ///    line. (The negative half lives in `commands::import`'s hint tests.)
    ///
    /// Asserted through [`normalized_help`] because clap hard-wraps this text
    /// and both phrases are long enough to straddle a break.
    #[test]
    fn provision_help_keeps_the_authoritative_instructions_pointer() {
        let help = normalized_help(&["import", "provision-identity"]).to_lowercase();

        assert!(
            help.contains("`instructions` field is the authoritative"),
            "provision help must point at the platform's own instructions: {help}"
        );
        assert!(
            help.contains("connected account is the access"),
            "provision help must state the current model, not merely omit the old one: {help}"
        );
        // The retired model may be DESCRIBED here as history — that passage is
        // deliberate — but never left standing as a live instruction.
        assert!(
            help.contains("that model is gone"),
            "the retired model must stay marked as retired wherever it is named: {help}"
        );
    }
}

#[cfg(test)]
mod metadata_surface_lock_tests {
    use super::Cli;
    use clap::CommandFactory;

    /// Every `fastio metadata` subcommand exposed today.
    ///
    /// A deliberate lock on the current surface: any addition or removal must
    /// be made here in the same change, so a metadata surface edit is always
    /// visible in review rather than silent.
    const METADATA_SUBCOMMANDS: &[&str] = &["eligible", "details", "extract", "search"];

    #[test]
    fn metadata_subcommand_surface_is_locked() {
        use std::collections::BTreeSet;

        let cmd = Cli::command();
        let metadata = cmd
            .find_subcommand("metadata")
            .expect("`metadata` subcommand registered");
        // Clap injects its own `help` subcommand during build; it is not part
        // of the metadata surface being locked.
        let actual: BTreeSet<&str> = metadata
            .get_subcommands()
            .map(clap::Command::get_name)
            .filter(|name| *name != "help")
            .collect();
        let expected: BTreeSet<&str> = METADATA_SUBCOMMANDS.iter().copied().collect();

        let missing: Vec<&str> = expected.difference(&actual).copied().collect();
        let extra: Vec<&str> = actual.difference(&expected).copied().collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "`fastio metadata` surface changed — missing: {missing:?}, unexpected: {extra:?}"
        );
        assert_eq!(
            expected.len(),
            METADATA_SUBCOMMANDS.len(),
            "METADATA_SUBCOMMANDS contains a duplicate"
        );
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

#[cfg(test)]
mod content_surface_tests {
    use super::{Cli, Commands, OrgCommands, SearchCommands, WorkspaceCommands};
    use clap::Parser;

    /// Helper: parse argv into a [`Cli`], panicking on a parse error with the
    /// clap message (so the cause is visible).
    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    // ─── `files content` window rules ──────────────────────────────────────
    //
    // clap owns WHICH FLAGS may be combined; `ContentReadParams::validate` owns
    // every VALUE BOUND and is shared with the MCP server. These lock the clap
    // half, which is the half a bad combination would otherwise reach the wire
    // through — the api-layer validator never sees a `--page` that clap allowed
    // beside a `--query`, because the builder can hold only one of each.

    /// The single-file form takes a positional `NODE_ID` and the whole window
    /// vocabulary, and reaches the parsed variant with each value intact.
    #[test]
    fn files_content_parses_a_single_file_range_read() {
        let parsed = cli(&[
            "fastio",
            "files",
            "content",
            "--workspace",
            "123",
            "node-1",
            "--chunk-from",
            "40",
            "--chunk-to",
            "42",
            "--detail",
            "standard",
        ]);
        // Chunk verbosity rides on the GLOBAL --detail flag (there is no
        // per-command --output; on this CLI that name means an output file).
        assert_eq!(parsed.detail.as_deref(), Some("standard"));
        match parsed.command {
            Commands::Files(super::FilesCommands::Content {
                workspace,
                node_id,
                nodes,
                chunk_from,
                chunk_to,
                ..
            }) => {
                assert_eq!(workspace.as_deref(), Some("123"));
                assert_eq!(node_id.as_deref(), Some("node-1"));
                assert!(nodes.is_none(), "single-file form must not set nodes");
                assert_eq!(chunk_from, Some(40));
                assert_eq!(chunk_to, Some(42));
            }
            other => panic!("expected files content, got {other:?}"),
        }
    }

    /// `--nodes` accepts the comma-separated spelling AND the repeated one, and
    /// both land as the same list — the two forms the flag advertises.
    #[test]
    fn files_content_nodes_accepts_csv_and_repetition() {
        for argv in [
            vec![
                "fastio",
                "files",
                "content",
                "--workspace",
                "123",
                "--nodes",
                "a,b,c",
                "--query",
                "termination clause",
            ],
            vec![
                "fastio",
                "files",
                "content",
                "--workspace",
                "123",
                "--nodes",
                "a",
                "--nodes",
                "b",
                "--nodes",
                "c",
                "--query",
                "termination clause",
            ],
        ] {
            match cli(&argv).command {
                Commands::Files(super::FilesCommands::Content { nodes, node_id, .. }) => {
                    assert_eq!(
                        nodes.as_deref(),
                        Some(["a".to_owned(), "b".to_owned(), "c".to_owned()].as_slice()),
                        "argv: {argv:?}"
                    );
                    assert!(node_id.is_none(), "argv: {argv:?}");
                }
                other => panic!("expected files content, got {other:?}"),
            }
        }
    }

    /// Every combination clap must refuse. Each is a request that would
    /// otherwise be sent and answered `406` by the server, or — worse for
    /// `--nodes --share` — sent to a route that does not exist.
    #[test]
    fn files_content_refuses_illegal_flag_combinations() {
        let cases: &[(&str, &[&str])] = &[
            (
                "--nodes without --query",
                &["--workspace", "123", "--nodes", "a,b"],
            ),
            (
                "--nodes with --share (multi-file is workspace-only)",
                &["--share", "s1", "--nodes", "a,b", "--query", "x"],
            ),
            (
                "--nodes with a positional NODE_ID",
                &["--workspace", "123", "n1", "--nodes", "a", "--query", "x"],
            ),
            (
                "--nodes with a single-file window",
                &[
                    "--workspace",
                    "123",
                    "--nodes",
                    "a",
                    "--query",
                    "x",
                    "--cursor",
                    "c",
                ],
            ),
            (
                "two window selectors (query + page)",
                &["--workspace", "123", "n1", "--page", "2", "--query", "x"],
            ),
            (
                "two window selectors (query + chunk range)",
                &[
                    "--workspace",
                    "123",
                    "n1",
                    "--chunk-from",
                    "1",
                    "--query",
                    "x",
                ],
            ),
            (
                "two window selectors (page + chunk range)",
                &[
                    "--workspace",
                    "123",
                    "n1",
                    "--page",
                    "2",
                    "--chunk-from",
                    "1",
                ],
            ),
            (
                "cursor with query (relevance is not a walk)",
                &["--workspace", "123", "n1", "--cursor", "c", "--query", "x"],
            ),
            (
                "--chunk-to without --chunk-from",
                &["--workspace", "123", "n1", "--chunk-to", "5"],
            ),
            ("neither NODE_ID nor --nodes", &["--workspace", "123"]),
            ("neither --workspace nor --share", &["n1"]),
            (
                "both --workspace and --share",
                &["--workspace", "123", "--share", "s1", "n1"],
            ),
            (
                "a detail value the routes do not accept",
                &["--workspace", "123", "n1", "--detail", "verbose"],
            ),
        ];
        for (label, tail) in cases {
            let mut argv = vec!["fastio", "files", "content"];
            argv.extend_from_slice(tail);
            assert!(
                Cli::try_parse_from(&argv).is_err(),
                "clap must reject {label}: {argv:?}"
            );
        }
    }

    /// A NEGATIVE CONTROL for the table above: the shapes it rejects are
    /// rejected for their combination, not because `files content` refuses
    /// everything. Each of these is a legal window and must parse.
    #[test]
    fn files_content_accepts_each_window_on_its_own() {
        for tail in [
            vec!["--workspace", "123", "n1"],
            vec!["--share", "s1", "n1"],
            vec!["--workspace", "123", "n1", "--query", "x"],
            vec!["--workspace", "123", "n1", "--page", "2"],
            vec![
                "--workspace",
                "123",
                "n1",
                "--chunk-from",
                "1",
                "--chunk-to",
                "3",
            ],
            vec!["--workspace", "123", "n1", "--cursor", "abc"],
            vec![
                "--workspace",
                "123",
                "n1",
                "--limit",
                "5",
                "--max-bytes",
                "2048",
                "--detail",
                "terse",
            ],
        ] {
            let mut argv = vec!["fastio", "files", "content"];
            argv.extend_from_slice(&tail);
            assert!(
                Cli::try_parse_from(&argv).is_ok(),
                "clap must accept {argv:?}"
            );
        }
    }

    /// `--details` reaches BOTH unified-search variants. The share leg carries
    /// it deliberately: the server accepts it there and simply returns no
    /// facts, so refusing it client-side would be a second, divergent rule.
    #[test]
    fn unified_search_details_parses_on_both_targets() {
        match cli(&["fastio", "search", "workspace", "ws1", "q", "--details"]).command {
            Commands::Search(SearchCommands::Workspace { details, .. }) => {
                assert!(details, "--details must reach the workspace variant");
            }
            other => panic!("expected search workspace, got {other:?}"),
        }
        match cli(&["fastio", "search", "share", "s1", "q", "--details"]).command {
            Commands::Search(SearchCommands::Share { details, .. }) => {
                assert!(details, "--details must reach the share variant");
            }
            other => panic!("expected search share, got {other:?}"),
        }
        // Absent means absent — the flag must not default to on.
        match cli(&["fastio", "search", "workspace", "ws1", "q"]).command {
            Commands::Search(SearchCommands::Workspace { details, .. }) => {
                assert!(!details, "--details must default to off");
            }
            other => panic!("expected search workspace, got {other:?}"),
        }
    }

    /// `--metadata-extraction` is a THREE-STATE flag on all three surfaces that
    /// accept it: absent (take the server default of on), `true`, and `false`.
    /// Absent must stay `None` — an unset opt-out that parsed as `Some(false)`
    /// would silently turn extraction off for every caller who never asked.
    #[test]
    fn metadata_extraction_is_three_state_everywhere() {
        let cases: &[(&str, &[&str])] = &[
            (
                "workspace create",
                &["workspace", "create", "n", "--org", "1"],
            ),
            ("workspace update", &["workspace", "update", "ws1"]),
            (
                "org create-workspace",
                &["org", "create-workspace", "1", "n"],
            ),
        ];
        for (label, base) in cases {
            for (arg, expected) in [
                (None, None),
                (Some("true"), Some(true)),
                (Some("false"), Some(false)),
            ] {
                let mut argv = vec!["fastio"];
                argv.extend_from_slice(base);
                if let Some(v) = arg {
                    argv.push("--metadata-extraction");
                    argv.push(v);
                }
                let parsed = cli(&argv);
                let actual = match parsed.command {
                    Commands::Workspace(
                        WorkspaceCommands::Create {
                            metadata_extraction,
                            ..
                        }
                        | WorkspaceCommands::Update {
                            metadata_extraction,
                            ..
                        },
                    )
                    | Commands::Org(OrgCommands::CreateWorkspace {
                        metadata_extraction,
                        ..
                    }) => metadata_extraction,
                    other => panic!("{label}: unexpected command {other:?}"),
                };
                assert_eq!(actual, expected, "{label} with {arg:?}");
            }
        }
    }
}
