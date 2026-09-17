/// Error types for the Fast.io CLI.
///
/// Uses `thiserror` for structured error variants. Command handlers
/// convert these into `anyhow::Error` with user-friendly context.
use std::fmt;

use colored::Colorize;

/// Top-level error type for the CLI.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CliError {
    /// An error returned by the Fast.io API.
    #[error("{0}")]
    Api(#[from] ApiError),

    /// Authentication failure (missing or expired credentials).
    #[error("Authentication error: {0}")]
    Auth(String),

    /// Configuration file error.
    #[error("Configuration error: {0}")]
    Config(String),

    /// File system I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// HTTP transport error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Data parsing or serialization error.
    #[error("Parse error: {0}")]
    Parse(String),

    /// API rate limit exceeded.
    #[error("Rate limit exceeded. Retry after {retry_after_secs} seconds.")]
    RateLimit {
        /// Seconds until the rate limit resets.
        retry_after_secs: u64,
    },

    /// An asynchronously-generated artifact (e.g. a signed PDF or audit
    /// certificate) is not ready yet.
    ///
    /// Surfaced as an HTTP 404 by the server, but it is NOT a genuine
    /// not-found: the resource ids are correct, the artifact simply has not
    /// been rendered yet. This variant exists so the rendered `hint:` line is
    /// the poll-and-retry guidance (see [`CliError::suggestion`]) instead of
    /// the misleading generic-404 "Verify the ID or path is correct" — the ids
    /// are fine.
    ///
    /// It WRAPS the original [`ApiError`] BY VALUE in a plain `api` field — NOT
    /// `#[source]` / `#[from]` — so:
    /// - its `Display` is the FULL server error verbatim (`[HTTP …] … (code …)`
    ///   plus any `see:` / `resource:` details), rendered EXACTLY ONCE because
    ///   no `source()` link is generated for a plain field, so the `anyhow`
    ///   chain carries no duplicate (LV-1); and
    /// - `suggestion()` can return the poll hint instead of the inner
    ///   `ApiError`'s generic-404 hint (LV-2).
    ///
    /// Command handlers construct it via the signing error-mapping layer; the
    /// shared hint is deliberately generic (no resource wording) so the variant
    /// stays usable for any async artifact and the signing-specific phrasing
    /// lives in the mapping layer's `.context(...)`, not here.
    #[error("{api}")]
    ArtifactNotReady {
        /// The original server error, preserved verbatim for `Display` (carries
        /// the HTTP status, code, and any `documentation_url` / `resource`
        /// details). Intentionally a plain field, NOT `#[source]`, so it adds
        /// no duplicate link to the rendered `anyhow` chain.
        api: ApiError,
    },

    /// A user-supplied secret (e.g. a link password) cannot be carried in an
    /// HTTP header value.
    ///
    /// The header value is built from the secret's raw bytes via
    /// [`reqwest::header::HeaderValue::from_bytes`], which accepts any byte
    /// EXCEPT control characters (including CR/LF) and a few disallowed header
    /// bytes — so a non-ASCII UTF-8 value (e.g. `"pässwört→"`) is fine, but a
    /// value containing a newline or other control byte cannot be turned into a
    /// [`reqwest::header::HeaderValue`]. This variant is raised at the seam that
    /// builds such a header so the failure is a clear client-side validation
    /// error rather than a panic or a confusing transport error.
    ///
    /// `header` names ONLY the header that could not be built — the offending
    /// VALUE is NEVER embedded, because it is a secret (this is the whole point
    /// of failing here). The wording is deliberately resource-agnostic so the
    /// variant stays reusable for any header-bound secret; the `suggestion()`
    /// hint explains the likely cause (the value contains characters HTTP
    /// headers cannot carry) without naming any specific feature.
    #[error("invalid value for {header} header")]
    InvalidHeaderValue {
        /// The name of the header that could not be constructed (e.g.
        /// `x-ve-password`). The disallowed value is intentionally NOT carried
        /// here — it is a secret.
        header: &'static str,
    },

    /// An [`ApiError`] for which a command-layer error mapper has supplied an
    /// OVERRIDE recovery hint (or chosen to SUPPRESS the generic one).
    ///
    /// Motivation: a command-scoped mapper (e.g. a File-Share or signing mapper)
    /// re-frames a raw API error with a resource-specific `.context(...)` message
    /// — but that context is layered ON TOP OF a `CliError::Api`, which the render
    /// layer still downcasts to in order to fetch `suggestion()`. The inner
    /// `ApiError`'s GENERIC status hint (e.g. "Run `fastio auth login`" for a 401,
    /// "Verify the ID or path is correct." for a 404, the generic-403 line) then
    /// gets appended UNDERNEATH the mapper's careful wording, contradicting it.
    /// Wrapping the error in this variant lets the mapper own the rendered `hint:`
    /// line: `suggestion()` returns the override (or `None` to print no hint at
    /// all), instead of the inner `ApiError`'s status default.
    ///
    /// Like [`CliError::ArtifactNotReady`], `api` is a PLAIN field (NOT
    /// `#[source]` / `#[from]`), so:
    /// - its `Display` is the inner `ApiError`'s `Display` verbatim (`[HTTP …] …
    ///   (code …)` plus any `see:` / `resource:` details), preserving the exit
    ///   code and the full server message; and
    /// - no `source()` link is generated, so the rendered `anyhow` chain carries
    ///   no duplicate `ApiError` block (the dedup invariant in `main.rs`'s
    ///   `render_chain_dedup` is untouched).
    ///
    /// `hint` is `Option<&'static str>` so the MAPPING LAYER owns the override
    /// TEXT (passed in as a `const` from the command module) — keeping this
    /// variant RESOURCE-AGNOSTIC: it embeds no feature-specific wording itself,
    /// only relaying whatever static string the caller provided (or `None`).
    #[error("{api}")]
    MappedApi {
        /// The original server error, preserved verbatim for `Display` and for
        /// the HTTP status / code it carries. A plain field (NOT `#[source]`), so
        /// it adds no duplicate link to the rendered `anyhow` chain — identical to
        /// [`CliError::ArtifactNotReady`].
        api: ApiError,
        /// The override recovery hint the mapping layer chose. `Some(text)`
        /// replaces the inner `ApiError`'s generic status hint with `text`;
        /// `None` SUPPRESSES the hint entirely (no `hint:` line is printed). The
        /// text is owned by the caller (a command-module `const`), so this variant
        /// stays resource-agnostic.
        hint: Option<&'static str>,
    },

    /// A local CLI feature is currently disabled (e.g. behind a kill-switch /
    /// feature-flag env var), so the command was refused BEFORE any
    /// config/auth/network work.
    ///
    /// This is a purely CLIENT-side gate — it does NOT wrap a server
    /// [`ApiError`] (which is why [`CliError::MappedApi`] is the wrong vehicle:
    /// that variant carries an HTTP status / code from the server). Both the
    /// `message` and the `hint` are static, caller-supplied strings so the
    /// variant stays resource-agnostic — the feature-specific wording lives at
    /// the gate that constructs it (e.g. the cloud-import kill-switch in
    /// `main.rs`), not here. Rendering flows through the same red `error:` +
    /// yellow `hint:` path as every other `CliError`, via
    /// [`CliError::suggestion`].
    #[error("{message}")]
    FeatureDisabled {
        /// The user-facing headline (e.g. "Cloud import is not yet
        /// available."). A static string owned by the gate that constructs the
        /// variant.
        message: &'static str,
        /// The recovery hint rendered on the yellow `hint:` line (e.g. how to
        /// re-enable the feature). A static string owned by the gate.
        hint: &'static str,
    },

    /// A compare-and-swap (optimistic-concurrency) write was rejected because
    /// the target moved on since the version the caller based their change on.
    ///
    /// Surfaced when a server returns a version-mismatch on a conditional write
    /// (the caller passed an "if the current version is X" precondition and the
    /// current version is no longer X). The wording is deliberately
    /// resource-AGNOSTIC — "the target file changed" rather than naming any one
    /// feature — so the variant is reusable for any CAS write; feature-specific
    /// phrasing belongs in the command layer's `.context(...)`, never here. The
    /// `suggestion()` hint is the rebase recipe: re-fetch the latest, re-apply
    /// the change, and retry with the current version id.
    #[error(
        "the target file changed since the version you supplied (current version: {current_version})"
    )]
    VersionConflict {
        /// The version id that is now current on the server. Not a secret.
        ///
        /// The value to rebase onto — but INSPECT it before re-applying: a
        /// retried write-back can report a conflict naming the caller's own
        /// committed write, so blind re-apply duplicates the bytes. See
        /// [`HINT_VERSION_CONFLICT`].
        current_version: String,
    },
}

impl CliError {
    /// Return a human-readable suggestion for recovering from this error.
    #[must_use]
    pub fn suggestion(&self) -> Option<&'static str> {
        match self {
            Self::Auth(_) => Some("Run `fastio auth login` to sign in."),
            Self::Config(_) => {
                Some("Run `fastio configure init` to set up a profile, or check your config file.")
            }
            Self::RateLimit { .. } => {
                Some("Wait for the rate limit window to reset, then retry your request.")
            }
            Self::Api(api_err) => api_err.suggestion(),
            // The mapping layer OWNS the rendered hint: `Some(text)` overrides the
            // inner ApiError's generic status hint; `None` suppresses it entirely.
            Self::MappedApi { hint, .. } => *hint,
            Self::ArtifactNotReady { .. } => Some(HINT_ARTIFACT_NOT_READY),
            // The gate that constructs the variant owns the recovery hint.
            Self::FeatureDisabled { hint, .. } => Some(hint),
            Self::InvalidHeaderValue { .. } => Some(HINT_INVALID_HEADER_VALUE),
            Self::VersionConflict { .. } => Some(HINT_VERSION_CONFLICT),
            Self::Http(_) => {
                Some("Check your network connection and verify the API base URL is correct.")
            }
            Self::Io(_) | Self::Parse(_) => None,
        }
    }

    /// Format the error for display on stderr with colors.
    pub fn render_stderr(&self) {
        eprintln!("{} {self}", "error:".red().bold());
        if let Some(hint) = self.suggestion() {
            eprintln!("{} {hint}", "hint:".yellow().bold());
        }
    }
}

/// Shared "no active plan / credits exhausted" upgrade hint.
///
/// Single source of truth so billing, signing, and Ripley all emit
/// a consistent recovery path for HTTP 402 and code `1688`. Plan IDs/names are
/// deliberately NOT hardcoded here — callers drive them off
/// `GET /org/billing/plan/list/`. References only commands that exist in the
/// current CLI surface (`org billing plans`, `org billing subscribe`); the
/// onboarding URL is the canonical recovery path.
pub const HINT_SUBSCRIPTION_REQUIRED: &str = "No active paid plan or credits exhausted. \
     Run `fastio org billing plans` to see options, then `fastio org billing subscribe <org> --plan <id>`, \
     or visit https://go.fast.io/onboarding.";

/// Shared "feature requires a higher tier" upgrade hint (code `1695`).
pub const HINT_UPGRADE_REQUIRED: &str =
    "This feature requires a higher plan tier. See `fastio org billing plans`.";

/// Shared "credit limit reached" hint (code `1696`).
pub const HINT_CREDIT_LIMIT: &str =
    "Credit limit reached. See `fastio org billing plans`, or visit https://go.fast.io/onboarding.";

/// Shared generic "access restricted" hint (code `1670`).
///
/// Code `1670` is a GENERAL-purpose restricted/access-denied code in this API
/// (2FA restrictions, workspace/share/upload limits, feature availability,
/// etc.), so this hint is deliberately resource-agnostic. The server's own
/// `error.text` carries the specifics.
pub const HINT_RESTRICTED: &str =
    "Access restricted — your plan, role, or account state does not permit this action.";

/// Shared generic "feature/plan limit reached" hint (code `1685`).
///
/// Code `1685` is a GENERAL-purpose feature-limit / precondition-failed code in
/// this API (workspace/share/upload limits, feature availability, etc.), so
/// this hint is deliberately resource-agnostic.
pub const HINT_FEATURE_LIMIT: &str =
    "A plan or feature limit was reached for this operation; a higher plan tier may be required.";

/// Hint for `9992` — the API director finishing its path walk unmatched.
///
/// `9992` is emitted by exactly one call site: the API director's path walk,
/// which answers `9992` / "Resource not found." / 404 / "File Not Found" when
/// it finds no handler for the request path. The request never reaches a
/// handler, so **no endpoint log is written** — which is how you confirm it: a
/// uniquely tagged request leaves no trace in the endpoint logs.
///
/// **Do not generalise `9992` to a `999x`/`9xxxx` "family".** An earlier
/// revision of this constant asserted the whole series meant the edge layer
/// rejecting a RESPONSE; that is a different condition with a different code
/// (`90211` / `errNoVeHeader`), and reasoning "both start with 9, so both are
/// the edge" cost this codebase three wrong rewrites of shipped copy in one
/// afternoon. A code names a call site. The cause is always a separate
/// question.
///
/// The copy therefore names the **mechanism** (the walk ended unmatched) but
/// refuses to attribute a **side**: a path this build calls that the
/// deployment does not expose, and a deployment missing an endpoint, are
/// indistinguishable from the response alone. Both are real — the cloud-import
/// surface hit the second while every client believed the first.
///
/// Consequences encoded here deliberately:
/// - the copy sends nobody to "fix" arguments that may be correct;
/// - nothing branches application logic on the number;
/// - when the platform supplies a structured `reason`, branch on that instead
///   (see [`ApiError::field_reason`]) — never on the code.
pub const HINT_UNKNOWN_ROUTE: &str = "The server's API director finished resolving this request path without matching an endpoint. \
     The response cannot say which side is at fault: a path this build calls that the deployment does not expose, and a deployment missing that endpoint, look identical from here. \
     Report it with the exact command you ran rather than adjusting arguments on a guess.";

/// Cloud-import drive-selection hints.
///
/// The drive endpoints emit a per-call-site numeric `code` alongside a stable
/// machine-readable `reason` (`drive_required`, `drive_unknown`,
/// `drive_not_supported`, `drive_immutable`, `drive_lookup_failed`). The server
/// message already states the condition; these hints add the part the server
/// cannot know — the exact `fastio` commands that resolve it.
///
/// Code `100125` / `drive_required`: an `onedrive_business` source needs a
/// library and none was supplied.
pub const HINT_DRIVE_REQUIRED: &str = "This provider binds each source to one document library. \
     List them with `fastio import list-drives --workspace <id> --identity-id <id>` and pass the chosen `drive_id` as `--drive-id`. \
     If the list is empty, build it first with `fastio import refresh-drives`.";

/// Code `159439` / `drive_unknown`: the id is not in this identity's catalog.
///
/// Deliberately does NOT say "wrong id" — a stale catalog is the likelier
/// cause, since the id may have been valid when it was last listed.
pub const HINT_DRIVE_UNKNOWN: &str = "That drive is not in this identity's catalog — it may be stale, or belong to another connection. \
     Rebuild it with `fastio import refresh-drives`, then re-list with `fastio import list-drives` and choose again.";

/// Code `192640` / `drive_not_supported`: `drive_id` sent to a provider that
/// has no drive concept. Rejected rather than ignored, so a caller never
/// believes it selected something.
pub const HINT_DRIVE_NOT_SUPPORTED: &str = "Only `onedrive_business` uses drive selection. \
     Omit `--drive-id` for this provider.";

/// Code `170775` / `drive_immutable`: the drive is create-time only.
///
/// The imported folder tree was built against the original library, so
/// re-pointing a live source would orphan every imported node.
pub const HINT_DRIVE_IMMUTABLE: &str = "A source's drive is fixed when the source is created and cannot be changed. \
     Create a new import source for the other library.";

/// Code `143457` / `drive_lookup_failed`: the catalog read failed and the
/// server failed CLOSED rather than admitting an unvalidated drive. Retryable.
pub const HINT_DRIVE_LOOKUP_FAILED: &str = "The drive catalog could not be read, so the request was refused rather than risk an unverified drive. \
     This is usually transient — retry shortly.";

/// Code `destination_unknown`: the chosen graft folder could not be resolved.
///
/// Deliberately vague about *why*, because the server deliberately is: several
/// causes (malformed id, not a folder, not yours, trashed) funnel into one
/// bucket so a caller cannot probe for a node's existence. Suggesting "check
/// the id" would leak the very distinction the single bucket is protecting.
pub const HINT_DESTINATION_UNKNOWN: &str = "The chosen destination folder could not be used.      Confirm it is a folder you can reach in this workspace (files and deleted folders are not valid destinations), then retry.";

/// Code `destination_nested`: the target sits inside an existing import graft.
pub const HINT_DESTINATION_NESTED: &str = "That folder is already part of an import.      Choose a folder outside any existing import tree.";

/// Code `destination_immutable`: the destination is create-time only.
pub const HINT_DESTINATION_IMMUTABLE: &str = "A source's destination is fixed when the source is created.      Move the imported folder in storage instead of changing the source.";

/// Map a server `reason` to its recovery hint.
///
/// The single place that knows the reason vocabulary. Unknown reasons return
/// `None` so the caller falls through to the code/status hints rather than
/// inventing guidance for a condition this build does not know about.
#[must_use]
fn reason_hint(reason: &str) -> Option<&'static str> {
    match reason {
        "drive_required" => Some(HINT_DRIVE_REQUIRED),
        "drive_unknown" => Some(HINT_DRIVE_UNKNOWN),
        "drive_not_supported" => Some(HINT_DRIVE_NOT_SUPPORTED),
        "drive_immutable" => Some(HINT_DRIVE_IMMUTABLE),
        "drive_lookup_failed" => Some(HINT_DRIVE_LOOKUP_FAILED),
        "destination_unknown" => Some(HINT_DESTINATION_UNKNOWN),
        "destination_nested" => Some(HINT_DESTINATION_NESTED),
        "destination_immutable" => Some(HINT_DESTINATION_IMMUTABLE),
        _ => None,
    }
}

/// Wrong email or password at sign-in — the code that actually lands in
/// `error.code`.
///
/// **The published error table documents this as `1650 (Authentication
/// Invalid)`.** That is the error CLASS constant; the value on the wire is
/// this per-call-site code. **Measured 2026-08-23** — a failed sign-in
/// returns `10008`, never `1650`. Keying on the documented number yields code
/// that never fires. Same hazard as the six may-never-fire arms in
/// [`ApiError::suggestion`].
pub(crate) const ERR_CREDENTIALS_INVALID: u32 = 10008;

/// The per-account failed-login lockout (HTTP 429) — the code that actually
/// lands in `error.code`.
///
/// Documented in the same table as `1671 (Rate Limited)` — again the CLASS
/// constant. The prose note twelve lines below that table gives the correct
/// value, contradicting the table in the same document. **Measured
/// 2026-08-23**: `10760`.
///
/// Distinct from the per-IP rate limit (`10368`), which is also a 429 but
/// reflects request volume from an address rather than failed credentials for
/// one account — and from the administrative "Your account is locked" 401, which
/// a client cannot wait out.
pub(crate) const ERR_LOGIN_LOCKED: u32 = 10760;

/// `ApiError::error_code` marker meaning **the server's error body could not be
/// read**, so its reason is unknown — as distinct from a response that genuinely
/// carried no body.
///
/// Bounding the error-body read (byte cap + deadline) introduced a state that
/// had not existed before: "there IS a reason, we just could not obtain it."
/// Without a marker it is indistinguishable from a bodyless response, and
/// downstream classifiers that infer meaning from the HTTP status alone then
/// draw a conclusion the server never supported.
///
/// The concrete regression this prevents: a slow-but-valid signing `404`
/// carrying `9992` ("route not found") degraded to `code: 0`, sailed
/// past the `code != 9992` exclusion in the signing mapper, and was classified
/// `ArtifactNotReady` — telling a user or agent to poll for something that can
/// never appear.
///
/// **Any classifier that keys on HTTP status must treat this marker as "unknown",
/// not as a normal instance of that status.**
pub const ERR_BODY_UNAVAILABLE: &str = "ERR_ERROR_BODY_UNAVAILABLE";

/// Coerce a JSON value to `u64`, accepting a string-encoded number.
///
/// The platform string-encodes some numerics (`"code": "400"` — see
/// `ApiClient::extract_error`), so a bare `as_u64()` silently drops real values.
/// Shared so that every consumer of a numeric `error.params` field uses the SAME
/// coercion: `ApiClient::rate_limit_error` otherwise carries its own weaker
/// parse, which fails OPEN to a 60-second default where
/// [`ApiError::param_u64`] fails CLOSED — an
/// unparseable lockout wait would silently reinstate the very
/// "60 seconds for a 30-minute lockout" bug the body-first read was written to
/// fix.
///
/// Rejects (yielding `None`, never a guess): floats, negatives, values above
/// `u64::MAX`, and non-numeric strings.
#[must_use]
pub(crate) fn json_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
}

/// Strip the request URL from a `reqwest` error.
///
/// `reqwest` attaches the request URL to errors raised by BUFFERED body reads
/// (`bytes()`, `text()`, `json()`). Request URLs in this crate can carry
/// short-lived capability tokens in their query string — upload, download and
/// lock tokens — so an error rendered into stderr, into a `tracing` field, or
/// into an MCP error payload would publish a live credential.
///
/// [`reqwest::Error::without_url`] removes only the URL: the error kind and the
/// whole source chain are preserved, so the real cause is still reported.
#[must_use]
pub(crate) fn without_request_url(e: reqwest::Error) -> reqwest::Error {
    e.without_url()
}

/// Code `1680` — the platform's **generic** access-denied code (`APP_DENIED`).
///
/// **The wording here is deliberately resource-agnostic.** `1680` is
/// `APP_DENIED` and is emitted from many unrelated surfaces — org transfer
/// tokens ("caller is not an agent account"), OAuth provider refusals,
/// e-signing (signer-token access, and editing a non-draft envelope), audit-log
/// queries missing admin permission, and metadata templates in use. Wording
/// this hint for one domain would tell a user rejected by any of the others
/// about a subsystem they never touched — the same hazard as **domain-specific
/// wording on a code reused across domains** seen on `240731`. The phrasing
/// matches [`HINT_ENTITY_MEMBERSHIP`] and [`HINT_RESOURCE_ACCESS`].
///
/// The cloud-import ownership rule (grafting is identity-owner only, no admin
/// bypass; write-back is owner-or-admin) is real — it belongs in the import
/// command's own documentation, not on a code shared with five other
/// subsystems.
pub const HINT_ACCESS_DENIED: &str = "You are authenticated, but not permitted to perform this action on this resource — this is an authorization failure, not a login problem. \
     Common causes: the resource belongs to another account, the action requires ownership rather than membership, or it requires an admin role you do not hold. \
     Check who owns the target and what role the action requires.";

/// The MEMBERSHIP reading of `10545` — the HTTP **401** axis.
///
/// `10545` is `ERROR_ORG_NOT_AUTHORIZED`: the caller is authenticated but has
/// not been granted access to the target profile. The shared request validator
/// emits it for every profile type (org, workspace, share, file-share,
/// sign-envelope), so
/// the wording is entity-agnostic and must NOT name only one of them — nor
/// mention signing, since it is not signing-specific.
///
/// It exists so the rendered `hint:` line steers to the access problem instead
/// of the misleading generic-401 "run `fastio auth login`" suggestion (the
/// caller IS authenticated).
///
/// **This is only half the code.** `10545` is also emitted on the token-SCOPE
/// axis, which answers **403** since 2026-08-23 → [`HINT_ENTITY_SCOPE`]. The
/// split is made in [`ApiError::suggestion`], which is the one place in this
/// file that keys on STATUS rather than code, and for that reason.
///
/// The wording must not name a single entity: `10545` has been measured live on
/// `/org/{id}/details/` and `/org/{id}/members/list/`, so calling it a "generic
/// workspace-access code" would send the reader to check the wrong ID.
///
/// It also covers TWO access failures, not just missing membership. The shared
/// request validator emits `10545` both when the caller is not a member AND
/// when a member does not meet a required permission threshold — so "you are
/// not a member" alone over-claims, and would tell an existing member with an
/// insufficient role to get themselves added again.
pub const HINT_ENTITY_MEMBERSHIP: &str = "Your account does not have access to this resource — either it is not a member, or its role does not meet the level this action requires. \
     Ask an admin of the org, workspace, or share to add you or grant the required role, and verify the ID you passed.";

/// The SCOPE reading of `10545` — see [`ApiError::suggestion`] for why one code
/// needs two hints.
///
/// `10545` is emitted from four sites on two different axes: "you are not a
/// member" (membership) and "this token is not scoped for it" (credential).
/// After the 2026-08-23 flip the membership sites keep HTTP 401 while the
/// scope site answers 403 — so the STATUS is what tells them apart, and the
/// membership advice ("ask an admin to add you") is actively wrong for the
/// scope case: no admin action can widen a token's scope.
pub const HINT_ENTITY_SCOPE: &str = "Your credential is not scoped for this org or workspace — this is NOT a membership problem, so being added will not help, and retrying with the SAME credential cannot add scope. \
     Restricted credentials (scoped API keys) reach only what they were issued for; a full account sign-in may have broader access.";

/// Insufficient token SCOPE for the target workspace (code `10560`).
///
/// Sibling of [`HINT_ENTITY_MEMBERSHIP`] and in the same "HTTP 401 that is
/// NOT a missing login" class, but a distinct cause: the caller is
/// authenticated **and** may well be a member — the *credential* is simply
/// NOTE: room-agent keys were RETIRED with Coordination Rooms (2026-08-25) —
/// the class no longer exists server-side. The observations below are kept as
/// the ORIGINAL EVIDENCE for this hint, not as a current credential list; the
/// user-facing strings name only scoped API keys.
///
/// scoped to other workspaces. A restricted key (room-agent key, scoped API
/// key) hits this while a full-account credential for the same user would not.
///
/// Found live 2026-08-08: `room state` against a workspace outside the test
/// key's scope returned `10560` and the generic 401 fallback told the user to
/// *"run `fastio auth login`"* — advice that cannot help, since re-running
/// login mints the same restricted credential.
///
/// Carries the same caveat as [`HINT_SCOPE_INCORRECT`] — "signing in again will
/// not help" is not generally true. Retrying with the SAME credential cannot
/// add scope, but a restricted room-agent/API key can often be replaced by a
/// full-account PKCE sign-in that legitimately has broader access. The two are
/// worded together, because fixing one and leaving the other is how a corrected
/// claim quietly survives.
pub const HINT_TOKEN_SCOPE: &str = "Your credential is not scoped to this workspace — this is NOT a login problem, and retrying with the SAME credential cannot add scope. \
     Restricted credentials (scoped API keys) only reach the workspaces they were issued for, so a full account sign-in may have broader access. \
     Use a credential scoped to this workspace, or verify the workspace ID.";

/// Insufficient token SCOPE for the requested endpoint (code `10175`,
/// `ERROR_AUTHORIZATION_SCOPE_INCORRECT`).
///
/// Third member of the same "authenticated, but this credential is not permitted
/// here" family as [`HINT_ENTITY_MEMBERSHIP`] and [`HINT_TOKEN_SCOPE`], and
/// the one with the sharpest failure mode: the platform's central scope gate
/// emits `10175` when a credential is valid and current but its SCOPES do not
/// cover the endpoint.
///
/// **Keyed on the CODE, never the HTTP status, and that is deliberate.**
/// `10175` shipped as HTTP **401** (`APP_AUTH_INVALID`) and moves to HTTP
/// **403** (`APP_FORBIDDEN`) — established with the backend on 2026-08-23, per
/// RFC 6750 (`insufficient_scope` SHOULD be
/// 403). Both statuses have a misleading generic fallback and this arm must
/// outrank both:
/// - under **401**, [`ApiError::suggestion`]'s fallback says *"run `fastio auth
///   login`"* — which is the actively harmful one. The CLI stores the
///   `twofactor`-scoped token issued at sign-in BEFORE the second factor is
///   supplied (`commands/auth.rs:277-291`, checked at `:293`), so any ordinary
///   command on a 2FA account hits this gate. Following that advice spends
///   **another counted login attempt**, and the platform locks the account after
///   five. An automated client can hit the self-driving form of this loop; the
///   CLI does not loop by itself, it *instructs the user to*.
/// - under **403**, the fallback says *"check that your account has the required
///   role"* — harmless but wrong: a scope failure is a property of the
///   CREDENTIAL, not of the account's role.
///
/// Keying on the status would therefore have been wrong before the change and
/// wrong after it, in opposite directions.
/// **Two corrections, both dated 2026-08-23 — do not reintroduce either.**
///
/// 1. This hint recommended `fastio auth 2fa verify <code>`. **That command does
///    not exist**: the flag is required (`--code <CODE>`), and the positional
///    form exits with `unexpected argument`. Verified by running it. A recovery
///    hint naming an invalid command is worse than no hint — the user follows it
///    and gets a second, unrelated error. The test asserted only the substring
///    `"2fa verify"`, so it passed. It now pins the full invocation.
/// 2. It stated that "a new token carries the same scopes." **Not generally
///    true**: a restricted API key or room-agent key supplied via `--token` can
///    often be replaced by signing in via PKCE for a broader account credential.
///    What IS always true is the narrower claim — retrying with the SAME
///    credential cannot add scopes.
pub const HINT_SCOPE_INCORRECT: &str = "Your credential is valid, but its scope does not permit this endpoint — this is NOT a login problem, and retrying with the SAME credential cannot add scopes. \
     If you signed in to a 2FA-enabled account, the stored token stays limited until you finish verifying: run `fastio auth 2fa verify --code <CODE>`. \
     Otherwise use a credential whose scopes cover this operation (a restricted API key reaches only what it was issued for; a full account sign-in may have broader scopes).";

/// Scope / access-mode refusal hints (HTTP 403).
///
/// The server enriches these refusals with an OBJECT-shaped `error.params`
/// carrying a stable `reason` plus the `credential_type` that produced the
/// refusal, so the recovery differs by BOTH. The reason picks the family; the
/// credential type picks the member. The `reason` is AUTHORITATIVE: it is what
/// selects a hint whenever it is present, and it outranks the numeric code —
/// see [`ApiError::field_reason`] for why. Codes 10767/10768/10769 are keyed
/// only as an ABSENCE fallback, reached when the refusal carries no `params` at
/// all; each then yields the generic member of its family, since the credential
/// type is unknowable without the reason object.
///
/// The wording deliberately never calls admin a "role" and never tells the
/// reader to ask someone else to grant it: an access mode is a property of the
/// CREDENTIAL, so the fix is always to re-issue or re-consent the credential.
///
/// `scope_admin_required`, `credential_type: api_key` — the key was issued
/// without an `rwa` access mode for the target entity.
/// **The update is a WHOLESALE REPLACEMENT.** `POST /user/auth/key/{id}/`
/// replaces the key's entire scope set with the array it is sent, so a hint
/// that names only the missing scope would talk the reader into deleting every
/// other scope the key holds. The wording therefore sends them to read the key
/// first and re-state everything it should keep.
///
/// **`--admin` is applied to EVERY scope the invocation names**, so re-stating a
/// mixed-mode key with the selector flags silently escalates its read-only
/// scopes to admin. The hint therefore names the raw `--scopes` form as the
/// escape hatch for a key whose scopes do not all share one access mode.
pub const HINT_SCOPE_ADMIN_REQUIRED_API_KEY: &str = "This API key has no admin (rwa) access mode for that resource. \
     Re-issue it with admin — but an update REPLACES the key's whole scope set, so run `fastio auth api-key get <key-id>` first, then run `fastio auth api-key update <key-id>` and re-specify every scope it should keep — `--org <id>`, `--workspace <id>`, `--share <id>` or `--all` — alongside `--admin`, from a credential that already holds that access. \
     If the key's scopes do not all use the same access mode, re-state them verbatim with `--scopes` instead: `--admin` applies admin to every scope the invocation names, which would escalate a read-only one. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_admin_required`, `credential_type: oauth` — the consent page was not
/// asked for, or did not grant, admin.
pub const HINT_SCOPE_ADMIN_REQUIRED_OAUTH: &str = "This login has no admin (rwa) access mode for that resource. \
     Run `fastio auth login --admin` and approve admin access on the consent page, then retry. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_admin_required`, `credential_type: session` — a web session whose
/// grant does not include admin.
pub const HINT_SCOPE_ADMIN_REQUIRED_SESSION: &str = "This session has no admin (rwa) access mode for that resource. \
     Run `fastio auth login --admin` and approve admin access on the consent page, then retry. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_admin_required`, credential type absent or unrecognised — names both
/// recovery paths because the response did not say which credential this is.
pub const HINT_SCOPE_ADMIN_REQUIRED_GENERIC: &str = "The credential has no admin (rwa) access mode for that resource. \
     Run `fastio auth login --admin` and approve admin access, or issue a key with admin — `fastio auth api-key create` with `--org <id>`, `--workspace <id>`, `--share <id>` or `--all`, alongside `--admin` — then retry. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_exceeds_issuer` / `access_mode_exceeds_initiate`,
/// `credential_type: api_key` — the requested grant is wider than the issuing
/// key. Both reasons share this family: they are the same refusal seen at two
/// points in the flow, and the recovery is identical.
pub const HINT_SCOPE_EXCEEDS_ISSUER_API_KEY: &str = "The requested scopes are broader than this API key. \
     Narrow the request, or issue it from a credential that already holds the access you asked for — a web session, or a fresh login with the ceiling you need (`fastio auth login --admin` for admin access, `fastio auth login --account-settings` for account settings). \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_exceeds_issuer` / `access_mode_exceeds_initiate`,
/// `credential_type: oauth`.
pub const HINT_SCOPE_EXCEEDS_ISSUER_OAUTH: &str = "The requested scopes are broader than this login's grant. \
     Narrow the request, or issue it from a credential that already holds the access you asked for — a web session, or a fresh login with the ceiling you need (`fastio auth login --admin` for admin access, `fastio auth login --account-settings` for account settings). \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_exceeds_issuer` / `access_mode_exceeds_initiate`,
/// `credential_type: session`.
pub const HINT_SCOPE_EXCEEDS_ISSUER_SESSION: &str = "The requested scopes are broader than this session's grant. \
     Narrow the request, or issue it from a credential that already holds the access you asked for — a web session, or a fresh login with the ceiling you need (`fastio auth login --admin` for admin access, `fastio auth login --account-settings` for account settings). \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_exceeds_issuer` / `access_mode_exceeds_initiate`, credential type
/// absent or unrecognised.
pub const HINT_SCOPE_EXCEEDS_ISSUER_GENERIC: &str = "The requested scopes are broader than the credential issuing them. \
     Narrow the request, or issue it from a credential that already holds the access you asked for — a web session, or a fresh login with the ceiling you need (`fastio auth login --admin` for admin access, `fastio auth login --account-settings` for account settings). \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `userdetails_scope_required`, `credential_type: api_key` — the key lacks
/// `userdetails:*:rw`. Never mentions admin: account settings are a separate
/// scope, not a wider access mode.
pub const HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY: &str = "This API key cannot change account settings. \
     Add the account-settings scope — but an update REPLACES the key's whole scope set, so run `fastio auth api-key get <key-id>` first, then run `fastio auth api-key update <key-id>` and re-specify every scope it should keep — `--org <id>`, `--workspace <id>`, `--share <id>` or `--all` — alongside `--account-settings`, from a credential that already holds that access. \
     Then retry.";

/// `userdetails_scope_required`, `credential_type: oauth`.
pub const HINT_USERDETAILS_SCOPE_REQUIRED_OAUTH: &str = "This login cannot change account settings. \
     Run `fastio auth login --account-settings` and approve account settings on the consent page, then retry.";

/// `userdetails_scope_required`, `credential_type: session`.
pub const HINT_USERDETAILS_SCOPE_REQUIRED_SESSION: &str = "This session cannot change account settings. \
     Run `fastio auth login --account-settings` and approve account settings on the consent page, then retry.";

/// `userdetails_scope_required`, credential type absent or unrecognised —
/// names the scope itself plus both recovery paths.
pub const HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC: &str = "Account settings changes need the `userdetails:*:rw` scope. \
     Run `fastio auth login --account-settings`, or add it to a key — a key update REPLACES the whole scope set, so run `fastio auth api-key get <key-id>` first, then run `fastio auth api-key update <key-id>` and re-specify every scope it should keep — `--org <id>`, `--workspace <id>`, `--share <id>` or `--all` — alongside `--account-settings`. \
     Then retry.";

/// `scope_write_required`, `credential_type: api_key` — every scope mode on the
/// key is `r`, so it may read the account but not mutate it. Write is NOT
/// admin: the fix is a read-write access mode, never `--admin`, which asks for
/// a strictly higher ceiling the operation does not need.
///
/// Like the other API-key hints, the update is a WHOLESALE REPLACEMENT, so the
/// wording sends the reader to read the key back before re-stating it.
pub const HINT_SCOPE_WRITE_REQUIRED_API_KEY: &str = "This API key is read-only (access mode r) for account operations. \
     Re-issue it read-write — but an update REPLACES the key's whole scope set, so run `fastio auth api-key get <key-id>` first, then run `fastio auth api-key update <key-id>` and re-specify every scope it should keep — `--org <id>`, `--workspace <id>`, `--share <id>` or `--all` — WITHOUT `--read-only`, from a credential that already has that access. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_write_required`, `credential_type: oauth` — the login was consented
/// read-only.
pub const HINT_SCOPE_WRITE_REQUIRED_OAUTH: &str = "This login is read-only (access mode r) for account operations. \
     Sign in again without `--read-only` — run `fastio auth login` and approve the request, then retry. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_write_required`, `credential_type: session` — a web session whose
/// grant is read-only.
pub const HINT_SCOPE_WRITE_REQUIRED_SESSION: &str = "This session is read-only (access mode r) for account operations. \
     Sign in again without `--read-only` — run `fastio auth login` and approve the request, then retry. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// `scope_write_required`, credential type absent or unrecognised — names both
/// recovery paths because the response did not say which credential this is.
pub const HINT_SCOPE_WRITE_REQUIRED_GENERIC: &str = "The credential is read-only (access mode r) for account operations. \
     Sign in again without `--read-only` (`fastio auth login`), or issue a read-write key — a key update REPLACES the whole scope set, so run `fastio auth api-key get <key-id>` first, then run `fastio auth api-key update <key-id>` and re-specify every scope it should keep — `--org <id>`, `--workspace <id>`, `--share <id>` or `--all` — without `--read-only`. \
     Run `fastio auth scopes` to see what the current credential holds.";

/// Map a scope/access-mode refusal `reason` plus the refusing `credential_type`
/// to its recovery hint.
///
/// Separate from [`reason_hint`] because these four reasons need a SECOND
/// discriminator: the same refusal has a different recovery for an API key
/// (re-issue it), an OAuth grant (re-consent), and a web session. Unknown
/// reasons return `None` so the caller falls through to the reason/code/status
/// table unchanged.
#[must_use]
fn scope_refusal_hint(reason: &str, credential_type: Option<&str>) -> Option<&'static str> {
    match reason {
        "scope_admin_required" => Some(match credential_type {
            Some("api_key") => HINT_SCOPE_ADMIN_REQUIRED_API_KEY,
            Some("oauth") => HINT_SCOPE_ADMIN_REQUIRED_OAUTH,
            Some("session") => HINT_SCOPE_ADMIN_REQUIRED_SESSION,
            _ => HINT_SCOPE_ADMIN_REQUIRED_GENERIC,
        }),
        "scope_exceeds_issuer" | "access_mode_exceeds_initiate" => Some(match credential_type {
            Some("api_key") => HINT_SCOPE_EXCEEDS_ISSUER_API_KEY,
            Some("oauth") => HINT_SCOPE_EXCEEDS_ISSUER_OAUTH,
            Some("session") => HINT_SCOPE_EXCEEDS_ISSUER_SESSION,
            _ => HINT_SCOPE_EXCEEDS_ISSUER_GENERIC,
        }),
        "userdetails_scope_required" => Some(match credential_type {
            Some("api_key") => HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY,
            Some("oauth") => HINT_USERDETAILS_SCOPE_REQUIRED_OAUTH,
            Some("session") => HINT_USERDETAILS_SCOPE_REQUIRED_SESSION,
            _ => HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC,
        }),
        "scope_write_required" => Some(match credential_type {
            Some("api_key") => HINT_SCOPE_WRITE_REQUIRED_API_KEY,
            Some("oauth") => HINT_SCOPE_WRITE_REQUIRED_OAUTH,
            Some("session") => HINT_SCOPE_WRITE_REQUIRED_SESSION,
            _ => HINT_SCOPE_WRITE_REQUIRED_GENERIC,
        }),
        _ => None,
    }
}

/// Wrong email or password at sign-in (code `10008`).
///
/// **Found live 2026-08-23**, while end-to-end testing
/// [`HINT_SCOPE_INCORRECT`] — not by reading code. A failed
/// `fastio auth login` rendered:
///
/// ```text
/// error: login failed: [HTTP 401] Your credentials supplied are invalid. (code 10008)
///   4 of 5 sign-in attempts remaining before a temporary lockout.
/// hint: Authentication failed. Run `fastio auth login` to sign in.
/// ```
///
/// **That transcript is HISTORICAL and is kept verbatim as the evidence.** The
/// generic 401 fallback has since been rewritten to be action-only and
/// cause-free, so it no longer renders those words — but this override is
/// still required, because a cause-free hint is not the same as a *specific*
/// recovery, and this code has one.
///
/// The generic 401 fallback told a user who had **just run `fastio auth login`**
/// to run `fastio auth login` — advice that cannot help (the credentials are
/// simply wrong) and that spends **another of the four remaining attempts**, five
/// of which lock the account for 30 minutes. Same trap as `10175`, reached
/// through a different code: the hint is not merely useless, it is pointed at
/// the one action that makes the situation worse.
///
/// In the three-meanings taxonomy of the platform's 401 surface (2026-08-23)
/// this is the third case — *"these credentials are wrong: don't retry, don't
/// clear, count it"* — as distinct from `10011`
/// ("credential is dead", clear it) and `10175` ("real but not authorized here",
/// keep it). All three ship as HTTP 401 today, so only the CODE separates them.
pub const HINT_CREDENTIALS_INVALID: &str = "The email or password is incorrect — re-running `fastio auth login` with the same details will fail again. \
     Check both, and note that each failed attempt counts toward a temporary account lockout. \
     If you have forgotten the password, reset it rather than retrying.";

/// The per-account failed-login lockout (code `10760`, HTTP 429).
///
/// Distinct from the generic 429 hint ("Rate limited. Wait a moment and try
/// again.") in the one way that matters: **"a moment" is up to 30 minutes here**,
/// and the lockout is per-ACCOUNT rather than per-address, so switching networks
/// does not clear it. Retrying during the window does not extend the lockout but
/// does not succeed either, so there is nothing to gain by hammering.
///
/// Deliberately does NOT state the wait duration — [`ApiError::lockout_note`]
/// renders that from `error.params.retry_after_seconds` when the server supplies
/// it, and inventing a number here would be wrong on exactly the path where the
/// server declined to give one.
pub const HINT_LOGIN_LOCKED: &str = "Too many failed sign-in attempts — this account is temporarily locked, and further attempts during the lockout will not succeed. \
     The lock is per-ACCOUNT, so retrying from a different network or machine will not clear it. \
     Wait for it to expire (a successful sign-in afterwards resets the failure count), or reset your password if you no longer know it.";

/// Shared "resource access not granted" hint (code `115069`).
///
/// Code `115069` is an access-denied code (the caller is authenticated but the
/// specific resource is not shared with their account). This hint is
/// deliberately resource-agnostic and must NOT mention signing; it replaces the
/// misleading generic-401 "run `fastio auth login`" suggestion.
pub const HINT_RESOURCE_ACCESS: &str = "Access to this resource is not granted to your account. \
     Verify the resource ID and that you have permission on it.";

/// Shared "asynchronously-generated artifact not ready" hint
/// ([`CliError::ArtifactNotReady`]).
///
/// The server returns HTTP 404 until the artifact (a signed PDF, audit
/// certificate, etc.) is rendered, but it is NOT a genuine not-found — the ids
/// are correct. This hint therefore must NOT steer the user to re-check the id
/// (the generic-404 "Verify the ID or path is correct."): the recovery is to
/// poll and retry. Kept resource-agnostic (no "sign" wording) so it stays a
/// generic hint; the signing-specific phrasing lives in the mapping layer's
/// added `.context(...)`.
pub const HINT_ARTIFACT_NOT_READY: &str = "The requested artifact is generated asynchronously and is not ready yet. \
     Poll the resource and retry once it reaches the required (terminal) stage.";

/// Shared "secret cannot be carried in an HTTP header" hint
/// ([`CliError::InvalidHeaderValue`]).
///
/// The value is sent via `HeaderValue::from_bytes`, which accepts non-ASCII
/// UTF-8 but rejects control characters (including newlines) and a few
/// disallowed header bytes — so only such bytes can trip this error. The hint
/// stays resource-agnostic — it names no specific feature — and NEVER echoes the
/// offending value (it is a secret).
pub const HINT_INVALID_HEADER_VALUE: &str = "The supplied value contains a control character or newline, which cannot be \
     carried in an HTTP header. Non-ASCII letters are fine; re-check the value and remove any control characters or line breaks.";

/// Shared generic "invalid input" hint (code `1605`).
///
/// Code `1605` ("Invalid Input") is a GENERAL-purpose 400 in this API: storage
/// `update/` (rename) and `transfer/` return it on a name conflict or invalid
/// name (corrected from an opaque HTTP 500 on 2026-06-14), but the SAME code also
/// covers an invalid AI chat `type`/`privacy`/`personality` value, a bad scope
/// combination, a metadata-template node-cap overflow, and more. A bare 400
/// yields no hint at all, so this surfaces the actionable shape — fix the
/// offending value and retry — while staying resource-agnostic (it must NOT name
/// a single feature or assume "name conflict"); the server's `error.text` (and
/// any `param`/`reason` detail) carries the specific cause.
pub const HINT_INVALID_INPUT: &str = "The server rejected a value in this request as invalid or conflicting. \
     Check the error message above for the offending field (e.g. a name that already exists, or an out-of-range value), correct it, and retry.";

/// Comment DISPLAY-text limit (`166910`).
///
/// This is the limit an ordinary commenter actually hits — **not** the 8192
/// body limit, which rarely binds because reaching it requires mention markup
/// (markup counts toward the body limit, and is discounted from this one only
/// under the condition below).
///
/// Counted in **characters** (code points), matching the server's own wording.
///
/// This was UTF-8 BYTES until the platform's unit fix reached a given
/// deployment. **Re-verified 2026-08-07 at the exact boundary:**
/// 500 CJK characters (1 500 bytes) accepted, 501 rejected with `166910`
/// reporting "has 501 characters" — so the count in the message is now the true
/// character count. The byte-window caveat this hint used to carry has been
/// removed: it is wrong wherever the fix has landed, and the server's own
/// message is accurate there.
///
/// **The discount is CONDITIONAL.** It was stated unconditionally until
/// 2026-08-10, when a discriminating pair — identical inputs differing only by
/// a code fence — showed the qualifier:
/// 460 visible characters plus 76 characters of UNFENCED `@[file:…]` markup was
/// ACCEPTED, while the same 460 plus the SAME markup inside a fence was REJECTED
/// with `166910`. Unfenced markup is discounted; fenced markup counts in full.
/// An author who fences markup for display therefore reaches the cap sooner than
/// an unconditional statement predicts, and goes looking in the wrong place.
///
/// The CLI deliberately does not pre-check this one — computing it needs the
/// server's mention grammar *and* its fence rules.
pub const HINT_COMMENT_DISPLAY_LIMIT: &str = "The comment's visible text exceeds the server's 500-character limit. \
     Mention markup is discounted only while UNFENCED — markup inside a code fence counts in full (measured 2026-08-10). \
     The count in the message is the character count. Shorten the comment and retry.";

/// Comment BODY limit (`162417` create / `164797` update).
///
/// Counted in **characters**, never truncated. The CLI mirrors
/// this client-side in code points, so reaching it from here means the local
/// check was bypassed or the server's bound moved.
///
/// Was a byte count before the platform's unit fix; the byte-window caveat is
/// removed for the same reason as [`HINT_COMMENT_DISPLAY_LIMIT`] — see its docs
/// for the boundary verification on 2026-08-07.
pub const HINT_COMMENT_BODY_LIMIT: &str = "The comment body exceeds the server's 8192-character limit. \
     The 500-limit on visible text usually binds first. Shorten the comment and retry.";

/// Agent / application / device name rejected (`107184`).
///
/// The server says only *"The agent name provided is not valid."* — it never
/// says **why**, and returns the identical text for an over-long name and a
/// malformed one.
///
/// Verified 2026-08-08, immediately after the character-count deploy: 128
/// CJK characters (384 bytes) accepted, 129 CJK rejected, 129 ASCII rejected —
/// so the bound is **128 code points**. Measured the same boundary 22 minutes
/// earlier, pre-deploy, and it was 128 BYTES; the hint names characters because
/// that is what is live, and the unit is the thing most likely to move again.
pub const HINT_AGENT_NAME_INVALID: &str = "The server rejected the agent/application/device name without saying why. \
     The usual cause is length — the limit is 128 characters — or a disallowed character. \
     Shorten the name or simplify it, then retry.";

/// Generic per-field validation rejection (`10566`).
///
/// The server names the offending field but **never says what was wrong with
/// it** — the whole message is *"An invalid configuration was supplied in node
/// 'title': Invalid string in node'."* whether the value was too long, too
/// short, or malformed.
///
/// Verified 2026-08-07 against `share update --title`, whose bound is
/// **2-80 characters**: a 1-character title and an 81-character title both
/// return this same code and this same text, while 80 CJK characters (240
/// bytes) is accepted — so the bound is code points and `10566` is a generic
/// field-validation code, NOT a length-specific one. The hint is therefore
/// deliberately resource-agnostic: naming a cause it cannot know would be the
/// stale-mirror defect in hint form.
pub const HINT_INVALID_NODE_FIELD: &str = "The server rejected one field's value as invalid and named the field in the message, but not the reason. \
     The usual causes are a length bound (too long OR too short) or a disallowed character. \
     Check that field against its documented range and character set, then retry.";

/// Shared "a reason/comment is required" hint (server 422 `ERR_REASON_REQUIRED`).
///
/// A `reject` / `request_changes` decision (and any other decision the server
/// gates the same way) MUST carry a non-empty reason; the
/// CLI guards this client-side, but a caller that bypasses the guard (e.g. an
/// MCP client, or a value that slips past it) gets a bare 422 whose generic
/// status hint is unhelpful. This hint names the actionable fix. Kept
/// resource-agnostic (it phrases it as "a reason/comment") so it stays reusable
/// for any reason-required decision; the server's `error.text` carries specifics.
pub const HINT_REASON_REQUIRED: &str = "This action requires a non-empty reason. \
     Re-run with a comment/reason (e.g. `--comment \"<reason>\"`) explaining the decision.";

/// Shared compare-and-swap (optimistic-concurrency) conflict hint
/// ([`CliError::VersionConflict`]).
///
/// The conditional write was rejected because the target advanced past the
/// version the caller based their change on. The recovery is: re-fetch the
/// latest, CHECK whether the change is already present, and only then re-apply
/// and retry with the now-current version id. The check is not optional — see
/// the note below on retried write-back sessions. Kept resource-agnostic (no feature wording) so the variant stays
/// reusable; the conflict error's `Display` carries the current version id.
///
/// The verify-before-re-apply step is NOT defensive padding. On the async
/// write-back path a session can commit V1→V2, fail its final status write
/// WITHOUT setting a terminal state, and be RETRIED by the queue with the same
/// persisted base — which then mismatches against the node its own first
/// attempt advanced. The conflict is real, but the version it names is the
/// CALLER'S OWN LANDED WRITE. Blind "re-apply and retry" therefore stacks a
/// second version of identical bytes: the caller believes it wrote nothing and
/// writes twice. Verified against the backend source, 2026-08-24. The step is
/// cheap and correct even after that is fixed, so it stays either way.
pub const HINT_VERSION_CONFLICT: &str = "The target changed since the version you supplied. \
     Re-fetch the latest and CHECK WHETHER YOUR CHANGE IS ALREADY PRESENT before re-applying — a \
     conflict can name a version your own earlier attempt produced — then retry using the current \
     version id shown above.";

/// An error returned by the Fast.io REST API.
///
/// The optional [`ApiError::details`] field carries structured server
/// diagnostics (field-validation `params[]`, a `validation_report`, a
/// conflict/fire `reason`, plus `documentation_url`/`resource`) so command
/// handlers can surface them. The contained JSON is server diagnostics, never
/// a credential, so the derived `Debug` is safe (no token-bearing fields).
#[derive(Debug)]
#[non_exhaustive]
pub struct ApiError {
    /// Numeric API error code (e.g. 1650).
    pub code: u32,
    /// Machine-readable error identifier (e.g. `APP_AUTH_INVALID`).
    pub error_code: Option<String>,
    /// Human-readable error message.
    pub message: String,
    /// HTTP status code of the response.
    pub http_status: u16,
    /// Structured server diagnostics preserved from the error envelope
    /// (`params[]`, `validation_report`, `reason`, `documentation_url`,
    /// `resource`). `None` when the envelope carried no extra detail.
    ///
    /// Boxed to keep `ApiError` (and therefore `CliError`) small: a
    /// `serde_json::Value` is a large enum, and `Result<_, CliError>` appears
    /// on nearly every function in the crate, so an inline `Value` would
    /// bloat every `Result`'s error variant.
    pub details: Option<Box<serde_json::Value>>,
}

impl ApiError {
    /// Construct an `ApiError` with no structured `details`.
    ///
    /// Convenience constructor for the common case; equivalent to setting
    /// `details: None` on the struct literal. Keeps call sites terse now that
    /// the struct is `#[non_exhaustive]`.
    #[must_use]
    pub fn new(code: u32, error_code: Option<String>, message: String, http_status: u16) -> Self {
        Self {
            code,
            error_code,
            message,
            http_status,
            details: None,
        }
    }

    /// Attach structured server `details` to an `ApiError`.
    ///
    /// Builder companion to [`ApiError::new`] for the case where the caller has
    /// the enrichment JSON (e.g. a `params` conflict object) — needed because the
    /// struct is `#[non_exhaustive]`, so a `details: Some(..)` field cannot be set
    /// via a struct literal from outside this crate.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    /// The stable machine-readable `reason` from the first per-field entry of
    /// the `params` enrichment array, when present.
    ///
    /// The server documents `reason` as "the stable string a client branches
    /// on" while the numeric code is unique **per call site** and exists for
    /// support correlation. Those are different guarantees: consolidating
    /// several call sites into one — as the imports layer did to remove an
    /// existence oracle — collapses distinct numeric codes onto a single value
    /// without changing any `reason`. Branching on the code would silently lose
    /// the distinction; branching here does not.
    #[must_use]
    pub fn field_reason(&self) -> Option<&str> {
        let params = self.details.as_deref()?.get("params")?;
        // `params` carries BOTH shapes in this API: an array of per-field
        // entries (validation errors), and a plain object for endpoint-level
        // enrichment — e.g. the CAS conflict, whose `params` is
        // `{reason, current: {version_id, hash}}`. Reading only the array shape
        // silently missed every object-shaped reason.
        if let Some(entries) = params.as_array() {
            return entries
                .iter()
                .find_map(|p| p.get("reason").and_then(serde_json::Value::as_str));
        }
        params.get("reason").and_then(serde_json::Value::as_str)
    }

    /// Read a NUMERIC value out of `error.params` by key, tolerating every shape
    /// the platform actually emits.
    ///
    /// `error.params` is **polymorphic and undocumented**: the server hands the
    /// call site's own array straight to the JSON encoder, so an associative
    /// array becomes a JSON **object** and a sequential one becomes a JSON
    /// **array** — the wire type is decided per call site by how someone wrote a
    /// literal. Confirmed with the backend on 2026-08-23, after three
    /// independently-written clients each guarded with an array-only accessor
    /// and each silently dropped the object form.
    ///
    /// **Object shape ONLY, deliberately.** `params.{key}` is the shape the
    /// lockout emits (measured 2026-08-23:
    /// `{"attempts_remaining":4,"attempts_max":5}`). The ARRAY form is the
    /// per-field validation-diagnostics list (`{name, kind, message, code,
    /// expected_type?, received_alias?}`) and is a *different payload* that
    /// never carries lockout keys — the web app depends on exactly that
    /// distinction, using the shape itself to tell a lockout from a field
    /// rejection.
    ///
    /// The array is deliberately **not** searched for a `{name, value}` row.
    /// Such a lookup was once justified as forward-compatibility against a
    /// possible `list-always` normalisation; that normalisation was ruled out,
    /// so the justification expired, and the branch is a live hazard rather
    /// than dead code — a validation row that ever gained a `value` field could
    /// make a rejection of a field *named* `attempts_remaining` render as a
    /// sign-in countdown. Reading only the shape that actually carries these
    /// keys is fail-closed by construction.
    ///
    /// Numbers may arrive string-encoded (the framework does this for some
    /// codes — see `ApiClient::extract_error`), so a numeric string is accepted.
    #[must_use]
    fn param_u64(&self, key: &str) -> Option<u64> {
        match self.details.as_deref()?.get("params")? {
            serde_json::Value::Object(map) => map.get(key).and_then(json_u64),
            _ => None,
        }
    }

    /// Read a STRING value out of the OBJECT-shaped `error.params` by key.
    ///
    /// Object shape ONLY, exactly like [`Self::param_u64`] and for the same
    /// reason: the ARRAY form is the per-field validation-diagnostics list, a
    /// different payload that never carries endpoint-level enrichment such as
    /// `credential_type`. Reading only the shape that actually carries the key
    /// is fail-closed by construction — an array-shaped refusal still resolves
    /// its `reason`, but yields no credential type, so the caller falls back to
    /// the credential-agnostic wording rather than guessing.
    #[must_use]
    fn param_str(&self, key: &str) -> Option<&str> {
        match self.details.as_deref()?.get("params")? {
            serde_json::Value::Object(map) => map.get(key).and_then(serde_json::Value::as_str),
            _ => None,
        }
    }

    /// Whether this error says the account is CURRENTLY locked out.
    ///
    /// **One predicate, used by both [`Self::lockout_note`] and
    /// [`Self::suggestion`].** They previously disagreed: the note treated any
    /// parseable `retry_after_seconds` as proof of a lock, while the hint keyed
    /// only on `attempts_remaining == 0`. A `10008` carrying both a wait AND a
    /// non-zero remaining count therefore rendered "locked for about two
    /// minutes" immediately above "the email or password is incorrect … attempts
    /// remain".
    ///
    /// **Policy for contradictory fields: any lock signal wins.** The origin does
    /// not send that combination today (it emits only remaining+max), so this
    /// only governs a payload that is already
    /// self-inconsistent — and of the two possible readings, "you are locked" is
    /// the one that cannot waste a user's remaining attempts.
    #[must_use]
    fn is_locked_out(&self) -> bool {
        self.code == ERR_LOGIN_LOCKED
            || self.param_u64("retry_after_seconds").is_some()
            || self.param_u64("attempts_remaining") == Some(0)
    }

    /// A human sentence for the platform's per-account failed-login lockout,
    /// when the response carries the numbers to build one.
    ///
    /// The lockout (CASA / ASVS 2.2.1: 5 failures → 30-minute soft lockout,
    /// cleared by a successful sign-in) enriches its errors with
    /// `attempts_remaining` / `attempts_max` and, once locked,
    /// `retry_after_seconds`. Those values already reach the user as raw
    /// `param <key>: <value>` lines via [`render_details`] — but
    /// `param retry_after_seconds: 1800` is a diagnostic, not an answer to "when
    /// can I sign in again?". This turns them into the sentence.
    ///
    /// # Two semantics that are easy to get backwards
    ///
    /// Both established 2026-08-23, and both are easy to record backwards:
    ///
    /// 1. **`attempts_remaining: 0` means the account is ALREADY LOCKED** — not
    ///    "one more failure will lock you". The next request is refused before
    ///    the password is even evaluated.
    /// 2. **Absent is NOT zero.** Absent means the count could not be recorded,
    ///    and must never be rendered as a countdown. Hence `Option`, and hence
    ///    the deliberate `None` fall-through rather than a `0` default anywhere
    ///    in this function.
    ///
    #[must_use]
    pub fn lockout_note(&self) -> Option<String> {
        // ── Gate 1: only the two sign-in codes may produce a sign-in sentence.
        //
        // `Display` calls this for EVERY `ApiError`, so without this gate any
        // error whose `params` happened to carry `retry_after_seconds` would
        // announce an account lockout — an ordinary throttled download rendering
        // "Account temporarily locked."
        // Gate on the CODE (the two sign-in codes), not the HTTP status, since
        // 401/429 are shared by unrelated surfaces.
        if !matches!(self.code, ERR_CREDENTIALS_INVALID | ERR_LOGIN_LOCKED) {
            return None;
        }
        // ── Gate 2: `10760` MEANS LOCKED. It may never render a countdown.
        //
        // The bug this prevents: with code 10760 and an unparseable
        // `retry_after_seconds` (e.g. a value above `u64::MAX`), a fall-through
        // to the `attempts_remaining` arm would tell a LOCKED user "3 of 5
        // sign-in attempts remaining." An absent wait value does the same. The
        // lock is established by the code; the wait is
        // only ever a refinement of HOW LONG.
        //
        // The old tests could not catch this: the fixture used code 10760 for
        // every case INCLUDING the positive-countdown test, so it pinned the
        // defect as expected behaviour. Fixtures are now split per code.
        let locked_by_code = self.code == ERR_LOGIN_LOCKED;

        // Locked: a wait is the only fact that matters, so it outranks any
        // remaining-count that may also be present.
        if let Some(secs) = self.param_u64("retry_after_seconds") {
            // Round UP: "try again in 0 minutes" is never useful, and advising a
            // retry marginally too late is harmless where too early is not.
            let mins = secs.div_ceil(60);
            return Some(match mins {
                0 => "Account temporarily locked. Try again shortly.".to_owned(),
                1 => "Account temporarily locked. Try again in about 1 minute.".to_owned(),
                n => format!("Account temporarily locked. Try again in about {n} minutes."),
            });
        }
        // Locked by code, but no parseable wait. Still locked — say so, and do
        // NOT invent a duration or fall through to a countdown (Gate 2 above).
        if locked_by_code {
            return Some(
                "Account temporarily locked: too many failed sign-in attempts. \
                 Wait for the lockout to expire before trying again."
                    .to_owned(),
            );
        }
        // Not locked by the clock — report the countdown, if the server sent one.
        match self.param_u64("attempts_remaining") {
            // 0 = ALREADY locked (semantic 1 above). Never phrase this as a
            // remaining-attempt warning.
            Some(0) => Some(
                "Account temporarily locked: no sign-in attempts remain. \
                 A successful sign-in clears the lockout once it expires."
                    .to_owned(),
            ),
            Some(n) => {
                // Sanity: `remaining` above `max` is not a state the server can
                // be in, so the pair is untrustworthy — drop the max rather than
                // print "18446744073709551615 of 5". Both values parse, so
                // nothing else would catch it, and the result is nonsensical
                // rather than fail-closed.
                let max = self.param_u64("attempts_max").filter(|max| n <= *max);
                Some(match (n, max) {
                    (1, Some(max)) => {
                        format!("1 of {max} sign-in attempts remaining before a temporary lockout.")
                    }
                    (1, None) => {
                        "1 sign-in attempt remaining before a temporary lockout.".to_owned()
                    }
                    (n, Some(max)) => {
                        format!(
                            "{n} of {max} sign-in attempts remaining before a temporary lockout."
                        )
                    }
                    (n, None) => {
                        format!("{n} sign-in attempts remaining before a temporary lockout.")
                    }
                })
            }
            // Semantic 2: absent is not zero. No number, no countdown.
            None => None,
        }
    }

    /// Return a human-readable suggestion based on the HTTP status or error code.
    #[must_use]
    pub fn suggestion(&self) -> Option<&'static str> {
        // Structured `reason` first: it is the server's documented stable
        // discriminator, so it outranks both the numeric code and the status.
        //
        // Scope / access-mode refusals are keyed on the reason AND the
        // `credential_type` that produced it — the same refusal has a different
        // recovery per credential — so that two-key table resolves first.
        if let Some(hint) = self
            .field_reason()
            .and_then(|reason| scope_refusal_hint(reason, self.param_str("credential_type")))
        {
            return Some(hint);
        }
        if let Some(hint) = self.field_reason().and_then(reason_hint) {
            return Some(hint);
        }
        // Check specific error codes before falling back to HTTP status.
        if self.code == 10587 {
            return Some(
                "Account email not verified. Run `fastio auth verify --email <your-email>` to resend the verification email.",
            );
        }
        // The 422 reason-required error is keyed by its string `error_code`
        // (`ERR_REASON_REQUIRED`), not a numeric code, so match it here before
        // the numeric-code / HTTP-status fallbacks (a bare 422 yields no hint).
        if self.error_code.as_deref() == Some("ERR_REASON_REQUIRED") {
            return Some(HINT_REASON_REQUIRED);
        }
        // Billing / entitlement codes share centrally-defined hint strings so
        // billing, signing, and Ripley stay consistent. These are
        // checked before the HTTP-status fallback because several map onto
        // 402/403 where the generic status hint is less actionable.
        match self.code {
            // THE SIX ARMS BELOW MAY NEVER FIRE — DO NOT COUNT THEM AS COVERAGE.
            //
            // 1670/1680/1685/1688/1695/1696 (and 1605) are error CLASS
            // constants, i.e. the FIRST argument at the call site. The value
            // that reaches `error.code` is the SECOND — a unique per-site code
            // injected at build time. Established 2026-08-09; seven converging
            // lines, four behavioural:
            //
            //   • the server's validator falls back to 169650, NOT the class
            //     (test-pinned)
            //   • a "no top-level code at all" sentinel is redundant if the class
            //     were the natural default
            //   • authorization refusal → 10560, never 1680  [measured]
            //   • dangling manifest hash pair  → 196420, never 1605  [measured]
            //   • 2026-06-16, IN PRODUCTION: a hint keyed on 1695 never fired
            //     because the live code was 274701
            //   • 2026-06-23: documented 1605 arrived as 10571
            //
            // Kept rather than deleted because they cost nothing — correct if ever
            // reached, and the status/text fallback still prints and exits 1 if
            // not. **The hazard is believing these conditions are handled**: that
            // belief is exactly why 274701 went unmapped for two months. When a
            // plan/credit/access condition is OBSERVED, map the unique code it
            // actually carries and do not assume the arm below did the job.
            // CODE-KEYED MAPPINGS ARE FUSES. The platform's code generator
            // draws a RANDOM number in
            // 100000..=199999 and redraws only until it finds one not CURRENTLY
            // in use. There is no retirement list and no monotonic counter, so
            // the moment a call site is deleted its number is free and the next
            // substitution can draw it for something unrelated.
            //
            // ⇒ RULE: if you map a code to a meaning, DELETE THE MAPPING WHEN
            // THE MEANING DIES. The code is a pointer; deleting the target does
            // not null the pointer, and the inheritance is random rather than
            // adjacent — a stale arm prints billing advice for an upload error
            // and nothing signals the mismatch.
            //
            // Exposure here (audited 2026-08-25, none currently stale):
            //   IN the redraw pool → 107184, 115069, 162417, 164797, 166910
            //   OUTSIDE it (stable APP_* constants, 4-5 digit) → all the rest
            // Removing a feature? Check the first list before you finish.
            // Precedent: the four room arms were removed with Coordination
            // Rooms; three of those numbers were free within the hour.
            1688 => return Some(HINT_SUBSCRIPTION_REQUIRED),
            1695 => return Some(HINT_UPGRADE_REQUIRED),
            1696 => return Some(HINT_CREDIT_LIMIT),
            1670 => return Some(HINT_RESTRICTED),
            1685 => return Some(HINT_FEATURE_LIMIT),
            // Generic "Invalid Input" (HTTP 400). Covers storage rename/transfer
            // name-conflicts (corrected from HTTP 500 on 2026-06-14) and many
            // other invalid-value cases. The bare-400 fallback below yields no
            // hint, so map the code to actionable (resource-agnostic) guidance.
            1605 => return Some(HINT_INVALID_INPUT),
            10566 => return Some(HINT_INVALID_NODE_FIELD),
            107_184 => return Some(HINT_AGENT_NAME_INVALID),
            9992 => return Some(HINT_UNKNOWN_ROUTE),
            // Length-limit codes. Undocumented in the published code lists, and
            // every one of them reports a BYTE bound in a message that says
            // "characters" (except the room display cap, which really is
            // characters) — so the generic 1605-ish wording would leave the
            // caller shortening the wrong thing, or shortening at all when the
            // real problem is that their language costs 3 bytes per character.
            166_910 => return Some(HINT_COMMENT_DISPLAY_LIMIT),
            162_417 | 164_797 => return Some(HINT_COMMENT_BODY_LIMIT),
            // Access-denied codes that surface as HTTP 401 but are NOT a missing
            // login (the caller IS authenticated). Without these arms the bare
            // 401 fallback below would emit the misleading "run `fastio auth
            // login`" hint.
            // THE ONE PLACE THIS FILE KEYS ON STATUS ON PURPOSE — and it is
            // the deliberate inverse of the `10175` rule two arms below.
            //
            // Normally the CODE is the precise discriminator and the status is
            // the coarse one, so keying on the code survives a re-status. `10545`
            // is the exception: the platform emits it from four sites across TWO
            // AXES — "you are not a member" and "this token is not scoped for
            // it" — and after the 2026-08-23 flip the membership sites keep 401
            // while the scope site answers 403. So here the code is the AMBIGUOUS
            // signal and the status is the disambiguator.
            //
            // It matters because the membership advice is actively wrong for the
            // scope case: "ask an admin to add you" cannot help when the problem
            // is the credential's scope, and no admin action widens a token.
            //
            // The backend kept `10545` on 401 for the membership sites
            // deliberately ("a different axis"), which is what creates the split.
            10545 => {
                return Some(if self.http_status == 403 {
                    HINT_ENTITY_SCOPE
                } else {
                    HINT_ENTITY_MEMBERSHIP
                });
            }
            10560 => return Some(HINT_TOKEN_SCOPE),
            // Code-keyed FALLBACK for the scope/access-mode refusals, reached
            // only when the refusal arrives without the `params` object the
            // reason-keyed table above reads. Without these arms such a
            // response lands on the generic 403 line, whose "your account lacks
            // the required role" wording is the exact advice this whole family
            // exists to displace. The credential type is unknowable here, so
            // each code maps to the GENERIC member of its family — the only
            // member that is correct without one.
            10767 => return Some(HINT_SCOPE_ADMIN_REQUIRED_GENERIC),
            10768 => return Some(HINT_SCOPE_EXCEEDS_ISSUER_GENERIC),
            10769 => return Some(HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC),
            10770 => return Some(HINT_SCOPE_WRITE_REQUIRED_GENERIC),
            // `10175` rides HTTP 401 today and HTTP 403 after the platform's
            // RFC-6750 correction. Keyed here on the CODE so it outranks BOTH
            // status fallbacks — see [`HINT_SCOPE_INCORRECT`] for why each of
            // them is wrong, and why the 401 one is actively harmful.
            10175 => return Some(HINT_SCOPE_INCORRECT),
            // `10008` is a WRONG-CREDENTIALS answer, not a missing-login state.
            // The generic 401 fallback points the user back at `auth login`,
            // which is what they just ran and which spends another counted
            // attempt — measured live, see [`HINT_CREDENTIALS_INVALID`].
            // `10008` is a WRONG-CREDENTIALS answer — EXCEPT on the response
            // that establishes the lock, which still carries 10008 alongside
            // `attempts_remaining: 0`. There, "check the password" is the wrong
            // advice: the next request is refused before any password is
            // evaluated. Observed live — the fifth failure rendered
            // "Account temporarily locked" and "The email or password is
            // incorrect" together, a dual message review of this path predicted.
            ERR_CREDENTIALS_INVALID => {
                // Shares [`Self::is_locked_out`] with `lockout_note()` so the
                // rendered note and the hint can never contradict each other.
                return Some(if self.is_locked_out() {
                    HINT_LOGIN_LOCKED
                } else {
                    HINT_CREDENTIALS_INVALID
                });
            }
            // The lockout itself. Without an arm here a 429 lockout falls to the
            // generic "Rate limited. Wait a moment and try again." — "a moment"
            // being up to 30 minutes. The exact wait, when the server supplies
            // it, is rendered separately by [`Self::lockout_note`].
            ERR_LOGIN_LOCKED => return Some(HINT_LOGIN_LOCKED),
            115_069 => return Some(HINT_RESOURCE_ACCESS),
            1680 => return Some(HINT_ACCESS_DENIED),
            // Cloud-import drive/destination selection are NOT mapped here:
            // they carry a stable `reason` and are resolved by `reason_hint`
            // above. Numeric codes are per-call-site and get consolidated (the
            // imports layer merged four call sites onto one code to remove an
            // existence oracle), so keying on them would silently conflate
            // distinct conditions.
            _ => {}
        }
        // HONOUR [`ERR_BODY_UNAVAILABLE`] BEFORE ANY STATUS FALLBACK.
        //
        // That constant's contract says any classifier keying on HTTP status
        // must treat the marker as UNKNOWN rather than as a normal instance of
        // that status — and this function is the central status classifier. It
        // ignoring the marker here would violate a rule written three hundred
        // lines up in the same file.
        //
        // The status is real; the REASON is not, because the body carrying it
        // could not be read. Guessing from the status alone reproduces exactly
        // what this change exists to stop — an unreadable 401 rendered
        // "Run `fastio auth login` to sign in", the harmful fallback killed for
        // `10175`/`10545`/`10008` everywhere else in this file. `Display`
        // already states the body was unreadable, so silence here is honest;
        // a hint would be invention.
        //
        // Code-keyed arms above are unreachable for a marker error anyway (its
        // `code` is 0), so this only gates the status fallbacks.
        if self.error_code.as_deref() == Some(ERR_BODY_UNAVAILABLE) {
            return None;
        }
        match self.http_status {
            // 401 IS THE MOST OVERLOADED STATUS ON THIS API. Four per-code
            // overrides exist and THREE of them exist precisely because this
            // generic line was wrong — `HINT_TOKEN_SCOPE`,
            // `HINT_WORKSPACE_MEMBERSHIP` and `HINT_RESOURCE_ACCESS` each say so
            // at their own definition. The old wording asserted a cause
            // ("Authentication failed") and prescribed ONE action ("run `fastio
            // auth login`") that provably CANNOT help when the caller is
            // authenticated but unscoped, is not a workspace member, or is
            // looking at something not shared with them: re-running login mints
            // the same restricted credential (measured live 2026-08-08, code
            // `10560`).
            //
            // Action-only and cause-free, so it cannot contradict whichever of
            // the four this actually is. Codes with a SPECIFIC recovery keep
            // their own arm above and outrank this.
            401 => Some(
                "Check the credential and your access to this resource: `fastio auth scopes` shows \
                 what a restricted key reaches, and `fastio auth login` replaces an expired or \
                 missing one. Neither helps if the resource has simply not been shared with your \
                 account.",
            ),
            // 402 with no recognized billing code still steers to the billing
            // surface — the shared subscription-required hint is the right
            // default recovery path.
            402 => Some(HINT_SUBSCRIPTION_REQUIRED),
            // 403 CARRIES TWO AXES SINCE 2026-08-23. It used to mean only
            // "your account lacks the permission"; the platform's scope flip
            // moved an entire family of CREDENTIAL-scope refusals onto it
            // (`10175`, plus the entity-scope codes `10560`/`10574`/`10753`/
            // `10754`/`10757`). The old wording named only the first axis, so a
            // scope refusal read as a role problem and sent the user to an admin
            // who cannot help.
            //
            // Deliberately covers both axes here rather than mapping each new
            // code: those codes are per-call-site and their fate is still being
            // decided upstream, so a code-keyed arm risks becoming a
            // may-never-fire hint (see the warning above the 1670/1688/1695
            // block). A status-keyed fallback that names both possibilities
            // cannot go stale that way. Codes with a SPECIFIC recovery (`10175`
            // → finish 2FA) still get their own arm above and outrank this.
            // The recovery is CONDITIONAL on purpose. An earlier version named
            // both axes and then gave one imperative — "Check `fastio auth
            // scopes`" — which sends a user who genuinely lacks a ROLE to
            // inspect token scopes, and lets "no admin action widens a token's
            // scope" read as a fact about THIS 403 rather than about the scope
            // axis. `APP_FORBIDDEN` is not scope-only: it also covers
            // publish-disabled, signer-token gates, 2FA channel refusals,
            // File Share forbidden and deprecation — so an unconditional
            // scope-only imperative is a wording bias, not a logic error.
            403 => Some(
                "Permission denied. Either your account lacks the required role for this resource, \
                 or this credential is not scoped for it — a restricted API key reaches only what \
                 it was issued for, and being granted membership does not add scope. If you are \
                 using a restricted credential, run `fastio auth scopes` to see what it covers.",
            ),
            // 404 is overloaded (two per-code overrides: `9992
            // UNKNOWN_ROUTE`, and `HINT_ARTIFACT_NOT_READY` whose own docs say
            // it is "NOT a genuine not-found — the ids are correct"). The old
            // wording asserted the cause ("Resource not found") and told the
            // user to re-check an ID that may be perfectly correct.
            //
            // Action-only, so it cannot contradict a tombstone, a not-yet-ready
            // artifact, or a route that never existed — while keeping the half
            // that plain deletion would have discarded, the discovery path.
            404 => Some("Use a `list` command to discover valid IDs."),
            // 5xx DELIBERATELY UNCHANGED. It names a cause and offers no action,
            // which is the same shape as the two arms above — but the obvious
            // repair ("wait and retry") is one this client must not ship.
            //
            // MEASURED 2026-08-26 against the backend, and it is the bad
            // branch: a 502-504 does NOT mean the request never arrived, and a
            // 504 is POSITIVE EVIDENCE that it did — the gateway's read timeout
            // fires only after the whole request was delivered, so the upstream
            // keeps running and the mutation likely COMPLETES afterwards,
            // invisibly. The gateway also retries idempotent-BY-METHOD on its
            // own, so on PUT/DELETE it and this client both retry and the
            // attempts MULTIPLY. Telling a user to retry a failed mutation is
            // therefore advice to duplicate it.
            //
            // An earlier draft of this comment said "the gateway refuses to
            // replay mutations". That was WRONG — it refuses only
            // non-idempotent methods, and method-level idempotency is an
            // assumption about the application, not a guarantee from the edge.
            // The conclusion survives the correction and is strengthened by it;
            // the premise did not, which is why it is written down.
            //
            // The rule is that the GAPS are safe and the ENTRIES are the
            // hazard; adding a better-sounding 5xx action would be a
            // regression, not a fix.
            // Deliberately does NOT say "wait and retry". A 409 on this API is
            // EITHER lock contention (waiting helps) OR a compare-and-swap
            // rejection (waiting NEVER helps — the version has moved and will
            // stay moved, so retrying unchanged loops forever). The marker that
            // separates them (`params.reason`) is not emitted yet, so this hint
            // must be correct for BOTH: re-read first is right either way.
            // Measured: a note CAS conflict returns 409 `113958` with no
            // `reason`, so it lands here — and the old wording counselled
            // exactly the livelock this program exists to stop.
            409 => Some(
                "The resource is locked by another request, or has changed since the state you \
                 supplied. Re-read its current state before retrying — repeating the same request \
                 unchanged may never succeed.",
            ),
            429 => Some("Rate limited. Wait a moment and try again."),
            500..=599 => Some("Server error. The Fast.io API may be experiencing issues."),
            _ => None,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[HTTP {}] {}", self.http_status, self.message)?;
        if self.code > 0 {
            write!(f, " (code {})", self.code)?;
        }
        if let Some(ref ec) = self.error_code {
            write!(f, " [{ec}]")?;
        }
        if let Some(ref details) = self.details {
            render_details(f, details)?;
        }
        // The failed-login lockout numbers already render above as raw
        // `param <key>: <value>` diagnostics. Append the interpreted sentence so
        // the user gets an answer ("try again in about 30 minutes") rather than
        // only a datum ("param retry_after_seconds: 1800"). The raw lines stay
        // for support correlation. Emits nothing when the numbers are absent —
        // see [`ApiError::lockout_note`], where absent is deliberately not zero.
        if let Some(note) = self.lockout_note() {
            write!(f, "\n  {note}")?;
        }
        Ok(())
    }
}

/// Render the structured server `details` onto an [`ApiError`]'s `Display`.
///
/// Without this, the structured enrichment (`reason`, `validation_report`,
/// `params[]`, `documentation_url`, `resource`) harvested into
/// [`ApiError::details`] is invisible to both the CLI (anyhow → `Display` →
/// stderr) and MCP (`cli_err_to_result` → `to_string`). The rendering is a
/// compact, multi-line digest appended after the headline so a 422 template
/// `validation_report`, a trigger-fire 409 `reason`, and 400 `params[]`
/// surface through one shared path.
///
/// The contained JSON is server diagnostics (never a credential), but it is
/// still untrusted text; long values are truncated so the message stays
/// readable on a terminal.
fn render_details(f: &mut fmt::Formatter<'_>, details: &serde_json::Value) -> fmt::Result {
    use serde_json::Value;

    // `reason` (409 fire/conflict): a string or a structured object.
    if let Some(reason) = details.get("reason").filter(|v| !v.is_null()) {
        match reason {
            Value::String(s) => write!(f, "\n  reason: {}", truncate_detail(s))?,
            other => write!(
                f,
                "\n  reason: {}",
                truncate_detail(&compact_json_bounded(other))
            )?,
        }
    }

    // `params` (400 / 409): two shapes.
    //   - An ARRAY of per-field validation failures (the modern replacement for
    //     the retired per-field integer codes; `name`/`kind`/`code`/`message`).
    //   - An OBJECT of structured diagnostics for a single conflict — e.g. a
    //     decision CAS 409 carries `{code, reason, current_round_id}`.
    //     Without the object arm, that payload reaches `ApiError::details` but is
    //     silently dropped from `Display`, so the user never sees WHY the write
    //     was rejected or which round is current.
    // Cap the number of rendered entries either way so a pathological response
    // can't flood stderr / MCP.
    match details.get("params") {
        Some(Value::Array(params)) => {
            for p in params.iter().take(MAX_RENDERED_PARAMS) {
                let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
                let msg = p
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| p.get("kind").and_then(Value::as_str))
                    .unwrap_or("invalid");
                // Both the field name and the message are untrusted server text;
                // bound each so a pathological name can't blow up the render either.
                write!(
                    f,
                    "\n  param {}: {}",
                    truncate_detail(name),
                    truncate_detail(msg)
                )?;
            }
            if params.len() > MAX_RENDERED_PARAMS {
                write!(f, "\n  … ({} more)", params.len() - MAX_RENDERED_PARAMS)?;
            }
        }
        Some(Value::Object(fields)) => {
            for (key, val) in fields.iter().take(MAX_RENDERED_PARAMS) {
                // Render scalars as themselves; nest objects/arrays compactly so
                // no field is dropped. Both key and value are untrusted server
                // text, so bound each.
                let rendered = match val {
                    Value::String(s) => truncate_detail(s).into_owned(),
                    Value::Null => "null".to_owned(),
                    // Serialize nested object/array values with a byte cap so a
                    // large/hostile diagnostic can't drive an unbounded
                    // allocation here before truncation.
                    other => truncate_detail(&compact_json_bounded(other)).into_owned(),
                };
                write!(f, "\n  param {}: {}", truncate_detail(key), rendered)?;
            }
            if fields.len() > MAX_RENDERED_PARAMS {
                write!(f, "\n  … ({} more)", fields.len() - MAX_RENDERED_PARAMS)?;
            }
        }
        _ => {}
    }

    // `validation_report` (422): structured template/schema report. Serialized
    // with the same byte cap as the object-params arm so a large report can't
    // drive an unbounded allocation before truncation.
    if let Some(report) = details.get("validation_report").filter(|v| !v.is_null()) {
        write!(
            f,
            "\n  validation_report: {}",
            truncate_detail(&compact_json_bounded(report))
        )?;
    }

    // Doc + resource links, when present. Bounded like every other detail
    // value so an oversized server-supplied link can't blow up stderr / MCP.
    if let Some(url) = details.get("documentation_url").and_then(Value::as_str) {
        write!(f, "\n  see: {}", truncate_detail(url))?;
    }
    if let Some(res) = details.get("resource").and_then(Value::as_str) {
        write!(f, "\n  resource: {}", truncate_detail(res))?;
    }
    Ok(())
}

/// Maximum number of `params[]` entries rendered onto an [`ApiError`]'s
/// `Display`; further entries are summarized as `… (N more)` so a pathological
/// validation response cannot flood stderr or an MCP error payload.
const MAX_RENDERED_PARAMS: usize = 10;

/// Maximum rendered length of a single detail value, in **BYTES** (keeps stderr
/// readable). Bytes, not characters, because this bounds allocation and output
/// size rather than mirroring any server contract — [`truncate_detail`] walks
/// back to a char boundary before slicing, so a multi-byte char is never split.
const DETAIL_MAX_LEN: usize = 400;

/// Byte cap applied while serializing a nested object/array detail value. Sized
/// a little above [`DETAIL_MAX_LEN`] so the captured prefix still spans the
/// truncation point that [`truncate_detail`] clips, while bounding the
/// allocation (and serialization work) a large/hostile nested value can drive.
const COMPACT_JSON_CAP: usize = DETAIL_MAX_LEN + 64;

/// A `std::io::Write` sink that captures at most `cap` bytes and then refuses
/// further writes, so [`serde_json::to_writer`] aborts early instead of
/// materializing a full string for a large/hostile nested value.
struct CappedWriter {
    buf: Vec<u8>,
    cap: usize,
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let remaining = self.cap.saturating_sub(self.buf.len());
        if remaining == 0 {
            // At capacity — signal "full" so the serializer stops here.
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "detail value capped",
            ));
        }
        let take = data.len().min(remaining);
        self.buf.extend_from_slice(&data[..take]);
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serialize a JSON value compactly but bounded to approximately
/// [`COMPACT_JSON_CAP`] bytes, so rendering a structured `error.params` /
/// `validation_report` value during `Display` cannot drive an unbounded
/// allocation. The byte buffer is hard-capped at [`COMPACT_JSON_CAP`]; the
/// returned `String` can exceed that by at most +2 bytes, because the captured
/// prefix is lossy-decoded ([`String::from_utf8_lossy`]) and a multi-byte char
/// split at the cap boundary is replaced by a 3-byte U+FFFD (so a 1-byte
/// fragment grows by +2). Lossy decoding is also why a char split at the cap
/// boundary can never panic. [`truncate_detail`] then clips the result to
/// [`DETAIL_MAX_LEN`].
fn compact_json_bounded(value: &serde_json::Value) -> String {
    let mut writer = CappedWriter {
        buf: Vec::new(),
        cap: COMPACT_JSON_CAP,
    };
    // The only error is our own "capped" signal (or, in principle, a serializer
    // failure that cannot occur for an in-memory `Value`); either way the
    // captured prefix is what we render.
    let _ = serde_json::to_writer(&mut writer, value);
    String::from_utf8_lossy(&writer.buf).into_owned()
}

/// Truncate a detail string on a char boundary, appending an ellipsis marker
/// so the reader knows it was clipped.
fn truncate_detail(s: &str) -> std::borrow::Cow<'_, str> {
    if s.len() <= DETAIL_MAX_LEN {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut end = DETAIL_MAX_LEN;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!("{}… (truncated)", &s[..end]))
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn api_err(code: u32, http_status: u16) -> ApiError {
        ApiError::new(code, None, "boom".to_owned(), http_status)
    }

    #[test]
    fn suggestion_billing_codes_use_shared_hints() {
        assert_eq!(
            api_err(1688, 402).suggestion(),
            Some(HINT_SUBSCRIPTION_REQUIRED)
        );
        assert_eq!(api_err(1695, 402).suggestion(), Some(HINT_UPGRADE_REQUIRED));
        assert_eq!(api_err(1696, 402).suggestion(), Some(HINT_CREDIT_LIMIT));
    }

    #[test]
    fn suggestion_restricted_and_feature_limit_codes_use_generic_hints() {
        // Codes 1670/1685 are general-purpose in this API, so their hints must
        // be resource-agnostic and must NOT mention signing.
        assert_eq!(api_err(1670, 403).suggestion(), Some(HINT_RESTRICTED));
        assert_eq!(api_err(1685, 402).suggestion(), Some(HINT_FEATURE_LIMIT));
        assert!(!HINT_RESTRICTED.to_lowercase().contains("sign"));
        assert!(!HINT_FEATURE_LIMIT.to_lowercase().contains("sign"));
    }

    #[test]
    fn truncate_detail_never_splits_a_multibyte_char_at_the_cut() {
        use super::{DETAIL_MAX_LEN, truncate_detail};
        // A "clean" boundary check often just means the fixture never crossed
        // a boundary — in a UTF-16 client an even-length prefix hides a
        // surrogate split entirely. The Rust equivalent: slicing off a char
        // boundary PANICS, so the only way to prove the walk-back works is to
        // place a multi-byte character so it STRADDLES the cut. Sweep every
        // offset at which that can happen, for both 3- and 4-byte characters,
        // rather than picking one padding length and calling it verified.
        for (ch, width) in [('猫', 3usize), ('😀', 4usize)] {
            for straddle in 1..width {
                // Pad so exactly `width - straddle` bytes of `ch` sit before
                // DETAIL_MAX_LEN and the rest after — i.e. the cut lands INSIDE
                // the character.
                let pad = DETAIL_MAX_LEN - (width - straddle);
                let mut s = "a".repeat(pad);
                for _ in 0..64 {
                    s.push(ch);
                }
                assert!(s.len() > DETAIL_MAX_LEN, "fixture must trigger truncation");
                assert!(
                    !s.is_char_boundary(DETAIL_MAX_LEN),
                    "fixture must actually straddle: {ch:?} at offset {DETAIL_MAX_LEN}"
                );
                // Panics here if the walk-back is wrong — that is the assertion.
                let out = truncate_detail(&s);
                assert!(out.ends_with("… (truncated)"));
                // And it must not have emitted a partial character.
                assert!(std::str::from_utf8(out.as_bytes()).is_ok());
            }
        }
    }

    /// The version-conflict hint must tell the caller to CHECK before
    /// re-applying, not just to re-apply.
    ///
    /// A retried write-back session can produce a conflict naming the caller's
    /// OWN committed write, so blind re-apply stacks duplicate bytes. Retry
    /// advice must survive (stripping it would leave a dead end) — what must
    /// also be present is the verification step.
    #[test]
    fn version_conflict_hint_says_verify_before_reapplying() {
        let hint = super::HINT_VERSION_CONFLICT.to_lowercase();
        assert!(
            hint.contains("already present"),
            "must tell the caller to check whether their change already landed: {hint}"
        );
        assert!(
            hint.contains("retry"),
            "retry advice must survive — removing it replaces a duplicate with a dead end: {hint}"
        );
    }

    /// A bare 409 must never counsel WAITING.
    ///
    /// Measured: the note CAS conflict is `113958`/409 with no
    /// `params.reason`, so it falls through to the generic 409 hint. Waiting
    /// cannot resolve a compare-and-swap rejection — the version has moved and
    /// stays moved — so "wait and retry" instructs an agent to loop forever.
    /// Sibling of the `120719` carve-out below, and the same reason.
    #[test]
    fn generic_409_hint_does_not_counsel_waiting() {
        let hint = api_err(113_958, 409)
            .suggestion()
            .expect("a 409 carries a hint");
        for banned in ["Wait a moment", "wait a moment", "Wait and retry"] {
            assert!(
                !hint.contains(banned),
                "a 409 may be a CAS rejection, where waiting never helps: {hint}"
            );
        }
        assert!(
            hint.to_lowercase().contains("re-read"),
            "the 409 hint must tell the caller to re-read current state: {hint}"
        );
    }

    #[test]
    fn suggestion_length_limit_codes_name_the_right_unit() {
        use super::{HINT_COMMENT_BODY_LIMIT, HINT_COMMENT_DISPLAY_LIMIT};
        assert_eq!(
            api_err(166_910, 406).suggestion(),
            Some(HINT_COMMENT_DISPLAY_LIMIT)
        );
        assert_eq!(
            api_err(162_417, 406).suggestion(),
            Some(HINT_COMMENT_BODY_LIMIT)
        );
        assert_eq!(
            api_err(164_797, 406).suggestion(),
            Some(HINT_COMMENT_BODY_LIMIT)
        );

        // The comment bounds are CHARACTERS since the platform's unit fix, and
        // as of 2026-08-07 that is what the servers enforce — re-verified at
        // the exact boundary: 500 CJK characters (1 500 bytes) accepted, 501
        // rejected with `166910` reporting "has 501 characters". These hints
        // carried a byte-window caveat while the servers still counted bytes
        // (verified 2026-08-06); it is removed because it is now WRONG where the
        // fix has landed, and re-adding it would tell users the accurate count
        // in the server's message is a byte count.
        // NOTE the negative assertions below are CASE-INSENSITIVE by design. A
        // guard spelled `!h.contains("BYTES")` passes the moment someone writes
        // "bytes" — it would report green while the caveat is back. That is the
        // vacuous-guard shape: a negative needle pinned to an incidental detail
        // of the text it is meant to forbid.
        for h in [HINT_COMMENT_DISPLAY_LIMIT, HINT_COMMENT_BODY_LIMIT] {
            assert!(h.contains("character"), "must state the character contract");
            assert!(
                !h.to_lowercase().contains("byte"),
                "must NOT re-introduce the byte-window caveat: the fix is deployed \
                 and the server's own count is characters"
            );
        }
        // Rooms counts CHARACTERS and says so (`121897`).
        //
    }

    /// The comment display-limit hint must state that the mention discount is
    /// CONDITIONAL on the markup being unfenced.
    ///
    /// It asserted the discount flatly until 2026-08-10, when a discriminating
    /// pair — identical but for a code fence — showed 460 visible
    /// characters plus 76 characters of UNFENCED `@[file:…]` markup ACCEPTED and
    /// the same input FENCED rejected with `166910`. An author who fences markup
    /// hits the cap sooner than the flat claim predicts, and the hint is where
    /// they look.
    ///
    /// Asserted positively — dropping the word "discounted" would satisfy a
    /// negative-only guard while still leaving a reader with no idea when the
    /// discount applies.
    #[test]
    fn comment_display_hint_states_the_discount_is_fence_conditional() {
        use super::HINT_COMMENT_DISPLAY_LIMIT as HINT;

        assert!(
            HINT.contains("discounted only while UNFENCED"),
            "the hint must QUALIFY the discount, not assert it flatly: {HINT}"
        );
        assert!(
            HINT.contains("code fence") && HINT.contains("counts in full"),
            "the hint must say what happens to FENCED markup — the half a user \
             gets wrong: {HINT}"
        );
        // The rest of the contract this hint carries must survive the qualifier.
        assert!(
            HINT.contains("500-character") && HINT.contains("character count"),
            "the hint must still name the bound and that the reported count is \
             in characters: {HINT}"
        );
    }

    #[test]
    fn suggestion_unknown_route_9992_is_generic_non_signing() {
        // `9992` is ONE call site: the API director's path walk ending with no
        // handler matched. The copy must name that
        // mechanism, must NOT diagnose which side caused it, and must stay
        // resource-agnostic (no "sign" wording; that lives in
        // map_signing_error / the MCP sign_err_to_result, never here).
        assert_eq!(api_err(9992, 404).suggestion(), Some(HINT_UNKNOWN_ROUTE));
        let lower = HINT_UNKNOWN_ROUTE.to_lowercase();
        assert!(!lower.contains("sign"));

        // Must not send the caller to "fix" arguments that may be correct.
        for domain_cause in [
            "does not recognize this api path",
            "route may have been removed",
            "check the id",
            "id does not exist",
            "cli update",
        ] {
            assert!(
                !lower.contains(domain_cause),
                "9992 copy must name no domain cause, found {domain_cause:?}: {HINT_UNKNOWN_ROUTE}"
            );
        }

        // Must not resurrect the retired "999x family = edge rejected the
        // RESPONSE" model. That is `90211`/`errNoVeHeader`, a different
        // condition; conflating them by shared leading digit rewrote this
        // shipped string wrongly three times in one afternoon.
        for retired in ["edge layer", "worker", "rejected this response"] {
            assert!(
                !lower.contains(retired),
                "9992 copy must not assert the retired edge-rejection model, \
                 found {retired:?}: {HINT_UNKNOWN_ROUTE}"
            );
        }

        // Must name the mechanism, and must refuse to attribute a side: a
        // client calling an unexposed path and a deployment missing an
        // endpoint are indistinguishable from the response alone.
        assert!(
            lower.contains("director"),
            "9992 copy must name the call site: {HINT_UNKNOWN_ROUTE}"
        );
        assert!(
            lower.contains("cannot say which side") || lower.contains("look identical"),
            "9992 copy must refuse to attribute a side: {HINT_UNKNOWN_ROUTE}"
        );
    }

    #[test]
    fn suggestion_access_codes_override_generic_401_hint() {
        // 10545 (workspace membership) and 115069 (resource access) surface as
        // HTTP 401 but are NOT a missing login — the caller IS authenticated. The
        // rendered hint must steer to the access problem, never to `auth login`.
        // Both hints must also stay resource-agnostic (no "sign" wording — that
        // lives in map_signing_error / the MCP sign_err_to_result, never here).
        let m = api_err(10545, 401).suggestion().unwrap_or_default();
        assert_eq!(
            api_err(10545, 401).suggestion(),
            Some(HINT_ENTITY_MEMBERSHIP)
        );
        assert!(
            !m.to_lowercase().contains("auth login"),
            "10545 hint must not steer to auth login: {m}"
        );
        // Entity-agnostic: `10545` is ERROR_ORG_NOT_AUTHORIZED and was measured
        // live on /org/{id}/ paths, so the hint must not name only "workspace".
        assert!(m.to_lowercase().contains("org"));

        let r = api_err(115_069, 401).suggestion().unwrap_or_default();
        assert_eq!(
            api_err(115_069, 401).suggestion(),
            Some(HINT_RESOURCE_ACCESS)
        );
        assert!(
            !r.to_lowercase().contains("auth login"),
            "115069 hint must not steer to auth login: {r}"
        );
        assert!(r.to_lowercase().contains("access"));

        assert!(!HINT_ENTITY_MEMBERSHIP.to_lowercase().contains("sign"));
        assert!(!HINT_RESOURCE_ACCESS.to_lowercase().contains("sign"));
    }

    #[test]
    fn billing_hints_reference_only_existing_commands() {
        // The hints must not reference commands that don't exist in the current
        // CLI surface. `billing usage` and `billing subscribe` BOTH exist, so
        // they are valid targets; the deprecated `--plan-id` flag (the
        // canonical flag is `--plan`) must NOT appear.
        for hint in [
            HINT_SUBSCRIPTION_REQUIRED,
            HINT_UPGRADE_REQUIRED,
            HINT_CREDIT_LIMIT,
        ] {
            assert!(
                !hint.contains("--plan-id"),
                "hint references the removed `--plan-id` flag (use `--plan`): {hint}"
            );
        }
    }

    #[test]
    fn invalid_header_value_display_and_hint_are_resource_agnostic() {
        // The variant names ONLY the header (its whole purpose) and NEVER the
        // offending value (it is a secret). The Display deliberately carries the
        // header name `x-ve-password`, but neither the Display nor the hint may
        // carry FEATURE wording (fileshare / file share / sign) — so the variant
        // stays reusable for any header-bound secret. The hint additionally must
        // never name a concrete header or secret kind.
        let err = CliError::InvalidHeaderValue {
            header: "x-ve-password",
        };
        let rendered = err.to_string();
        assert_eq!(rendered, "invalid value for x-ve-password header");
        let hint = err.suggestion().unwrap_or_default();
        assert_eq!(hint, HINT_INVALID_HEADER_VALUE);
        // No FEATURE wording in either the Display or the hint.
        for needle in ["fileshare", "file share", "sign", "envelope"] {
            assert!(
                !rendered.to_lowercase().contains(needle),
                "InvalidHeaderValue Display must not carry resource wording ({needle}): {rendered}"
            );
            assert!(
                !hint.to_lowercase().contains(needle),
                "InvalidHeaderValue hint must not carry resource wording ({needle}): {hint}"
            );
        }
        // The hint stays generic: it names no specific header or secret kind.
        assert!(!hint.to_lowercase().contains("x-ve-password"));
        assert!(!hint.to_lowercase().contains("password"));
    }

    #[test]
    fn mapped_api_delegates_display_and_owns_hint() {
        // Display delegates to the inner ApiError verbatim (preserving the server
        // headline + code), so the exit-code-bearing message survives.
        let api = ApiError::new(1650, None, "boom".to_owned(), 401);
        let want_display = api.to_string();
        let mapped = CliError::MappedApi {
            api,
            hint: Some("use --password"),
        };
        assert_eq!(
            mapped.to_string(),
            want_display,
            "MappedApi Display must delegate to the inner ApiError"
        );
        // The hint is whatever the mapping layer supplied — NOT the inner
        // ApiError's generic 401 "auth login" default.
        assert_eq!(mapped.suggestion(), Some("use --password"));

        // `hint: None` SUPPRESSES the hint entirely (no generic fallback).
        let suppressed = CliError::MappedApi {
            api: ApiError::new(0, None, "boom".to_owned(), 404),
            hint: None,
        };
        assert_eq!(
            suppressed.suggestion(),
            None,
            "MappedApi with hint None must print no hint (no generic-404 fallback)"
        );
    }

    #[test]
    fn mapped_api_variant_carries_no_resource_wording() {
        // The variant itself must stay RESOURCE-AGNOSTIC: its Display delegates to
        // the inner (server-supplied) ApiError and it relays only the caller's
        // `hint`. With a plain server message and no hint, nothing fileshare /
        // share / sign specific may appear from the variant's own structure.
        let err = CliError::MappedApi {
            api: ApiError::new(1609, None, "not available".to_owned(), 404),
            hint: None,
        };
        let rendered = err.to_string();
        for needle in ["fileshare", "file share", "sign", "envelope"] {
            assert!(
                !rendered.to_lowercase().contains(needle),
                "MappedApi Display must not carry resource wording ({needle}): {rendered}"
            );
        }
    }

    #[test]
    fn version_conflict_display_and_hint_are_resource_agnostic() {
        // The conflict wording must be resource-AGNOSTIC ("the target file
        // changed") — never naming a feature — so the variant stays reusable for
        // any compare-and-swap write; the current version id is carried for the
        // rebase, but no fileshare/share/sign wording may appear.
        let err = CliError::VersionConflict {
            current_version: "v9xQ2-abc12".to_owned(),
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("v9xQ2-abc12"),
            "current version id must surface for the rebase: {rendered}"
        );
        let hint = err.suggestion().unwrap_or_default();
        assert_eq!(hint, HINT_VERSION_CONFLICT);
        for needle in ["fileshare", "file share", "share", "sign", "envelope"] {
            assert!(
                !rendered.to_lowercase().contains(needle),
                "VersionConflict Display must not carry resource wording ({needle}): {rendered}"
            );
            assert!(
                !hint.to_lowercase().contains(needle),
                "VersionConflict hint must not carry resource wording ({needle}): {hint}"
            );
        }
        // The CAS conflict hint must NOT collide with the Config hint (which
        // wrongly says "run fastio configure init" for this case).
        assert!(!hint.to_lowercase().contains("configure init"));
    }

    #[test]
    fn suggestion_bare_402_falls_back_to_subscription_hint() {
        // A 402 with an unrecognized code still steers to the billing surface.
        assert_eq!(
            api_err(0, 402).suggestion(),
            Some(HINT_SUBSCRIPTION_REQUIRED)
        );
    }

    #[test]
    fn suggestion_code_takes_precedence_over_http_status() {
        // 1695 maps to 402 server-side, but a 403-status response carrying
        // code 1695 must still yield the upgrade hint, not the 403 default.
        assert_eq!(api_err(1695, 403).suggestion(), Some(HINT_UPGRADE_REQUIRED));
    }

    #[test]
    fn suggestion_reason_required_error_code_maps_to_hint() {
        // The 422 reason-required error is keyed by its string error_code, not a
        // numeric code, and must yield the actionable reason hint (not the bare
        // 422 fallback, which is None).
        let e = ApiError::new(
            0,
            Some("ERR_REASON_REQUIRED".to_owned()),
            "comment_text required".to_owned(),
            422,
        );
        assert_eq!(e.suggestion(), Some(HINT_REASON_REQUIRED));
        // A plain 422 with no recognized code still yields no hint.
        assert_eq!(api_err(0, 422).suggestion(), None);
    }

    #[test]
    fn suggestion_unverified_email_code_unchanged() {
        assert!(
            api_err(10587, 403)
                .suggestion()
                .is_some_and(|s| s.contains("auth verify"))
        );
    }

    #[test]
    fn suggestion_plain_status_arms_unchanged() {
        assert!(api_err(0, 401).suggestion().is_some());
        assert!(api_err(0, 404).suggestion().is_some());
        assert_eq!(api_err(0, 418).suggestion(), None);
    }

    #[test]
    fn details_round_trips_through_cli_error() {
        let details = serde_json::json!({"params": [{"name": "x"}]});
        let api = ApiError {
            code: 1660,
            error_code: None,
            message: "conflict".to_owned(),
            http_status: 409,
            details: Some(Box::new(details.clone())),
        };
        let cli: CliError = api.into();
        match cli {
            CliError::Api(e) => assert_eq!(e.details.as_deref(), Some(&details)),
            _ => panic!("expected CliError::Api"),
        }
    }

    #[test]
    fn new_constructor_defaults_details_none() {
        assert!(
            ApiError::new(0, None, "x".to_owned(), 500)
                .details
                .is_none()
        );
    }

    #[test]
    fn display_without_details_is_unchanged() {
        // The headline shape must be byte-stable when there are no details.
        let e = ApiError::new(1650, Some("APP_X".to_owned()), "boom".to_owned(), 400);
        assert_eq!(e.to_string(), "[HTTP 400] boom (code 1650) [APP_X]");
    }

    #[test]
    fn display_renders_422_validation_report() {
        // A 422 template-validation report must surface in Display so the CLI
        // (anyhow→Display→stderr) and MCP (to_string) both show it.
        let details = serde_json::json!({
            "validation_report": {"ok": false, "fields": ["name", "steps[0].kind"]},
        });
        let e = ApiError {
            code: 1665,
            error_code: None,
            message: "template invalid".to_owned(),
            http_status: 422,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(rendered.contains("validation_report:"), "got: {rendered}");
        assert!(rendered.contains("name"), "field name surfaced: {rendered}");
        assert!(
            rendered.contains("steps[0].kind"),
            "nested field surfaced: {rendered}"
        );
    }

    /// `1680` is `APP_DENIED`, the platform's GENERIC access-denied code — not
    /// the importer's.
    ///
    /// Asserting the opposite — that the hint must name the cloud-import
    /// ownership rule ("member permission", "write-back") and must NOT be
    /// generic — would encode a bug as a requirement. `1680` is emitted from org
    /// transfer tokens, OAuth provider refusals, e-signing, audit-log gating and
    /// metadata templates; a cloud-import wording tells every one of those users
    /// about a subsystem they never touched.
    ///
    /// Now asserts the property that keeps it correct: the hint must carry NO
    /// subsystem-specific vocabulary, because it cannot know which subsystem
    /// rejected the call.
    #[test]
    fn generic_access_denied_1680_names_no_subsystem() {
        let hint = api_err(1680, 403).suggestion().expect("hint expected");
        assert_eq!(hint, HINT_ACCESS_DENIED);
        let lower = hint.to_lowercase();
        for domain in [
            "import",
            "write-back",
            "envelope",
            "sign",
            "oauth",
            "audit",
            "template",
        ] {
            assert!(
                !lower.contains(domain),
                "`1680` is APP_DENIED and reaches many subsystems — the hint must not \
                 name `{domain}`, or it is wrong everywhere else. got: {hint}"
            );
        }
        // It must still be actionable, not merely inoffensive.
        assert!(lower.contains("authenticated"), "got: {hint}");
        assert!(
            lower.contains("owner") || lower.contains("role"),
            "got: {hint}"
        );
    }

    #[test]
    fn reason_outranks_numeric_code_and_survives_code_consolidation() {
        let with_reason = |reason: &str, code: u32| {
            api_err(code, 400).with_details(json!({
                "params": [{ "name": "drive_id", "kind": "invalid", "reason": reason }]
            }))
        };
        // Same numeric code, different reasons => different hints. This is the
        // consolidation case that numeric-code branching would silently lose.
        let a = with_reason("drive_required", 146_652);
        let b = with_reason("drive_unknown", 146_652);
        assert_eq!(a.suggestion(), Some(HINT_DRIVE_REQUIRED));
        assert_eq!(b.suggestion(), Some(HINT_DRIVE_UNKNOWN));
        assert_ne!(a.suggestion(), b.suggestion());

        // Destination reasons resolve too.
        assert_eq!(
            with_reason("destination_nested", 146_652).suggestion(),
            Some(HINT_DESTINATION_NESTED)
        );
        assert_eq!(
            with_reason("destination_immutable", 146_652).suggestion(),
            Some(HINT_DESTINATION_IMMUTABLE)
        );
    }

    /// `10566` names the offending field but never the reason, so the bare
    /// message ("Invalid string in node 'title'") leaves the user with no idea
    /// whether the value was too long, too short, or malformed.
    ///
    /// Verified 2026-08-07: `share update --title` returns this exact
    /// code and text for BOTH a 1-character title and an 81-character one
    /// (documented bound: 2-80). Hence a resource-agnostic hint — asserting a
    /// specific cause here would be wrong half the time.
    #[test]
    fn invalid_node_field_maps_to_a_cause_agnostic_hint() {
        assert_eq!(
            api_err(10566, 406).suggestion(),
            Some(HINT_INVALID_NODE_FIELD)
        );
        // Must not claim a cause the code does not carry.
        assert!(
            HINT_INVALID_NODE_FIELD.contains("too long OR too short"),
            "must name both directions, since 10566 covers both"
        );
    }

    /// `10560` is an HTTP 401 that is NOT a missing login: the credential is
    /// authenticated but scoped to other workspaces. Without this arm the bare
    /// 401 fallback tells the user to `fastio auth login`, which cannot help —
    /// re-running login mints the same restricted credential.
    ///
    /// Reproduced live 2026-08-08 (`room state` against a workspace
    /// outside the test key's scope), which is how the gap was found.
    #[test]
    fn insufficient_scope_does_not_advise_logging_in_again() {
        assert_eq!(api_err(10560, 401).suggestion(), Some(HINT_TOKEN_SCOPE));
        // The whole point: it must NOT steer to a re-login. Case-insensitive and
        // matched on the verb, per the vacuous-negative-guard rule.
        assert!(
            !HINT_TOKEN_SCOPE.to_lowercase().contains("auth login"),
            "must not advise re-login for a scope failure"
        );
        assert!(
            HINT_TOKEN_SCOPE.to_lowercase().contains("scope"),
            "must name the actual cause"
        );
    }

    /// `107184` says only "not valid" — identical text for over-long and
    /// malformed — so the hint supplies the limit the server withholds.
    ///
    /// The number is asserted from the constant-free literal deliberately: it is
    /// a SERVER bound the CLI does not enforce, so there is nothing local to
    /// bind to. Verified 2026-08-08 post-deploy (128 CJK accepted, 129
    /// rejected); pre-deploy the same boundary was 128 BYTES.
    #[test]
    fn agent_name_invalid_hint_states_the_live_unit() {
        assert_eq!(
            api_err(107_184, 406).suggestion(),
            Some(HINT_AGENT_NAME_INVALID)
        );
        assert!(
            HINT_AGENT_NAME_INVALID.contains("128 characters"),
            "must state the CHARACTER bound that is live post-deploy"
        );
        assert!(
            !HINT_AGENT_NAME_INVALID.to_lowercase().contains("byte"),
            "the byte wording was correct only before the deploy (case-insensitive: \
             a `contains(\"BYTES\")` guard would pass on lowercase \"bytes\")"
        );
    }

    /// An unknown reason must fall through to the code/status hints rather than
    /// invent guidance for a condition this build does not know about.
    #[test]
    fn unknown_reason_falls_through_instead_of_guessing() {
        let e = api_err(1605, 400).with_details(json!({
            "params": [{ "name": "x", "reason": "some_future_reason" }]
        }));
        assert_eq!(e.suggestion(), Some(HINT_INVALID_INPUT));
    }

    #[test]
    fn field_reason_reads_the_params_array() {
        let e = api_err(1605, 400).with_details(json!({
            "params": [{ "name": "drive_id", "reason": "drive_required" }]
        }));
        assert_eq!(e.field_reason(), Some("drive_required"));
        // No params, or no reason => None, never a panic.
        assert_eq!(api_err(1605, 400).field_reason(), None);
        assert_eq!(
            api_err(1605, 400)
                .with_details(json!({ "params": [{ "name": "x" }] }))
                .field_reason(),
            None
        );
    }

    /// `params` also arrives OBJECT-shaped for endpoint-level enrichment — the
    /// CAS conflict is `{reason, current: {version_id, hash}}`. Reading only the
    /// array shape silently missed those, so the reason must be found in both.
    #[test]
    fn field_reason_reads_object_shaped_params() {
        let e = api_err(113_958, 409).with_details(json!({
            "params": {
                "reason": "CONFLICT_VERSION_MISMATCH",
                // Flat `current_version_id`, per the contract's §10: the
                // `current` object (and its `hash`) are retired and will not be
                // emitted. A hash can only be read AFTER the row lock releases,
                // so the pair could describe two different versions while
                // presenting as one coherent state.
                "current_version_id": "v3"
            }
        }));
        assert_eq!(e.field_reason(), Some("CONFLICT_VERSION_MISMATCH"));

        // An object with no reason is still None, never a panic.
        assert_eq!(
            api_err(1605, 400)
                .with_details(json!({ "params": { "current_version_id": "v3" } }))
                .field_reason(),
            None
        );
    }

    /// The destination bucket is deliberately one-cause-fits-all so a caller
    /// cannot probe for a node's existence. The hint must not undo that by
    /// telling the user to check whether the id exists.
    #[test]
    fn destination_unknown_hint_does_not_leak_an_existence_oracle() {
        for needle in [
            "does not exist",
            "not found",
            "no such",
            "wrong id",
            "invalid id",
        ] {
            assert!(
                !HINT_DESTINATION_UNKNOWN.to_lowercase().contains(needle),
                "destination hint must not distinguish causes ({needle})"
            );
        }
    }

    #[test]
    fn drive_required_hint_names_the_recovery_commands() {
        // The server message already states the condition; the CLI hint earns
        // its place only by naming the commands that resolve it.
        assert!(HINT_DRIVE_REQUIRED.contains("list-drives"));
        assert!(HINT_DRIVE_REQUIRED.contains("--drive-id"));
        assert!(HINT_DRIVE_REQUIRED.contains("refresh-drives"));
        assert!(HINT_DRIVE_UNKNOWN.contains("refresh-drives"));
        // A stale catalog is the likelier cause than a wrong id, so the hint
        // must not accuse the caller of passing a bad value.
        assert!(!HINT_DRIVE_UNKNOWN.to_lowercase().contains("invalid"));
    }

    #[test]
    fn suggestion_invalid_input_code_1605_maps_to_hint() {
        // Code 1605 ("Invalid Input") is a general-purpose 400 — storage
        // rename/transfer name-conflicts (corrected from HTTP 500 on 2026-06-14)
        // plus many other invalid-value cases. It must yield the actionable
        // invalid-input hint, not the bare-400 fallback (which is None). The hint
        // stays resource-agnostic (names no single feature, assumes no one cause).
        assert_eq!(api_err(1605, 400).suggestion(), Some(HINT_INVALID_INPUT));
        // A plain 400 with no recognized code still yields no hint.
        assert_eq!(api_err(0, 400).suggestion(), None);
        for needle in ["fileshare", "file share", "sign", "envelope", "workflow"] {
            assert!(
                !HINT_INVALID_INPUT.to_lowercase().contains(needle),
                "invalid-input hint must stay resource-agnostic ({needle})"
            );
        }
    }

    #[test]
    fn display_renders_object_valued_params() {
        // A decision CAS 409 carries an OBJECT-valued `error.params`
        // ({code, reason, current_round_id}), not an array. Each field must
        // surface in Display so the user sees WHY the decision was rejected and
        // which round is current — not just a bare 409.
        let details = serde_json::json!({
            "params": {
                "code": "ERR_DECISION_CAS_CONFLICT",
                "reason": "reviewer_already_decided_this_round",
                "current_round_id": "wr12345",
            },
        });
        let e = ApiError {
            code: 0,
            error_code: Some("ERR_DECISION_CAS_CONFLICT".to_owned()),
            message: "conflict".to_owned(),
            http_status: 409,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(
            rendered.contains("param code: ERR_DECISION_CAS_CONFLICT"),
            "object param `code` surfaced: {rendered}"
        );
        assert!(
            rendered.contains("param reason: reviewer_already_decided_this_round"),
            "object param `reason` surfaced: {rendered}"
        );
        assert!(
            rendered.contains("param current_round_id: wr12345"),
            "object param `current_round_id` surfaced: {rendered}"
        );
    }

    #[test]
    fn display_object_params_bounds_field_count() {
        // An object-valued params with more than MAX_RENDERED_PARAMS keys is
        // capped, with a "… (N more)" summary, mirroring the array path.
        let mut map = serde_json::Map::new();
        for i in 0..25 {
            map.insert(format!("k{i}"), serde_json::json!(format!("v{i}")));
        }
        let details = serde_json::json!({ "params": serde_json::Value::Object(map) });
        let e = ApiError {
            code: 0,
            error_code: None,
            message: "conflict".to_owned(),
            http_status: 409,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        // BTreeMap-style ordering is not guaranteed for serde_json::Map without
        // preserve_order, but with it (the crate enables it) insertion order
        // holds. Assert the cap + summary regardless of which keys land first.
        assert!(
            rendered.contains("… (15 more)"),
            "must summarize the elided object params: {rendered}"
        );
        // Exactly MAX_RENDERED_PARAMS `param ` lines render (10).
        assert_eq!(
            rendered.matches("\n  param ").count(),
            10,
            "object params must be capped at MAX_RENDERED_PARAMS: {rendered}"
        );
    }

    #[test]
    fn display_renders_409_reason() {
        // A trigger-fire 409 reason must surface in Display.
        let details = serde_json::json!({"reason": "dedup_hit"});
        let e = ApiError {
            code: 1660,
            error_code: None,
            message: "fire denied".to_owned(),
            http_status: 409,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(rendered.contains("reason: dedup_hit"), "got: {rendered}");
    }

    #[test]
    fn display_renders_400_params_and_links() {
        let details = serde_json::json!({
            "params": [
                {"name": "agent_credit_cap", "message": "must be a positive integer"},
                {"name": "visibility", "kind": "enum"},
            ],
            "documentation_url": "https://api.fast.io/docs",
            "resource": "decision/123",
        });
        let e = ApiError {
            code: 1640,
            error_code: None,
            message: "bad request".to_owned(),
            http_status: 400,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(
            rendered.contains("param agent_credit_cap: must be a positive integer"),
            "got: {rendered}"
        );
        // A param without a message falls back to its kind.
        assert!(
            rendered.contains("param visibility: enum"),
            "got: {rendered}"
        );
        assert!(rendered.contains("see: https://api.fast.io/docs"));
        assert!(rendered.contains("resource: decision/123"));
    }

    #[test]
    fn display_bounds_doc_url_resource_and_param_count() {
        let long_url = format!("https://api.fast.io/docs/{}", "u".repeat(1000));
        let long_resource = format!("decision/{}", "r".repeat(1000));
        // 25 params — well past the MAX_RENDERED_PARAMS cap of 10.
        let params: Vec<_> = (0..25)
            .map(|i| serde_json::json!({"name": format!("field_{i}"), "message": "bad"}))
            .collect();
        let details = serde_json::json!({
            "documentation_url": long_url,
            "resource": long_resource,
            "params": params,
        });
        let e = ApiError {
            code: 1640,
            error_code: None,
            message: "bad request".to_owned(),
            http_status: 400,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();

        // The doc URL and resource are truncated, not emitted raw.
        assert!(
            rendered.contains("(truncated)"),
            "doc/resource must be bounded: {}",
            rendered.len()
        );
        assert!(
            !rendered.contains(&"u".repeat(1000)),
            "raw oversized doc URL leaked"
        );
        assert!(
            !rendered.contains(&"r".repeat(1000)),
            "raw oversized resource leaked"
        );
        // Only the first 10 params render, plus a "… (15 more)" note.
        assert!(rendered.contains("param field_0: bad"));
        assert!(rendered.contains("param field_9: bad"));
        assert!(
            !rendered.contains("param field_10:"),
            "params past the cap must not render: {rendered}"
        );
        assert!(
            rendered.contains("… (15 more)"),
            "must summarize the elided params: {rendered}"
        );
    }

    #[test]
    fn display_truncates_overlong_detail() {
        let long = "x".repeat(1000);
        let details = serde_json::json!({ "reason": long });
        let e = ApiError {
            code: 0,
            error_code: None,
            message: "m".to_owned(),
            http_status: 409,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        assert!(
            rendered.contains("(truncated)"),
            "got len {}",
            rendered.len()
        );
        assert!(
            rendered.len() < 600,
            "render stayed bounded: {}",
            rendered.len()
        );
    }

    #[test]
    fn display_bounds_large_nested_object_param_value() {
        // A nested object/array value on an object-valued param must be serialized
        // with a byte cap and truncated — a large/hostile diagnostic cannot drive
        // an unbounded allocation during Display.
        let huge: Vec<serde_json::Value> = (0..50_000)
            .map(|i| serde_json::json!(format!("payload-element-{i}")))
            .collect();
        let details = serde_json::json!({
            "params": { "diagnostic": { "nested": huge } }
        });
        let e = ApiError {
            code: 0,
            error_code: None,
            message: "conflict".to_owned(),
            http_status: 409,
            details: Some(Box::new(details)),
        };
        let rendered = e.to_string();
        // The nested value is rendered (the field is not dropped) but clipped.
        assert!(
            rendered.contains("param diagnostic:"),
            "nested object param must still surface: {}",
            rendered.len()
        );
        assert!(
            rendered.contains("(truncated)"),
            "oversized nested value must be truncated: {}",
            rendered.len()
        );
        // The whole render stays small regardless of the multi-megabyte input.
        assert!(
            rendered.len() < 600,
            "render must stay bounded despite a huge nested value: {}",
            rendered.len()
        );
    }

    #[test]
    fn compact_json_bounded_caps_output() {
        // The bounded serializer never returns more than the byte cap, even for a
        // value whose full serialization would be orders of magnitude larger.
        let huge: Vec<serde_json::Value> = (0..50_000).map(|i| serde_json::json!(i)).collect();
        let out = compact_json_bounded(&serde_json::json!(huge));
        assert!(
            out.len() <= COMPACT_JSON_CAP,
            "bounded serialization exceeded the cap: {} > {COMPACT_JSON_CAP}",
            out.len()
        );
        assert!(
            out.starts_with('['),
            "captured the serialized prefix: {out}"
        );
    }

    /// Build an `ApiError` carrying an OBJECT-shaped `error.params` — the shape
    /// the failed-login lockout actually emits.
    /// Build an `ApiError` with an OBJECT-shaped `error.params`.
    ///
    /// **`code` is a PARAMETER on purpose.** Hard-coding `10760` for every case
    /// — including the positive-countdown test — would pin "a locked account may
    /// report attempts remaining" as expected behaviour, and the suite could not
    /// then catch that defect. The two codes mean opposite things and must never
    /// share a fixture:
    /// `10008` = wrong credentials (a countdown is meaningful),
    /// `10760` = ALREADY LOCKED (a countdown is never correct).
    fn params_err(code: u32, http_status: u16, params: serde_json::Value) -> ApiError {
        // Built by hand rather than via `json!` so `params` is genuinely moved
        // into the details object (the macro would only borrow it).
        let mut details = serde_json::Map::new();
        details.insert("params".to_owned(), params);
        ApiError {
            code,
            error_code: None,
            message: "auth failure".to_owned(),
            http_status,
            details: Some(Box::new(serde_json::Value::Object(details))),
        }
    }

    /// The pre-lockout wrong-credentials error (401 / `10008`) — the only shape
    /// for which a countdown is meaningful.
    fn attempts_err(params: serde_json::Value) -> ApiError {
        params_err(ERR_CREDENTIALS_INVALID, 401, params)
    }

    /// The lockout itself (429 / `10760`) — already locked.
    fn locked_err(params: serde_json::Value) -> ApiError {
        params_err(ERR_LOGIN_LOCKED, 429, params)
    }

    #[test]
    fn an_unreadable_body_yields_no_hint_at_any_status() {
        // `ERR_BODY_UNAVAILABLE`'s contract: a classifier keying on HTTP status
        // must treat the marker as UNKNOWN. `suggestion()` is THE status
        // classifier; ignoring it would violate a rule written 300 lines up in
        // this same file.
        //
        // The status is real; the reason is not, because the body carrying it
        // could not be read. The 401 case is the one that matters: it rendered
        // "Run `fastio auth login`", the exact harmful fallback this change
        // kills for 10175/10545/10008 everywhere else.
        for status in [401_u16, 403, 404, 429, 500] {
            let mut e = api_err(0, status);
            e.error_code = Some(ERR_BODY_UNAVAILABLE.to_owned());
            assert_eq!(
                e.suggestion(),
                None,
                "unreadable body must not be given a reason at HTTP {status}"
            );
        }
        // CONTROL — without the marker, the same statuses still hint normally,
        // so the gate is keyed on the marker and has not muted the fallbacks.
        assert!(api_err(0, 404).suggestion().is_some());
        assert!(api_err(0, 401).suggestion().is_some());
    }

    #[test]
    fn the_generic_403_names_both_axes_it_now_carries() {
        // The 2026-08-23 flip moved a whole family of CREDENTIAL-scope refusals
        // onto 403 (`10574`/`10753`/`10757` have no dedicated arm), so the old
        // role-only wording made a scope refusal read as a permissions problem
        // and sent the user to an admin who cannot help.
        //
        // Fixed at the STATUS fallback rather than by mapping each code: those
        // codes are per-call-site and still under discussion upstream, so
        // code-keyed arms risk never firing. This cannot go stale that way.
        let h = api_err(10_574, 403).suggestion().expect("hint");
        assert!(h.contains("role"), "keeps the permission axis: {h}");
        assert!(h.contains("scoped"), "adds the credential axis: {h}");
        assert!(
            h.contains("fastio auth scopes"),
            "and points at the command that shows the answer: {h}"
        );
        // A code WITH a specific recovery still outranks the fallback.
        assert_eq!(
            api_err(10175, 403).suggestion(),
            Some(HINT_SCOPE_INCORRECT),
            "10175 has a real recovery (finish 2FA) and must not fall through"
        );
    }

    #[test]
    fn code_10545_is_disambiguated_by_status_because_the_code_cannot_be() {
        // `10545` is emitted from four sites across TWO AXES. After the
        // 2026-08-23 flip the membership sites keep 401 while the token-scope
        // site answers 403, so the STATUS is the only discriminator available —
        // the deliberate inverse of the `10175` rule, and the reason that arm
        // keys on status while every neighbour keys on code.
        //
        // It matters because the membership advice is actively WRONG for the
        // scope case: no admin action widens a token's scope.
        let membership = api_err(10545, 401).suggestion().expect("hint");
        assert_eq!(membership, HINT_ENTITY_MEMBERSHIP);
        assert!(
            membership.contains("Ask an admin"),
            "401 is the membership axis: {membership}"
        );

        let scope = api_err(10545, 403).suggestion().expect("hint");
        assert_eq!(scope, HINT_ENTITY_SCOPE);
        assert!(
            !scope.contains("Ask an admin"),
            "403 is the CREDENTIAL axis — being added cannot help: {scope}"
        );
        assert!(
            scope.contains("NOT a membership problem"),
            "must say so explicitly: {scope}"
        );
        // Neither may steer to a re-login, the trap this family exists to avoid.
        for h in [membership, scope] {
            assert!(!h.to_lowercase().contains("auth login"), "{h}");
        }
    }

    #[test]
    fn scope_incorrect_hint_survives_the_401_to_403_change() {
        // `10175` ships as HTTP 401 today and moves to HTTP 403 (RFC 6750
        // `insufficient_scope`). The hint is keyed on the CODE precisely so the
        // flip cannot change it — pin BOTH statuses.
        assert_eq!(
            api_err(10175, 401).suggestion(),
            Some(HINT_SCOPE_INCORRECT),
            "401 (today): the code arm must outrank the status fallback"
        );
        assert_eq!(
            api_err(10175, 403).suggestion(),
            Some(HINT_SCOPE_INCORRECT),
            "403 (after the platform change): must be unchanged"
        );
    }

    #[test]
    fn scope_incorrect_never_tells_the_user_to_sign_in_again() {
        // THE bug this arm exists to prevent. The generic 401 fallback says "Run
        // `fastio auth login`" — and on a 2FA account (where the stored token is
        // `twofactor`-scoped) following that advice spends ANOTHER counted login
        // attempt, five of which lock the account. The hint must never steer
        // there, and must not claim a ROLE problem either (the 403 fallback's
        // wording) since scope is a property of the credential.
        let hint = api_err(10175, 401).suggestion().expect("hint expected");
        assert!(
            !hint.contains("auth login"),
            "must not steer into another counted login attempt: {hint}"
        );
        // Assert the FULL invocation, not the substring "2fa verify".
        // `fastio auth 2fa verify <code>` does not exist — `--code <CODE>` is
        // required and the positional form exits with "unexpected argument". A
        // substring assertion would pass for it anyway, endorsing a recovery
        // command that fails when run.
        assert!(
            hint.contains("fastio auth 2fa verify --code <CODE>"),
            "must name a command that actually runs: {hint}"
        );
        // The scope claim must stay narrow: retrying the SAME credential cannot
        // add scopes, but a different credential may legitimately have more.
        assert!(
            !hint.contains("a new token carries the same scopes"),
            "overstated: a fresh PKCE sign-in can carry broader scopes: {hint}"
        );
        let hint_403 = api_err(10175, 403).suggestion().expect("hint expected");
        assert!(
            !hint_403.contains("role"),
            "a scope failure is not a role problem: {hint_403}"
        );
    }

    #[test]
    fn invalid_credentials_hint_does_not_point_back_at_auth_login() {
        // Regression pin for a defect MEASURED live (2026-08-23): a
        // failed `fastio auth login` rendered the generic 401 hint "Run `fastio
        // auth login` to sign in" — telling the user to repeat the action that
        // just failed, spending another of five attempts before a 30-minute
        // lockout.
        let hint = api_err(10008, 401).suggestion().expect("hint expected");
        assert_eq!(hint, HINT_CREDENTIALS_INVALID);
        assert!(
            !hint.contains("Run `fastio auth login`"),
            "must not steer the user into another counted attempt: {hint}"
        );
        assert!(
            hint.contains("lockout"),
            "must warn that attempts are counted: {hint}"
        );
    }

    #[test]
    fn lockout_note_reports_the_wait_in_minutes_rounded_up() {
        // 1800s is the documented 30-minute soft lockout.
        let e = locked_err(serde_json::json!({ "retry_after_seconds": 1800 }));
        assert_eq!(
            e.lockout_note().as_deref(),
            Some("Account temporarily locked. Try again in about 30 minutes.")
        );
        // Rounds UP — advising a retry slightly too late is harmless; too early
        // is another refused request.
        let e = locked_err(serde_json::json!({ "retry_after_seconds": 61 }));
        assert_eq!(
            e.lockout_note().as_deref(),
            Some("Account temporarily locked. Try again in about 2 minutes.")
        );
        // Singular, not "1 minutes".
        let e = locked_err(serde_json::json!({ "retry_after_seconds": 30 }));
        assert_eq!(
            e.lockout_note().as_deref(),
            Some("Account temporarily locked. Try again in about 1 minute.")
        );
    }

    #[test]
    fn locked_account_never_reports_attempts_remaining() {
        // Code `10760` establishes that the account is ALREADY LOCKED. With an
        // unparseable `retry_after_seconds` (here, one above `u64::MAX`) a
        // fall-through to the `attempts_remaining` arm would tell a locked user
        // they had 3 tries left.
        //
        // A fixture hard-coding 10760 for every test INCLUDING the positive
        // countdown would pin that as correct, so fixtures are split by code and
        // this pins the opposite.
        let e = locked_err(serde_json::json!({
            "retry_after_seconds": "18446744073709551616",
            "attempts_remaining": 3,
            "attempts_max": 5,
        }));
        let note = e
            .lockout_note()
            .expect("a locked account must still say so");
        assert!(
            !note.contains("remaining before a temporary lockout"),
            "10760 must NEVER render a countdown: {note}"
        );
        assert!(
            note.contains("locked"),
            "must still report the lock: {note}"
        );
        // Same defect via a simply-absent wait value.
        let e = locked_err(serde_json::json!({ "attempts_remaining": 3, "attempts_max": 5 }));
        let note = e.lockout_note().expect("locked");
        assert!(
            !note.contains("remaining before a temporary lockout"),
            "absent wait must not reopen the countdown path: {note}"
        );
    }

    #[test]
    fn lock_establishing_401_gets_the_locked_hint_not_check_your_password() {
        // The dual-message bug, pinned on the HINT side. `lockout_note()` is
        // covered elsewhere; without this, a regression restoring
        // `HINT_CREDENTIALS_INVALID` here would pass the whole suite.
        //
        // Observed live: the FIFTH failure returns 401/10008 with
        // `attempts_remaining: 0`, and the account is already locked — so
        // "check the password" is wrong; the next request never evaluates one.
        let e = attempts_err(serde_json::json!({ "attempts_remaining": 0, "attempts_max": 5 }));
        assert_eq!(e.suggestion(), Some(HINT_LOGIN_LOCKED));
        // ...while attempts DO remain, the credentials hint is the right one.
        let e = attempts_err(serde_json::json!({ "attempts_remaining": 2, "attempts_max": 5 }));
        assert_eq!(e.suggestion(), Some(HINT_CREDENTIALS_INVALID));
    }

    #[test]
    fn note_and_hint_agree_on_contradictory_lock_fields() {
        // A `10008` carrying BOTH a wait and a non-zero remaining count is
        // self-inconsistent (the origin never sends it). It used to render
        // "locked for about two minutes" from `lockout_note()` directly above
        // "the email or password is incorrect ... attempts remain" from
        // `suggestion()`, because the two used different lock predicates.
        // They now share `is_locked_out()`, so they cannot disagree.
        let e = attempts_err(serde_json::json!({
            "retry_after_seconds": 120,
            "attempts_remaining": 3,
            "attempts_max": 5,
        }));
        let note = e.lockout_note().expect("note expected");
        assert!(note.contains("locked"), "note says locked: {note}");
        assert_eq!(
            e.suggestion(),
            Some(HINT_LOGIN_LOCKED),
            "hint must agree with the note, not contradict it"
        );
    }

    #[test]
    fn lockout_note_is_gated_to_the_two_sign_in_codes() {
        // `Display` calls `lockout_note` for EVERY ApiError, so without a code
        // gate any error carrying `retry_after_seconds` announces an account
        // lockout — e.g. an ordinary throttled download. This pins the gate.
        let unrelated = params_err(1605, 429, serde_json::json!({ "retry_after_seconds": 30 }));
        assert_eq!(
            unrelated.lockout_note(),
            None,
            "a non-sign-in code must never claim an account lockout"
        );
        let unrelated = params_err(
            115_069,
            401,
            serde_json::json!({ "attempts_remaining": 2, "attempts_max": 5 }),
        );
        assert_eq!(unrelated.lockout_note(), None);
    }

    #[test]
    fn lockout_note_treats_zero_attempts_as_already_locked() {
        // `attempts_remaining: 0` means the account is ALREADY LOCKED, not
        // "one more failure will lock you" — this is easy to record backwards,
        // and some server-side documentation
        // still states it backwards. Measured: 0 arrived on the FIFTH failure
        // and the SIXTH request was refused 429.
        let e = attempts_err(serde_json::json!({ "attempts_remaining": 0 }));
        let note = e.lockout_note().expect("note expected");
        assert!(
            note.contains("no sign-in attempts remain"),
            "0 must read as already-locked: {note}"
        );
        assert!(
            !note.contains("before a temporary lockout"),
            "0 must NOT read as a countdown: {note}"
        );
    }

    #[test]
    fn lockout_note_renders_the_countdown_with_and_without_a_max() {
        let e = attempts_err(serde_json::json!({ "attempts_remaining": 3, "attempts_max": 5 }));
        assert_eq!(
            e.lockout_note().as_deref(),
            Some("3 of 5 sign-in attempts remaining before a temporary lockout.")
        );
        let e = attempts_err(serde_json::json!({ "attempts_remaining": 1 }));
        assert_eq!(
            e.lockout_note().as_deref(),
            Some("1 sign-in attempt remaining before a temporary lockout."),
            "singular, and no invented max"
        );
    }

    #[test]
    fn lockout_note_drops_an_impossible_max() {
        // `remaining > max` is not a state the server can be in, so the pair is
        // untrustworthy — print the count alone rather than "18446744073709551615
        // of 5". Both values parse, so nothing else would have caught this.
        let e =
            attempts_err(serde_json::json!({ "attempts_remaining": u64::MAX, "attempts_max": 5 }));
        let note = e.lockout_note().expect("note expected");
        assert!(
            !note.contains(" of 5"),
            "impossible max must be dropped: {note}"
        );
    }

    #[test]
    fn lockout_note_absent_is_not_zero() {
        // ABSENT means the count could not be recorded — NOT that zero
        // attempts remain. The platform documents this rule itself: "treat
        // absence as unknown ... rather than assuming zero".
        assert_eq!(attempts_err(serde_json::json!({})).lockout_note(), None);
        assert_eq!(
            api_err(ERR_CREDENTIALS_INVALID, 401).lockout_note(),
            None,
            "no params at all must yield no note"
        );
        // A non-numeric value is also 'no number', not 0.
        assert_eq!(
            attempts_err(serde_json::json!({ "attempts_remaining": "many" })).lockout_note(),
            None
        );
        // POSITIVE CONTROL — this test asserts only absences, so a completely
        // dead accessor would satisfy every assertion above and prove nothing.
        // Verified: with `param_u64`'s object arm stubbed to `None`, only this
        // line fails.
        assert!(
            attempts_err(serde_json::json!({ "attempts_remaining": 2 }))
                .lockout_note()
                .is_some(),
            "control: a PRESENT count must still produce a note"
        );
    }

    #[test]
    fn lockout_note_accepts_string_encoded_numbers() {
        // The framework string-encodes some numerics; a quoted count must not
        // silently disable the note.
        let e = locked_err(serde_json::json!({ "retry_after_seconds": "1800" }));
        assert_eq!(
            e.lockout_note().as_deref(),
            Some("Account temporarily locked. Try again in about 30 minutes.")
        );
    }

    #[test]
    fn lockout_note_ignores_array_shaped_params() {
        // The ARRAY form is the per-field validation list — a DIFFERENT payload
        // that never carries lockout keys; the web app uses the shape itself as
        // the discriminator. A forward-compat `{name, value}` lookup is
        // deliberately absent: it could let a rejection of a field NAMED
        // `attempts_remaining` render as a sign-in countdown.
        let e = attempts_err(serde_json::json!([
            { "name": "attempts_remaining", "kind": "range", "message": "invalid", "value": 2 }
        ]));
        assert_eq!(
            e.lockout_note(),
            None,
            "a validation row must never be read as sign-in state"
        );
    }

    #[test]
    fn display_appends_the_lockout_note_and_keeps_the_raw_params() {
        // The interpreted sentence is ADDITIVE: the raw `param` diagnostics stay
        // for support correlation.
        let e = locked_err(serde_json::json!({ "retry_after_seconds": 1800 }));
        let rendered = e.to_string();
        assert!(
            rendered.contains("param retry_after_seconds: 1800"),
            "raw diagnostic preserved: {rendered}"
        );
        assert!(
            rendered.contains("Try again in about 30 minutes."),
            "interpreted sentence appended: {rendered}"
        );
    }

    /// Every hint the scope/access-mode refusal table can emit.
    const ALL_SCOPE_REFUSAL_HINTS: [&str; 16] = [
        HINT_SCOPE_ADMIN_REQUIRED_API_KEY,
        HINT_SCOPE_ADMIN_REQUIRED_OAUTH,
        HINT_SCOPE_ADMIN_REQUIRED_SESSION,
        HINT_SCOPE_ADMIN_REQUIRED_GENERIC,
        HINT_SCOPE_EXCEEDS_ISSUER_API_KEY,
        HINT_SCOPE_EXCEEDS_ISSUER_OAUTH,
        HINT_SCOPE_EXCEEDS_ISSUER_SESSION,
        HINT_SCOPE_EXCEEDS_ISSUER_GENERIC,
        HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY,
        HINT_USERDETAILS_SCOPE_REQUIRED_OAUTH,
        HINT_USERDETAILS_SCOPE_REQUIRED_SESSION,
        HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC,
        HINT_SCOPE_WRITE_REQUIRED_API_KEY,
        HINT_SCOPE_WRITE_REQUIRED_OAUTH,
        HINT_SCOPE_WRITE_REQUIRED_SESSION,
        HINT_SCOPE_WRITE_REQUIRED_GENERIC,
    ];

    /// The credential-type column of the refusal matrix: the three the server
    /// emits, plus the two ways it can fail to name one (absent, unrecognised),
    /// both of which must land on the GENERIC member.
    const CREDENTIAL_TYPE_CASES: [Option<&str>; 5] = [
        Some("api_key"),
        Some("oauth"),
        Some("session"),
        None,
        Some("robot"),
    ];

    /// A 403 refusal with the OBJECT-shaped `params` the server actually sends.
    fn scope_err(code: u32, reason: &str, credential_type: Option<&str>) -> ApiError {
        let mut params = serde_json::Map::new();
        params.insert("reason".to_owned(), json!(reason));
        params.insert("entity_type".to_owned(), json!("org"));
        params.insert("entity_id".to_owned(), json!("1234567890123456789"));
        if let Some(ct) = credential_type {
            params.insert("credential_type".to_owned(), json!(ct));
        }
        api_err(code, 403).with_details(json!({ "params": params }))
    }

    /// The full 5-reason × 5-credential-type matrix.
    ///
    /// Every cell must render its own const — never `None`, and never the
    /// generic 403 fallback, whose "your account lacks the required role"
    /// wording is the exact advice these hints exist to displace.
    #[test]
    fn scope_refusal_matrix_renders_the_credential_specific_hint() {
        let generic_403 = api_err(0, 403).suggestion();
        assert!(
            generic_403.is_some(),
            "fixture: the 403 fallback must exist"
        );

        let cases: [(u32, &str, [&str; 4]); 5] = [
            (
                10767,
                "scope_admin_required",
                [
                    HINT_SCOPE_ADMIN_REQUIRED_API_KEY,
                    HINT_SCOPE_ADMIN_REQUIRED_OAUTH,
                    HINT_SCOPE_ADMIN_REQUIRED_SESSION,
                    HINT_SCOPE_ADMIN_REQUIRED_GENERIC,
                ],
            ),
            (
                10768,
                "scope_exceeds_issuer",
                [
                    HINT_SCOPE_EXCEEDS_ISSUER_API_KEY,
                    HINT_SCOPE_EXCEEDS_ISSUER_OAUTH,
                    HINT_SCOPE_EXCEEDS_ISSUER_SESSION,
                    HINT_SCOPE_EXCEEDS_ISSUER_GENERIC,
                ],
            ),
            (
                10768,
                "access_mode_exceeds_initiate",
                [
                    HINT_SCOPE_EXCEEDS_ISSUER_API_KEY,
                    HINT_SCOPE_EXCEEDS_ISSUER_OAUTH,
                    HINT_SCOPE_EXCEEDS_ISSUER_SESSION,
                    HINT_SCOPE_EXCEEDS_ISSUER_GENERIC,
                ],
            ),
            (
                10769,
                "userdetails_scope_required",
                [
                    HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY,
                    HINT_USERDETAILS_SCOPE_REQUIRED_OAUTH,
                    HINT_USERDETAILS_SCOPE_REQUIRED_SESSION,
                    HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC,
                ],
            ),
            (
                10770,
                "scope_write_required",
                [
                    HINT_SCOPE_WRITE_REQUIRED_API_KEY,
                    HINT_SCOPE_WRITE_REQUIRED_OAUTH,
                    HINT_SCOPE_WRITE_REQUIRED_SESSION,
                    HINT_SCOPE_WRITE_REQUIRED_GENERIC,
                ],
            ),
        ];

        for (code, reason, [api_key, oauth, session, generic]) in cases {
            let expected = [
                (Some("api_key"), api_key),
                (Some("oauth"), oauth),
                (Some("session"), session),
                (None, generic),
                // Unrecognised credential types must degrade to GENERIC, never
                // to no hint at all.
                (Some("robot"), generic),
            ];
            for (credential_type, want) in expected {
                let got = scope_err(code, reason, credential_type).suggestion();
                assert_eq!(
                    got,
                    Some(want),
                    "{reason} / {credential_type:?} rendered the wrong hint"
                );
                assert!(
                    got.is_some(),
                    "{reason} / {credential_type:?} must never fall through to no hint"
                );
                assert_ne!(
                    got, generic_403,
                    "{reason} / {credential_type:?} must not fall back to the generic 403"
                );
            }
        }
    }

    /// `access_mode_exceeds_initiate` is the same refusal as
    /// `scope_exceeds_issuer` seen earlier in the flow — settled deliberately as
    /// one hint family, so the two must be indistinguishable to a user.
    #[test]
    fn access_mode_exceeds_initiate_matches_scope_exceeds_issuer() {
        for credential_type in CREDENTIAL_TYPE_CASES {
            let initiate = scope_err(10768, "access_mode_exceeds_initiate", credential_type);
            let exceeds = scope_err(10768, "scope_exceeds_issuer", credential_type);
            assert_eq!(
                initiate.suggestion(),
                exceeds.suggestion(),
                "the two reasons share one family ({credential_type:?})"
            );
            assert!(initiate.suggestion().is_some());
        }
    }

    /// Wording guards.
    ///
    /// An access mode is a property of the CREDENTIAL, so calling it a "role" or
    /// sending the reader to an admin is the failure mode these hints replace —
    /// no admin action widens a credential's own grant. The bare word `admin` is
    /// required and stays.
    ///
    /// Account settings are a separate SCOPE, not a wider access mode, so the
    /// `userdetails` hints must never recommend `--admin`.
    #[test]
    fn scope_refusal_hints_avoid_the_role_and_ask_an_admin_wording() {
        for hint in ALL_SCOPE_REFUSAL_HINTS {
            let lower = hint.to_lowercase();
            for needle in ["role", "ask an admin", "admin of the org"] {
                assert!(
                    !lower.contains(needle),
                    "an access mode is not a role — `{needle}` must not appear in: {hint}"
                );
            }
            // The line continuations must render single spaces, not the source
            // indentation, and never a newline.
            assert!(
                !hint.contains("  "),
                "double space in rendered hint: {hint}"
            );
            assert!(!hint.contains('\n'), "newline in rendered hint: {hint}");
        }
        for hint in [
            HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY,
            HINT_USERDETAILS_SCOPE_REQUIRED_OAUTH,
            HINT_USERDETAILS_SCOPE_REQUIRED_SESSION,
            HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC,
        ] {
            assert!(
                !hint.contains("--admin"),
                "account settings are a scope, not an access mode: {hint}"
            );
        }
        // Write is not admin. A read-only credential needs a read-write access
        // mode; recommending `--admin` would ask for a strictly higher ceiling
        // the operation never required, and a credential that cannot obtain it
        // would be told to give up on a request it could actually satisfy.
        for hint in [
            HINT_SCOPE_WRITE_REQUIRED_API_KEY,
            HINT_SCOPE_WRITE_REQUIRED_OAUTH,
            HINT_SCOPE_WRITE_REQUIRED_SESSION,
            HINT_SCOPE_WRITE_REQUIRED_GENERIC,
        ] {
            assert!(
                !hint.contains("--admin"),
                "write is not admin — an rwa ceiling is not the remedy: {hint}"
            );
            assert!(
                hint.contains("read-only"),
                "the refusal is about a read-only access mode; the hint must \
                 name it: {hint}"
            );
            assert!(
                hint.contains("--read-only"),
                "the flag that produced the read-only credential must be named \
                 so the reader knows what to drop: {hint}"
            );
        }
    }

    /// A key update REPLACES the whole scope set, so any hint that tells the
    /// reader to run `api-key update` must first tell them to read the key back
    /// and re-state what it should keep. Without that sentence the hint's own
    /// advice silently deletes every scope it did not name.
    #[test]
    fn api_key_update_hints_say_the_update_replaces_the_whole_scope_set() {
        for hint in [
            HINT_SCOPE_ADMIN_REQUIRED_API_KEY,
            HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY,
            HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC,
            HINT_SCOPE_WRITE_REQUIRED_API_KEY,
            HINT_SCOPE_WRITE_REQUIRED_GENERIC,
        ] {
            assert!(
                hint.contains("REPLACES"),
                "the wholesale replacement must be stated: {hint}"
            );
            assert!(
                hint.contains("api-key get"),
                "the reader must be told to read the key back first: {hint}"
            );
            assert!(
                hint.contains("re-specify every scope it should keep"),
                "the reader must be told to re-state the scopes to keep: {hint}"
            );
            assert!(
                hint.contains("api-key update"),
                "the command that applies the re-stated scopes must be named: {hint}"
            );
        }

        // `--admin` is applied to every scope the invocation names, so a key
        // holding a read-only scope alongside an admin one cannot be re-stated
        // with the selector flags without escalating it. Only the admin hint
        // recommends `--admin`, so only it needs the escape hatch.
        assert!(
            HINT_SCOPE_ADMIN_REQUIRED_API_KEY.contains("--scopes"),
            "the mixed-access-mode escape hatch must name `--scopes`: \
             {HINT_SCOPE_ADMIN_REQUIRED_API_KEY}"
        );
        assert!(
            HINT_SCOPE_ADMIN_REQUIRED_API_KEY.contains("read-only"),
            "the reason for `--scopes` — not escalating a read-only scope — must \
             be stated: {HINT_SCOPE_ADMIN_REQUIRED_API_KEY}"
        );
    }

    /// The example invocations in those same hints must be COPY-PASTEABLE.
    ///
    /// `--org/--workspace/--share <id>` reads as a menu to a human and is not a
    /// flag any of these commands accepts: a reader who pastes it gets
    /// `unexpected argument`. The example therefore names the selectors
    /// separately, and names more than one so it cannot be re-collapsed into a
    /// single hard-coded `--org`.
    ///
    /// The GENERIC members show an api-key invocation too, so they carry the
    /// same obligation: an org-only example reads as though a key can only ever
    /// be scoped to an org.
    #[test]
    fn api_key_update_hint_examples_are_valid_selector_neutral_syntax() {
        for hint in [
            HINT_SCOPE_ADMIN_REQUIRED_API_KEY,
            HINT_SCOPE_ADMIN_REQUIRED_GENERIC,
            HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY,
            HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC,
            HINT_SCOPE_WRITE_REQUIRED_API_KEY,
            HINT_SCOPE_WRITE_REQUIRED_GENERIC,
        ] {
            assert!(
                !hint.contains("--org/--workspace"),
                "`--org/--workspace/--share <id>` is not valid CLI syntax: {hint}"
            );
            assert!(
                hint.contains("--workspace <id>"),
                "the example must name more than one selector, not just \
                 `--org`: {hint}"
            );
            assert!(
                hint.contains("--org <id>") && hint.contains("--share <id>"),
                "every entity selector must be spelled out separately: {hint}"
            );
            assert!(
                hint.contains("--all"),
                "the all-entities selector must be offered alongside the \
                 per-entity ones: {hint}"
            );
            assert!(
                !hint.contains("<org-id>"),
                "an org-only placeholder re-introduces the single-selector \
                 example: {hint}"
            );
        }
    }

    /// `--admin` raises the ACCESS MODE; it does not grant `userdetails:*:rw`.
    /// A hint for an over-broad request that named only `--admin` would send a
    /// caller who needs account settings to a login that still cannot do it, so
    /// the issuer family must name both ceilings or neither.
    #[test]
    fn exceeds_issuer_hints_do_not_prescribe_admin_alone() {
        for hint in [
            HINT_SCOPE_EXCEEDS_ISSUER_API_KEY,
            HINT_SCOPE_EXCEEDS_ISSUER_OAUTH,
            HINT_SCOPE_EXCEEDS_ISSUER_SESSION,
            HINT_SCOPE_EXCEEDS_ISSUER_GENERIC,
        ] {
            assert!(
                hint.contains("--account-settings"),
                "a hint naming `--admin` must also name the account-settings \
                 ceiling, which admin does not confer: {hint}"
            );
            assert!(
                hint.contains("already holds the access you asked for"),
                "the remedy is a more privileged ISSUER, not a flag: {hint}"
            );
        }
    }

    /// The `reason` outranks the numeric `code`, exactly as it does for the
    /// import reasons: a refusal carrying an unrelated code that has its own
    /// arm must still render the scope hint.
    #[test]
    fn scope_reason_outranks_an_unrelated_numeric_code() {
        // `10175` has a code-keyed arm of its own further down `suggestion()`.
        let e = scope_err(10175, "scope_admin_required", Some("oauth"));
        assert_eq!(e.suggestion(), Some(HINT_SCOPE_ADMIN_REQUIRED_OAUTH));
        assert_ne!(e.suggestion(), Some(HINT_SCOPE_INCORRECT));

        // And a code from an unrelated family behaves the same way.
        assert_eq!(
            scope_err(1680, "userdetails_scope_required", Some("api_key")).suggestion(),
            Some(HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY)
        );
    }

    /// The ARRAY shape is the per-field validation payload: `field_reason()`
    /// reads it, but `param_str` deliberately does not, so a credential type
    /// riding in an array row is NOT read and the hint degrades to GENERIC
    /// rather than trusting a value from the wrong payload.
    #[test]
    fn array_shaped_params_resolve_the_reason_but_not_the_credential_type() {
        let e = api_err(10767, 403).with_details(json!({
            "params": [{
                "name": "scopes",
                "kind": "invalid",
                "reason": "scope_admin_required",
                "credential_type": "oauth"
            }]
        }));
        assert_eq!(e.field_reason(), Some("scope_admin_required"));
        assert_eq!(e.param_str("credential_type"), None);
        assert_eq!(e.suggestion(), Some(HINT_SCOPE_ADMIN_REQUIRED_GENERIC));
        assert_ne!(e.suggestion(), Some(HINT_SCOPE_ADMIN_REQUIRED_OAUTH));
    }

    /// A scope refusal that arrives with NO `params` still carries its numeric
    /// code, and the reason-keyed table cannot see it. Without a code-keyed
    /// fallback those refusals landed on the generic 403 line — whose "your
    /// account lacks the required role" wording is precisely what this family
    /// exists to displace — so each code must resolve to the GENERIC member of
    /// its family, the only member correct when the credential type is unknown.
    #[test]
    fn scope_codes_without_params_use_the_generic_hint() {
        let generic_403 = api_err(0, 403).suggestion();
        assert!(
            generic_403.is_some(),
            "fixture: the 403 fallback must exist"
        );

        for (code, want) in [
            (10767, HINT_SCOPE_ADMIN_REQUIRED_GENERIC),
            (10768, HINT_SCOPE_EXCEEDS_ISSUER_GENERIC),
            (10769, HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC),
            (10770, HINT_SCOPE_WRITE_REQUIRED_GENERIC),
        ] {
            let got = api_err(code, 403).suggestion();
            assert_eq!(got, Some(want), "code {code} with no params");
            assert_ne!(
                got, generic_403,
                "code {code} must not fall back to the generic 403"
            );
            let rendered = got.unwrap_or_default().to_lowercase();
            assert!(
                !rendered.contains("role"),
                "code {code} must not call an access mode a role: {rendered}"
            );
        }
    }

    /// Regression guard on the insertion point: the new branch sits ahead of the
    /// reason-only table, so every reason it does not claim must still reach the
    /// old hint unchanged.
    #[test]
    fn non_scope_reasons_are_unaffected_by_the_scope_branch() {
        let array_shaped = api_err(146_652, 400).with_details(json!({
            "params": [{ "name": "drive_id", "kind": "invalid", "reason": "drive_required" }]
        }));
        assert_eq!(array_shaped.suggestion(), Some(HINT_DRIVE_REQUIRED));

        let object_shaped = api_err(146_652, 400).with_details(json!({
            "params": { "reason": "destination_nested", "credential_type": "oauth" }
        }));
        assert_eq!(object_shaped.suggestion(), Some(HINT_DESTINATION_NESTED));

        // An unknown reason still falls through to the code/status table.
        let unknown = scope_err(1688, "some_future_reason", Some("oauth"));
        assert_eq!(unknown.suggestion(), Some(HINT_SUBSCRIPTION_REQUIRED));
    }

    /// A hint earns its place only by naming the flag that resolves the refusal
    /// — and the two families recommend DIFFERENT flags.
    #[test]
    fn scope_refusal_hints_name_the_flag_they_recommend() {
        for hint in [
            HINT_SCOPE_ADMIN_REQUIRED_API_KEY,
            HINT_SCOPE_ADMIN_REQUIRED_OAUTH,
            HINT_SCOPE_ADMIN_REQUIRED_SESSION,
            HINT_SCOPE_ADMIN_REQUIRED_GENERIC,
            HINT_SCOPE_EXCEEDS_ISSUER_API_KEY,
            HINT_SCOPE_EXCEEDS_ISSUER_OAUTH,
            HINT_SCOPE_EXCEEDS_ISSUER_SESSION,
            HINT_SCOPE_EXCEEDS_ISSUER_GENERIC,
        ] {
            assert!(hint.contains("--admin"), "must name the flag: {hint}");
        }
        for hint in [
            HINT_USERDETAILS_SCOPE_REQUIRED_API_KEY,
            HINT_USERDETAILS_SCOPE_REQUIRED_OAUTH,
            HINT_USERDETAILS_SCOPE_REQUIRED_SESSION,
            HINT_USERDETAILS_SCOPE_REQUIRED_GENERIC,
        ] {
            assert!(
                hint.contains("--account-settings"),
                "must name the flag: {hint}"
            );
        }
    }
}
