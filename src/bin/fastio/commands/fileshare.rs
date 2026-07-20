//! File Share (`fastio fileshare`) command handlers.
//!
//! Surface over [`fastio_cli::api::fileshare`] (management + consumption) plus
//! the write-back path in [`fastio_cli::api::upload`]. The disciplines this
//! module enforces:
//!
//! - **Anonymous-capable reads.** `info` / `download` / `versions` / `preview`
//!   build their client via [`CommandContext::build_client_allow_anonymous`] so
//!   a public (`anyone_with_link`) share serves without a token, and an EXPIRED
//!   stored profile credential falls back to anonymous (with one warning)
//!   rather than hard-blocking a read that may not need auth. Management /
//!   `upload` / `ws-token` / `activity` stay on the always-authed
//!   [`CommandContext::build_client`].
//! - **Password handling.** A link password comes from `--password` OR the
//!   `FASTIO_FILESHARE_PASSWORD` env var (flag wins), is wrapped in a
//!   [`SecretString`] immediately, and travels only in the `x-ve-password`
//!   header (threaded by the Wave-1 client helpers). The clap enum redacts the
//!   plaintext from `Debug` (see `cli.rs`).
//! - **Confirmation gates.** `delete`, `grants remove`, and `upload` (a
//!   destructive new-version write) require `--yes` or an interactive y/N
//!   confirmation; a non-interactive caller without `--yes` is blocked
//!   deterministically (mirrors `sign.rs`'s `confirm_destructive`).
//! - **Error mapping.** All File-Share-specific wording lives HERE in
//!   [`map_fileshare_error`] (never in the global `error.rs` hints, which stay
//!   resource-agnostic). A `1609`/`404` is surfaced UNIFORMLY ("unavailable")
//!   so an expired/revoked/never-existed share is never distinguished.
//! - **Write-back CAS.** `--if-version` is a SERVER-ENFORCED precondition: when
//!   the server detects a version conflict it ends the session terminally
//!   (`assembly_failed` + `CONFLICT_VERSION_MISMATCH:{vid}`), which the CLI
//!   parses into [`CliError::VersionConflict`] with the current version id.
//! - **Secret tokens.** `ws-token` mints a realtime token that is REDACTED from
//!   stdout and written 0600 to `--token-file`, reusing the shared
//!   [`super::secret_output`] helpers.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

use fastio_cli::api::{event, fileshare, upload};
use fastio_cli::error::CliError;

use crate::cli::{FileshareCommands, FileshareGrantsCommands};

use super::CommandContext;
use super::secret_output::{extract_secret, redact_secret_field, write_secret_file};

/// Environment variable consulted for the link password when `--password` is
/// omitted (keeps the plaintext out of `ps` / shell history).
const PASSWORD_ENV: &str = "FASTIO_FILESHARE_PASSWORD";

/// Default chunk size for the chunked write-back path (8 MiB). Matches the
/// upload command's working chunk size.
const WRITEBACK_CHUNK_SIZE: usize = 8 * 1024 * 1024;

/// Max status-poll attempts for a chunked write-back (≈ 2 min at 2s/attempt).
const WRITEBACK_POLL_ATTEMPTS: u32 = 60;

// ─── Password resolution ────────────────────────────────────────────────────

/// Resolve the optional link password, PRESERVING flag PRESENCE.
///
/// Precedence: a `--password` flag that is PRESENT wins — even when its value is
/// the empty string. An explicitly supplied empty flag (`--password ""`) flows
/// through as `Some("")` so the Wave-1 library validator rejects it with its
/// clear message ("the --password value must not be empty …"). It must NEVER be
/// silently downgraded to "absent", because that would let `--password ""` fall
/// back to the `FASTIO_FILESHARE_PASSWORD` env var (using a value the user did
/// not intend) or, on create, produce an UNPROTECTED share from what was meant
/// to be password-protected — bypassing the empty-password validators.
///
/// The env var (`FASTIO_FILESHARE_PASSWORD`) is consulted ONLY when the flag is
/// entirely absent (`None`); an empty env value is treated as unset.
///
/// The result is wrapped in a [`SecretString`] immediately so the plaintext
/// lifetime is minimal and no `String` copy lingers. Reading the env var HERE
/// (not via clap `env=`) keeps the plaintext out of the parsed argument struct
/// entirely on the env path.
fn resolve_password(flag: Option<&str>) -> Option<SecretString> {
    // Flag PRESENCE is load-bearing: a present-but-empty flag must reach the
    // validator, so do NOT `.filter(|s| !s.is_empty())` here.
    if let Some(value) = flag {
        return Some(SecretString::from(value.to_owned()));
    }
    std::env::var(PASSWORD_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .map(SecretString::from)
}

/// Resolve the link password for an `update`, honoring `--clear-password`.
///
/// When `--clear-password` is set the password is being REMOVED, so the env var
/// must NOT be resolved: a `FASTIO_FILESHARE_PASSWORD` value would otherwise
/// produce `password=Some(env)` alongside `clear_password=true`, which the
/// library validator rejects ("choose either --password or --clear-password") —
/// making a perfectly valid clear fail whenever the env var happens to be set.
/// (`--password` and `--clear-password` are already a clap conflict, so the flag
/// path can never collide here.) When `--clear-password` is NOT set, this defers
/// to the normal [`resolve_password`] precedence.
fn resolve_update_password(flag: Option<&str>, clear_password: bool) -> Option<SecretString> {
    if clear_password {
        return None;
    }
    resolve_password(flag)
}

/// Resolve the link password for a CONSUMPTION / WRITE-BACK path
/// (`info` / `download` / `versions` / `preview` / `upload`), rejecting a
/// PRESENT-but-EMPTY value.
///
/// On these paths the resolved password is applied DIRECTLY as the
/// `x-ve-password` header — the library `validate()` (which rejects an empty
/// password on management create/update) never runs. A link password is
/// contractually 1-255 chars, so a present `""` (an explicit `--password ""`)
/// is invalid: sending an empty header is meaningless and only masks an
/// unprotected-share mistake. An ABSENT password (`None`) is the correct way to
/// consume an UNPROTECTED share, so only a PRESENT empty string is rejected.
/// `resolve_password` already treats an empty ENV value as absent, so the only
/// way to reach `Some("")` here is an explicit empty flag. The value is NEVER
/// echoed in the error.
fn resolve_consumption_password(flag: Option<&str>) -> Result<Option<SecretString>> {
    let resolved = resolve_password(flag);
    if let Some(pw) = resolved.as_ref()
        && pw.expose_secret().is_empty()
    {
        anyhow::bail!("link password cannot be empty — omit --password for an unprotected share");
    }
    Ok(resolved)
}

// ─── Confirmation ───────────────────────────────────────────────────────────

/// Gate a destructive / outward-facing action behind explicit confirmation.
///
/// `--yes` proceeds unconditionally. Without it, an interactive TTY is prompted
/// y/N; a non-interactive caller that omitted `--yes` is blocked
/// deterministically (so an unattended script never silently deletes, revokes,
/// or writes a new version). Mirrors `sign.rs`'s `confirm_destructive`.
fn confirm_destructive(action: &str, detail: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    if !interactive {
        anyhow::bail!("'{action}' {detail}. Re-run with --yes to proceed.");
    }
    eprint!("'{action}' {detail}. Proceed? [y/N] ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("failed to read confirmation from stdin")?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        Ok(())
    } else {
        anyhow::bail!("aborted: '{action}' not confirmed");
    }
}

/// Reject an empty File Share id client-side.
///
/// The `activity` arm calls the generic `event::poll_activity`, which does NOT
/// guard its entity id — an empty id would build the malformed path
/// `/activity/poll//`. Mirrors the library's `require_id` wording / `Parse`
/// style so the surfaced message is consistent with the rest of the domain.
fn require_fileshare_id(fileshare_id: &str) -> Result<()> {
    if fileshare_id.is_empty() {
        return Err(anyhow::Error::from(CliError::Parse(
            "a File Share id is required for File Share operations".to_owned(),
        )));
    }
    Ok(())
}

// ─── Error mapping ──────────────────────────────────────────────────────────

/// Override `hint:` line for a link-password failure (`1650` on a
/// password-capable read/write). Steers to `--password` / the env var, NOT the
/// generic "run `fastio auth login`" — a LINK password is not an account login.
/// The TEXT lives here (a command-module `const`) so `error.rs` stays
/// resource-agnostic; the render layer fires it via [`CliError::MappedApi`].
const HINT_FS_LINK_PASSWORD: &str = "This File Share link requires a password (or the one supplied is wrong). \
     Pass --password, or set the FASTIO_FILESHARE_PASSWORD environment variable (preferred — keeps the value out of `ps` and shell history).";

/// Override hint for an insufficient-capability failure (`1700`).
const HINT_FS_CAPABILITY: &str = "Your capability on this File Share is insufficient for this action. \
     Capabilities are ordered view < download < edit; writing a new version (`fileshare upload`) needs an explicit `edit` grant.";

/// Override hint for the uniform "unavailable" failure (`1609`/404). SUPPRESSES
/// the generic-404 "Verify the ID …" hint, which would wrongly imply a fixable
/// id typo (and would re-introduce an enumeration oracle). The full uniform
/// wording is in the headline `.context(...)`; no extra hint line is needed.
const HINT_FS_UNAVAILABLE: Option<&str> = None;

/// Override hint for the bound-file-not-serveable failure (`1680`). Replaces the
/// generic-403 "Check that your account has the required role." — the problem is
/// the FILE's serveability, not the caller's role.
const HINT_FS_NOT_SERVEABLE: &str = "The bound file cannot be served — it may be locked, taken down (DMCA), or flagged as infected. \
     This is a property of the file, not your permissions.";

/// Override hint for a preview-specific miss (`143705`, or a bare 404 on the
/// `preview` op). Steers to `--type` / retry — NOT the uniform "share gone"
/// wording, because the SHARE exists; only the requested preview asset does not.
/// The TEXT lives here (a command-module `const`) so `error.rs` stays
/// resource-agnostic; the render layer fires it via [`CliError::MappedApi`].
const HINT_FS_PREVIEW_UNAVAILABLE: &str = "No preview of this type is available for the bound file — it may still be generating, \
     or this file type may not support it. Retry shortly, or try another --type.";

/// Map a File Share API error to an actionable, File-Share-specific message AND
/// hint.
///
/// All File-Share-specific wording lives HERE (and in the MCP mirror) — never in
/// the global `error.rs` hints, which stay resource-agnostic. Two things are
/// reframed per error:
///
/// 1. The HEADLINE — a `.context(...)` carrying the operation label and the
///    resource-specific explanation (keyed on `error.code`, with HTTP status as
///    a secondary signal, so an unrelated error with the same status is not
///    mislabeled).
/// 2. The `hint:` LINE — for the mapped codes the inner [`ApiError`] is wrapped
///    in [`CliError::MappedApi`] so the render layer prints OUR override hint (or
///    NONE) instead of the inner `ApiError`'s generic status hint. Without this,
///    a mapped `1650` still printed "Run `fastio auth login`" and a `1609` still
///    printed "Verify the ID …" — undercutting the careful headline.
///
/// Per-code behavior:
/// - `1650` / HTTP 401 → ON A LINK-ACCESS op (consumption / write-back, where
///   `x-ve-password` applies): a link-password failure → password hint. ON A
///   MANAGEMENT op (create/list/update/delete/grants/activity/ws-token, which
///   authenticate with an ACCOUNT token): a `1650` means invalid/expired ACCOUNT
///   auth, NOT a link password — so it FALLS THROUGH to the generic auth
///   handling (its normal "run `fastio auth login`" hint).
/// - `1700` / HTTP 403 → capability insufficient (`view` < `download` < `edit`).
/// - `1609` / HTTP 404 → UNIFORM "unavailable"; hint SUPPRESSED (never an id
///   typo — distinguishing would leak which ids ever existed).
/// - `1680` / HTTP 403 → the bound file is not serveable (locked / DMCA /
///   infected).
/// - `1605` / HTTP 400 → surface the server message; on create, hint
///   node-must-be-a-file.
/// - A [`CliError::VersionConflict`] passes through UNTOUCHED so its
///   current-version hint survives.
/// - Every other error attaches only the operation label and keeps its generic
///   `suggestion()`.
fn map_fileshare_error(err: CliError, ctx: &'static str, op: FsOp) -> anyhow::Error {
    // A CAS conflict is already fully framed (resource-agnostic Display +
    // current-version hint). Pass it through with only the operation label so
    // the current-version id and re-apply guidance survive intact.
    if matches!(err, CliError::VersionConflict { .. }) {
        return anyhow::Error::from(err).context(ctx);
    }

    // Take ownership of the inner ApiError up front. A non-Api CliError (auth /
    // io / parse …) has no File-Share-specific framing — attach only the
    // operation label and return. Matching by value (not `if let … = &err`)
    // means no `unreachable!()` in this production path.
    let api = match err {
        CliError::Api(api) => api,
        other => return anyhow::Error::from(other).context(ctx),
    };

    // Resolve `(headline note, hint action)` for the codes we reframe. A `None`
    // mapping means "no File-Share framing" → fall through to generic handling.
    let mapped: Option<(String, FsHint)> = match api.code {
        // Link password required or wrong — ONLY meaningful on a link-access op.
        // On a management op (account-token auth) a 1650 is account auth, not a
        // link password, so it must NOT be reframed (falls through to generic
        // auth handling with its login hint).
        1650 if op.is_link_access() => Some((
            format!(
                "{ctx}: this File Share requires a link password (or the one supplied is wrong) \
                 (1650). Pass --password, or set the {PASSWORD_ENV} environment variable \
                 (preferred — keeps the value out of `ps` and shell history)."
            ),
            FsHint::Override(Some(HINT_FS_LINK_PASSWORD)),
        )),
        // Capability insufficient for the action.
        1700 => Some((
            format!(
                "{ctx}: your capability on this File Share is insufficient for this action (1700). \
                 Capabilities are ordered view < download < edit; writing a new version \
                 (`fileshare upload`) needs an explicit `edit` grant."
            ),
            FsHint::Override(Some(HINT_FS_CAPABILITY)),
        )),
        // Preview-specific miss (143705 / "Unable to retrieve preview"). This
        // code is emitted ONLY by the storage preview-read path's default arm
        // (server `storage/Io.php`), so keying on the code alone is op-independent
        // and safe — the SHARE exists; only the requested preview asset does not
        // (still generating, or the file type does not support it). Distinct from
        // the uniform-unavailable 1609 below, which means the share itself is gone.
        143_705 => Some((
            format!(
                "{ctx}: no preview of this type is available for the bound file (143705) — it may \
                 still be generating, or this file type may not support it. Retry shortly, or try \
                 another --type."
            ),
            FsHint::Override(Some(HINT_FS_PREVIEW_UNAVAILABLE)),
        )),
        // Uniform unavailable — NEVER distinguish not-found / expired / revoked.
        // The generic-404 "Verify the ID …" hint is SUPPRESSED (would imply a
        // fixable id typo and re-introduce an enumeration oracle).
        1609 => Some((
            format!(
                "{ctx}: this File Share is unavailable (1609) — it may not exist, may have \
                 expired, or may have been revoked."
            ),
            FsHint::Override(HINT_FS_UNAVAILABLE),
        )),
        // Bound file is not serveable.
        1680 => Some((
            format!(
                "{ctx}: the bound file cannot be served (1680) — it may be locked, taken down \
                 (DMCA), or flagged as infected."
            ),
            FsHint::Override(Some(HINT_FS_NOT_SERVEABLE)),
        )),
        // Invalid input — surface the server message; hint node-must-be-a-file on
        // create. The generic-400 path has NO misleading hint to override, so we
        // keep the generic handling (no MappedApi wrap): only the headline is
        // reframed.
        1605 => Some((
            match op {
                FsOp::Create => format!(
                    "{ctx}: invalid request (1605): {}. The --node must be a FILE node (not a \
                     folder or note).",
                    api.message
                ),
                FsOp::ManagementOther | FsOp::LinkAccess | FsOp::Preview => {
                    format!("{ctx}: invalid request (1605): {}", api.message)
                }
            },
            FsHint::KeepGeneric,
        )),
        _ => None,
    };

    // Bare-status fallback for the password / unavailable cases when the server
    // returns the status without the specific code (keyed only after the code
    // match misses, so a coded error always wins). The 401 fallback, like 1650,
    // is link-access-only.
    let mapped = mapped.or_else(|| match api.http_status {
        401 if op.is_link_access() => Some((
            format!(
                "{ctx}: this File Share requires a link password (or the one supplied is wrong). \
                 Pass --password, or set the {PASSWORD_ENV} environment variable (preferred)."
            ),
            FsHint::Override(Some(HINT_FS_LINK_PASSWORD)),
        )),
        // A bare 404 on the PREVIEW op (no 1609, no 143705) is a preview miss, not
        // a share-gone — the consumption call reached the share but the requested
        // preview asset does not exist. Use the preview-specific wording. A bare
        // 404 on any NON-preview op keeps the uniform-unavailable discipline below
        // (that genuinely means the share is gone — do NOT weaken it).
        404 if op.is_preview() => Some((
            format!(
                "{ctx}: no preview of this type is available for the bound file — it may still be \
                 generating, or this file type may not support it. Retry shortly, or try another \
                 --type."
            ),
            FsHint::Override(Some(HINT_FS_PREVIEW_UNAVAILABLE)),
        )),
        404 => Some((
            format!(
                "{ctx}: this File Share is unavailable — it may not exist, may have expired, or \
                 may have been revoked."
            ),
            FsHint::Override(HINT_FS_UNAVAILABLE),
        )),
        _ => None,
    });

    match mapped {
        // Reframe the hint: wrap in MappedApi so the render layer prints OUR hint
        // (or none), not the inner ApiError's generic status default — while
        // preserving the inner Display (and exit code) and adding no duplicate
        // chain link (plain field, like ArtifactNotReady).
        Some((note, FsHint::Override(hint))) => {
            anyhow::Error::from(CliError::MappedApi { api, hint }).context(note)
        }
        // Reframe only the headline; keep the inner ApiError so its generic
        // suggestion (if any) still fires.
        Some((note, FsHint::KeepGeneric)) => anyhow::Error::from(CliError::Api(api)).context(note),
        // No File-Share framing (e.g. a management 1650/401, or an unmapped
        // code): keep the plain ApiError so its normal generic suggestion fires.
        None => anyhow::Error::from(CliError::Api(api)).context(ctx),
    }
}

/// What `map_fileshare_error` should do with the rendered `hint:` line for a
/// reframed error.
#[derive(Clone, Copy)]
enum FsHint {
    /// Replace the inner `ApiError`'s generic status hint with this override
    /// (`Some(text)`) or SUPPRESS it entirely (`None`). Implemented by wrapping
    /// the error in [`CliError::MappedApi`].
    Override(Option<&'static str>),
    /// Keep the inner `ApiError`'s generic `suggestion()` — only the headline is
    /// reframed (used for `1605`/400, whose generic hint is already absent and
    /// non-misleading).
    KeepGeneric,
}

/// Discriminates a File Share call by (a) whether a `1605` should hint
/// node-must-be-a-file (only `create`) and (b) its AUTH CLASS — whether the call
/// authenticates with the share's LINK gate (consumption + write-back, where
/// `x-ve-password` applies and a `1650`/401 means a link-password failure) or
/// with the caller's ACCOUNT token (management, where a `1650`/401 means
/// invalid/expired account auth, NOT a link password).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FsOp {
    /// A `create` call (a management op) — a `1605` invalid-input hints
    /// node-must-be-a-file.
    Create,
    /// A management call other than `create` (list / update / delete / grants /
    /// activity / ws-token). Authenticates with the ACCOUNT token, so a
    /// `1650`/401 is an account-auth failure, not a link password.
    ManagementOther,
    /// A link-access call (consumption: info / download / versions; and
    /// write-back). `x-ve-password` applies, so a `1650`/401 is a link-password
    /// failure.
    LinkAccess,
    /// The `preview` consumption call. Like [`Self::LinkAccess`] for auth
    /// purposes (`x-ve-password` applies, so a `1650`/401 is a link-password
    /// failure), but a `404` WITHOUT a share-gone code (`1609`) — or the
    /// preview-specific `143705` — is a PREVIEW miss (the share exists; the
    /// requested preview asset does not), NOT a share-gone, so it gets the
    /// preview-specific wording instead of the uniform "unavailable".
    Preview,
}

impl FsOp {
    /// Whether this op authenticates against the share's LINK gate
    /// (`x-ve-password`), so a `1650`/401 means a link-password failure rather
    /// than an account-auth failure. Both consumption classes (`LinkAccess` and
    /// `Preview`) gate on the link.
    fn is_link_access(self) -> bool {
        matches!(self, Self::LinkAccess | Self::Preview)
    }

    /// Whether this is the `preview` op, so a bare `404` is a preview miss rather
    /// than a share-gone (the SHARE exists; the requested preview asset does not).
    fn is_preview(self) -> bool {
        matches!(self, Self::Preview)
    }
}

// ─── Dispatch ───────────────────────────────────────────────────────────────

/// Execute a `fastio fileshare` command.
#[allow(clippy::too_many_lines)] // a flat dispatch over the File Share surface
pub async fn execute(command: FileshareCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        FileshareCommands::Create {
            workspace,
            node,
            title,
            access_option,
            password,
            expires,
            expires_at,
        } => {
            let params = fileshare::CreateFileShareParams::new()
                .node(Some(node))
                .title(title)
                .access_option(access_option)
                .password(resolve_password(password.as_deref()))
                .expires(expires)
                .expires_at(expires_at);
            // Let the Wave-1 validator surface XOR/empty/null/range errors
            // cleanly before the network.
            params
                .validate()
                .map_err(|e| anyhow::Error::from(e).context("invalid create request"))?;
            let client = ctx.build_client()?;
            let v = fileshare::create_fileshare(&client, &workspace, &params)
                .await
                .map_err(|e| map_fileshare_error(e, "failed to create File Share", FsOp::Create))?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareCommands::List {
            workspace,
            offset,
            limit,
        } => {
            let client = ctx.build_client()?;
            let v = fileshare::list_fileshares(&client, &workspace, offset, limit)
                .await
                .map_err(|e| {
                    map_fileshare_error(e, "failed to list File Shares", FsOp::ManagementOther)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareCommands::Info {
            fileshare_id,
            password,
        } => {
            let password = resolve_consumption_password(password.as_deref())?;
            let client = ctx.build_client_allow_anonymous()?;
            let v = fileshare::get_details(&client, &fileshare_id, password.as_ref())
                .await
                .map_err(|e| {
                    map_fileshare_error(e, "failed to get File Share details", FsOp::LinkAccess)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareCommands::Update {
            fileshare_id,
            title,
            access_option,
            password,
            clear_password,
            expires,
            expires_at,
            clear_expires,
        } => {
            let params = fileshare::UpdateFileShareParams::new()
                .title(title)
                .access_option(access_option)
                .password(resolve_update_password(password.as_deref(), clear_password))
                .clear_password(clear_password)
                .expires(expires)
                .expires_at(expires_at)
                .clear_expires(clear_expires);
            // Reject a no-op before the network for a clear error.
            anyhow::ensure!(
                !params.is_empty(),
                "nothing to update: supply at least one of --title, --access-option, --password, \
                 --clear-password, --expires, --expires-at, or --clear-expires"
            );
            params
                .validate()
                .map_err(|e| anyhow::Error::from(e).context("invalid update request"))?;
            let client = ctx.build_client()?;
            let v = fileshare::update_fileshare(&client, &fileshare_id, &params)
                .await
                .map_err(|e| {
                    map_fileshare_error(e, "failed to update File Share", FsOp::ManagementOther)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareCommands::Delete { fileshare_id, yes } => {
            confirm_destructive(
                "fileshare delete",
                "permanently revokes the link and cascades its grants (the bound file is not \
                 touched)",
                yes,
            )?;
            let client = ctx.build_client()?;
            let v = fileshare::delete_fileshare(&client, &fileshare_id)
                .await
                .map_err(|e| {
                    map_fileshare_error(e, "failed to delete File Share", FsOp::ManagementOther)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareCommands::Grants(c) => execute_grants(c, ctx).await,
        FileshareCommands::Download {
            fileshare_id,
            output,
            version,
            password,
        } => {
            download(
                ctx,
                &fileshare_id,
                output.as_deref(),
                version.as_deref(),
                password.as_deref(),
            )
            .await
        }
        FileshareCommands::Versions {
            fileshare_id,
            password,
        } => {
            let password = resolve_consumption_password(password.as_deref())?;
            let client = ctx.build_client_allow_anonymous()?;
            let v = fileshare::list_versions(&client, &fileshare_id, password.as_ref())
                .await
                .map_err(|e| {
                    map_fileshare_error(e, "failed to list File Share versions", FsOp::LinkAccess)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareCommands::Preview {
            fileshare_id,
            preview_type,
            output,
            password,
        } => {
            preview(
                ctx,
                &fileshare_id,
                &preview_type,
                output.as_deref(),
                password.as_deref(),
            )
            .await
        }
        FileshareCommands::Upload {
            fileshare_id,
            file,
            if_version,
            password,
            name,
            yes,
        } => {
            upload_writeback(
                ctx,
                &fileshare_id,
                &file,
                if_version.as_deref(),
                password.as_deref(),
                name.as_deref(),
                yes,
            )
            .await
        }
        FileshareCommands::Activity {
            fileshare_id,
            lastactivity,
            wait,
            updated,
        } => {
            // `event::poll_activity` is a generic helper that does NOT guard the
            // entity id; an empty fileshare_id would build `/activity/poll//`.
            // Reject it client-side (mirrors the library's `require_id` wording).
            require_fileshare_id(&fileshare_id)?;
            // Activity is members-only per spec → always-authed client.
            let client = ctx.build_client()?;
            let v = event::poll_activity(
                &client,
                &fileshare_id,
                lastactivity.as_deref(),
                wait,
                updated,
            )
            .await
            .map_err(|e| {
                map_fileshare_error(
                    e,
                    "failed to poll File Share activity",
                    FsOp::ManagementOther,
                )
            })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareCommands::WsToken {
            fileshare_id,
            token_file,
        } => ws_token(ctx, &fileshare_id, token_file.as_deref()).await,
    }
}

// ─── Grants ─────────────────────────────────────────────────────────────────

async fn execute_grants(command: FileshareGrantsCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        FileshareGrantsCommands::List { fileshare_id } => {
            let client = ctx.build_client()?;
            let v = fileshare::list_grants(&client, &fileshare_id)
                .await
                .map_err(|e| {
                    map_fileshare_error(
                        e,
                        "failed to list File Share grants",
                        FsOp::ManagementOther,
                    )
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareGrantsCommands::Add {
            fileshare_id,
            user,
            email,
            capability,
        } => {
            let params = fileshare::GrantParams::new()
                .user(user)
                .email(email)
                .capability(Some(capability));
            params
                .validate_add()
                .map_err(|e| anyhow::Error::from(e).context("invalid grant request"))?;
            let client = ctx.build_client()?;
            let v = fileshare::add_grant(&client, &fileshare_id, &params)
                .await
                .map_err(|e| {
                    map_fileshare_error(e, "failed to add File Share grant", FsOp::ManagementOther)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        FileshareGrantsCommands::Remove {
            fileshare_id,
            user,
            email,
            yes,
        } => {
            let params = fileshare::GrantParams::new().user(user).email(email);
            // Validate the user/email XOR BEFORE prompting, so a bad target
            // fails immediately rather than after the y/N confirmation.
            params
                .validate_remove()
                .map_err(|e| anyhow::Error::from(e).context("invalid grant request"))?;
            confirm_destructive(
                "fileshare grants remove",
                "revokes this user's access to the File Share",
                yes,
            )?;
            let client = ctx.build_client()?;
            let v = fileshare::remove_grant(&client, &fileshare_id, &params)
                .await
                .map_err(|e| {
                    map_fileshare_error(
                        e,
                        "failed to remove File Share grant",
                        FsOp::ManagementOther,
                    )
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
    }
}

// ─── Download ───────────────────────────────────────────────────────────────

/// Stream the bound file (or a historical version) to disk.
///
/// `--version` set → the version-read path; else the storage-read path (both
/// Wave-1 builders). Output filename: `--output` else the bound file's name
/// from `details` (via [`fileshare::fileshare_file_name`] →
/// [`fastio_cli::api::download::sanitize_filename`]) else the literal
/// `"download"`. Anonymous-capable.
async fn download(
    ctx: &CommandContext<'_>,
    fileshare_id: &str,
    output: Option<&str>,
    version: Option<&str>,
    password: Option<&str>,
) -> Result<()> {
    let password = resolve_consumption_password(password)?;
    let client = ctx.build_client_allow_anonymous()?;

    let output_path =
        determine_download_path(&client, fileshare_id, output, password.as_ref()).await?;

    let api_path = match version {
        Some(v) => fileshare::storage_version_read_path(fileshare_id, v)
            .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?,
        None => fileshare::storage_read_path(fileshare_id)
            .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?,
    };

    if !ctx.output.quiet {
        eprintln!("Downloading to: {}", output_path.display());
    }

    let bytes = client
        .download_file_stream_with_password(&api_path, &output_path, password.as_ref())
        .await
        .map_err(|e| map_fileshare_error(e, "failed to download File Share", FsOp::LinkAccess))?;

    let value = json!({
        "status": "downloaded",
        "fileshare": fileshare_id,
        "output": output_path.display().to_string(),
        "size": bytes,
    });
    ctx.output.render(&value)?;
    Ok(())
}

/// Resolve the download output path: an explicit `--output` wins; otherwise the
/// bound file's name from `details` (sanitized), falling back to `"download"`.
async fn determine_download_path(
    client: &fastio_cli::client::ApiClient,
    fileshare_id: &str,
    user_output: Option<&str>,
    password: Option<&SecretString>,
) -> Result<PathBuf> {
    if let Some(p) = user_output {
        return Ok(PathBuf::from(p));
    }
    // Best-effort details fetch for the bound file name. A failure here is not
    // fatal — fall back to the default name (the real download error, if any,
    // surfaces on the stream call with the proper mapping). P2F-9: trace-log the
    // swallowed error at debug so a `"download"` fallback filename is
    // diagnosable. The `CliError`/`ApiError` Display carries only server
    // diagnostics, never the link password (it travels only in the request
    // header), so logging it leaks no secret.
    let details = match fileshare::get_details(client, fileshare_id, password).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!(
                error = %e,
                "fileshare download: best-effort details fetch for the bound file name failed; \
                 falling back to the default output filename"
            );
            None
        }
    };
    let name = details
        .as_ref()
        .and_then(fileshare::fileshare_file_name)
        .map_or_else(
            || "download".to_owned(),
            |n| fastio_cli::api::download::sanitize_filename(&n),
        );
    Ok(PathBuf::from(name))
}

// ─── Preview ────────────────────────────────────────────────────────────────

/// Download the PRIMARY preview asset for the bound file (after at most one
/// manual, leak-safe redirect). Multi-file previews yield the primary asset
/// only — sub-assets are not fetched. Anonymous-capable.
async fn preview(
    ctx: &CommandContext<'_>,
    fileshare_id: &str,
    preview_type: &str,
    output: Option<&str>,
    password: Option<&str>,
) -> Result<()> {
    let password = resolve_consumption_password(password)?;
    let client = ctx.build_client_allow_anonymous()?;

    let api_path = fileshare::storage_preview_path(fileshare_id, preview_type)
        .map_err(|e| anyhow::Error::from(e).context("invalid preview request"))?;

    // Output name: --output else "<id>.<preview_type>" (a preview is a derived
    // asset, so the bound file name is not the right default).
    let output_path = match output {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(fastio_cli::api::download::sanitize_filename(&format!(
            "{fileshare_id}.{preview_type}"
        ))),
    };

    if !ctx.output.quiet {
        eprintln!("Downloading preview to: {}", output_path.display());
    }

    let bytes = client
        .download_preview_following_redirect(&api_path, &output_path, password.as_ref())
        .await
        .map_err(|e| {
            map_fileshare_error(e, "failed to download File Share preview", FsOp::Preview)
        })?;

    let value = json!({
        "status": "downloaded",
        "fileshare": fileshare_id,
        "preview_type": preview_type,
        "output": output_path.display().to_string(),
        "size": bytes,
    });
    ctx.output.render(&value)?;
    Ok(())
}

// ─── Write-back upload ──────────────────────────────────────────────────────

/// Validate that a write-back source path is an existing REGULAR file.
///
/// A write-back replaces the bound file's bytes with a single local file, so the
/// source must be a regular file: a missing path, a directory, or another
/// non-regular node (FIFO / socket / device) is rejected here — BEFORE the
/// destructive confirm gate — with a friendly message, rather than surfacing a
/// confusing metadata/read error deeper in the flow. `display` is the original
/// user-supplied argument, echoed back verbatim in the error.
fn validate_writeback_source(path: &Path, display: &str) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("file not found: {display}");
    }
    if !path.is_file() {
        anyhow::bail!(
            "'{display}' is not a regular file (it may be a directory) — a write-back replaces \
             the bound file's content with a single local file."
        );
    }
    Ok(())
}

/// Resolve the write-back `name` (explicit `--name` or the local file's base
/// name) and validate it against the SAME rules the normal upload path applies
/// (`upload::validate_filename`: empty, path separators, CR/LF, NUL, controls,
/// bidi/zero-width, `.`/`..`, trailing whitespace/dot, length).
///
/// The resolved name flows into the write-back `name` field and the multipart
/// `file_name`, so an invalid `--name ""` or a CR/LF name must be rejected
/// HERE — before the destructive confirm gate and any network call — exactly as
/// `validate_writeback_source` guards the source path. Factored into a pure
/// helper so the rejection is unit-testable.
fn resolve_writeback_name(path: &Path, file: &str, name: Option<&str>) -> Result<String> {
    let upload_name = match name {
        Some(n) => n.to_owned(),
        None => path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid filename for '{file}'"))?
            .to_owned(),
    };
    upload::validate_filename(&upload_name)
        .map_err(|e| anyhow::anyhow!("invalid upload name: {e}"))?;
    Ok(upload_name)
}

/// Replace the bound file's content with a local file (write-back). Requires an
/// `edit` grant (always-authed). Resolves the bound node id from `details`
/// (`fileshare.file.id`), then takes the single-shot path for files ≤ 4 MB or
/// the chunked path (session → chunks → complete → poll) for larger ones.
/// `--if-version` is a server-enforced precondition: when the server detects a
/// version conflict the session ends terminally and the CLI surfaces a
/// [`CliError::VersionConflict`] with the current version id.
async fn upload_writeback(
    ctx: &CommandContext<'_>,
    fileshare_id: &str,
    file: &str,
    if_version: Option<&str>,
    password: Option<&str>,
    name: Option<&str>,
    yes: bool,
) -> Result<()> {
    let path = Path::new(file);
    // P2F-6: validate the path is an existing REGULAR file BEFORE the confirm
    // gate (a directory / FIFO / device cannot be a write-back source). Factored
    // into a pure helper so the rejection is unit-testable.
    validate_writeback_source(path, file)?;
    let metadata = std::fs::metadata(path).context("failed to read file metadata")?;
    let file_size = metadata.len();
    // Resolve AND validate the write-back name BEFORE the confirm gate and any
    // network call. Factored into a pure helper so the rejection is
    // unit-testable (mirrors the P2F-6 `validate_writeback_source` pattern).
    let upload_name = resolve_writeback_name(path, file, name)?;

    confirm_destructive(
        "fileshare upload",
        "writes a NEW VERSION of the File Share's bound file (the previous version is retained \
         in history)",
        yes,
    )?;

    let password = resolve_consumption_password(password)?;

    // Resolve the bound node id (fileshare.file.id) via details. The write-back
    // requires it as `file_id`; supplying any other node 404s.
    let client = ctx.build_client()?;
    let details = fileshare::get_details(&client, fileshare_id, password.as_ref())
        .await
        .map_err(|e| {
            map_fileshare_error(
                e,
                "failed to resolve the File Share's bound file",
                FsOp::LinkAccess,
            )
        })?;
    let bound_node_id = fileshare::extract_fileshare(&details)
        .and_then(|fs| fs.get("file"))
        .and_then(|f| f.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the File Share details did not include the bound file id"))?
        .to_owned();

    if !ctx.output.quiet {
        eprintln!("Uploading new version of '{upload_name}' ({file_size} bytes)");
    }

    let session = if file_size <= upload::SINGLE_CALL_MAX_SIZE {
        single_shot_writeback(
            ctx,
            fileshare_id,
            &bound_node_id,
            &upload_name,
            path,
            if_version,
            password.as_ref(),
        )
        .await?
    } else {
        chunked_writeback(
            ctx,
            &client,
            fileshare_id,
            &bound_node_id,
            &upload_name,
            path,
            file_size,
            if_version,
            password.as_ref(),
        )
        .await?
    };

    ctx.output.render(&session)?;
    Ok(())
}

/// Single-shot write-back (≤ 4 MB): the full body rides the initial multipart
/// request and the server auto-assembles — `complete` is NOT called. A CAS miss
/// surfaces here as `assembly_failed` in the returned session, mapped to a
/// [`CliError::VersionConflict`] by [`check_writeback_session`].
async fn single_shot_writeback(
    ctx: &CommandContext<'_>,
    fileshare_id: &str,
    bound_node_id: &str,
    upload_name: &str,
    path: &Path,
    if_version: Option<&str>,
    password: Option<&SecretString>,
) -> Result<Value> {
    let token = resolve_token(ctx)?;
    let data =
        std::fs::read(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    let resp = upload::single_shot_fileshare_writeback(
        &token,
        ctx.api_base,
        fileshare_id,
        bound_node_id,
        upload_name,
        data,
        if_version,
        password,
    )
    .await
    .map_err(|e| {
        map_fileshare_error(
            e,
            "failed to write back the File Share file",
            FsOp::LinkAccess,
        )
    })?;
    check_writeback_session(&resp)?;
    Ok(resp)
}

/// Chunked write-back (> 4 MB): create the session, upload chunks, complete,
/// then poll until a terminal status. A CAS miss appears as `assembly_failed`
/// on the polled session → [`CliError::VersionConflict`].
#[allow(clippy::too_many_arguments)] // the write-back contract needs every field
async fn chunked_writeback(
    ctx: &CommandContext<'_>,
    client: &fastio_cli::client::ApiClient,
    fileshare_id: &str,
    bound_node_id: &str,
    upload_name: &str,
    path: &Path,
    file_size: u64,
    if_version: Option<&str>,
    password: Option<&SecretString>,
) -> Result<Value> {
    let token = resolve_token(ctx)?;

    let session = upload::create_fileshare_writeback_session(
        client,
        fileshare_id,
        bound_node_id,
        upload_name,
        file_size,
        if_version,
        password,
    )
    .await
    .map_err(|e| {
        map_fileshare_error(
            e,
            "failed to create the write-back session",
            FsOp::LinkAccess,
        )
    })?;
    let upload_id = fileshare::extract_session(&session)
        .and_then(|s| s.get("id"))
        .or_else(|| session.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the write-back session did not return an id"))?
        .to_owned();

    // Upload chunks sequentially.
    let mut file_handle = std::fs::File::open(path)
        .with_context(|| format!("failed to open '{}'", path.display()))?;
    send_writeback_chunks(&mut file_handle, &token, ctx.api_base, &upload_id, password).await?;

    upload::complete_upload_with_password(client, &upload_id, password)
        .await
        .map_err(|e| {
            map_fileshare_error(e, "failed to complete the write-back", FsOp::LinkAccess)
        })?;

    poll_writeback_completion(client, &upload_id, password).await
}

/// Map a zero-based chunk index to its 1-BASED upload `order` value.
///
/// The upload contract (upload.txt:35,246) makes chunk ordering 1-based: the
/// FIRST chunk is `order=1`. Centralizing the conversion here (rather than an
/// inline `+ 1`) lets a unit test pin the first chunk's order without driving a
/// network upload. `checked_add` guards the (practically unreachable) overflow.
fn writeback_chunk_order(zero_based_index: u32) -> Result<u32> {
    zero_based_index
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("too many write-back chunks"))
}

/// Read the local file in `WRITEBACK_CHUNK_SIZE` pieces and upload each via the
/// password-capable chunk path.
async fn send_writeback_chunks(
    file_handle: &mut std::fs::File,
    token: &str,
    api_base: &str,
    upload_id: &str,
    password: Option<&SecretString>,
) -> Result<()> {
    use std::io::Read;
    // The chunk `order` is 1-BASED per the upload contract (upload.txt:35,246 —
    // "first chunk is `order=1`"), matching the canonical upload command's
    // chunk loop. We keep a zero-based read index and convert via
    // `writeback_chunk_order`, so the first chunk goes out as `order=1`; a
    // 0-based first chunk would be rejected server-side ("No `order` supplied" /
    // invalid order).
    let mut chunk_index: u32 = 0;
    loop {
        let mut buf = vec![0u8; WRITEBACK_CHUNK_SIZE];
        let mut filled = 0usize;
        // Fill a full chunk (read may return short).
        while filled < WRITEBACK_CHUNK_SIZE {
            let n = file_handle
                .read(&mut buf[filled..])
                .context("failed to read file chunk")?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        buf.truncate(filled);
        let order = writeback_chunk_order(chunk_index)?;
        upload::upload_chunk_with_password(token, api_base, upload_id, order, buf, password)
            .await
            .map_err(|e| {
                map_fileshare_error(e, "failed to upload a write-back chunk", FsOp::LinkAccess)
            })?;
        chunk_index = chunk_index
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("too many write-back chunks"))?;
        if filled < WRITEBACK_CHUNK_SIZE {
            break;
        }
    }
    Ok(())
}

/// Poll a chunked write-back session to a terminal status, returning the final
/// status envelope. `complete`/`stored` → success; `assembly_failed` →
/// [`check_writeback_session`] (CAS conflict or surfaced message); a bounded
/// attempt count guards against a never-terminating poll.
async fn poll_writeback_completion(
    client: &fastio_cli::client::ApiClient,
    upload_id: &str,
    password: Option<&SecretString>,
) -> Result<Value> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let status = upload::get_upload_status_with_password(client, upload_id, password)
            .await
            .map_err(|e| {
                map_fileshare_error(e, "failed to poll the write-back status", FsOp::LinkAccess)
            })?;
        let status_str = writeback_status(&status);
        match status_str {
            "complete" | "stored" => return Ok(status),
            "assembly_failed" | "store_failed" => {
                check_writeback_session(&status)?;
                // check_writeback_session only returns Ok when it is NOT a
                // conflict and there is no message; bail with the status.
                anyhow::bail!("write-back failed (status: {status_str})");
            }
            _ => {
                if attempts >= WRITEBACK_POLL_ATTEMPTS {
                    anyhow::bail!(
                        "write-back timed out after {WRITEBACK_POLL_ATTEMPTS} attempts \
                         (status: {status_str})"
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

/// Read the `session.status` (or top-level `status`) from a write-back envelope.
fn writeback_status(value: &Value) -> &str {
    fileshare::extract_session(value)
        .and_then(|s| s.get("status"))
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Inspect a write-back session envelope for a terminal failure. On
/// `assembly_failed` with a `CONFLICT_VERSION_MISMATCH:{vid}` status message,
/// returns [`CliError::VersionConflict`] carrying the current version id; on
/// `assembly_failed` with any other message, surfaces that message; otherwise
/// `Ok(())`.
fn check_writeback_session(value: &Value) -> Result<()> {
    let session = fileshare::extract_session(value).or(Some(value));
    let status = session
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if status != "assembly_failed" && status != "store_failed" {
        return Ok(());
    }
    let message = session
        .and_then(|s| s.get("status_message"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(vid) = fileshare::parse_conflict_version(message) {
        return Err(anyhow::Error::from(CliError::VersionConflict {
            current_version: vid.to_owned(),
        })
        .context("the File Share's bound file changed since the version you supplied"));
    }
    anyhow::bail!("write-back failed ({status}): {message}");
}

/// Resolve the raw bearer token for the multipart write-back paths (which need
/// a token string + api base, not an `ApiClient`).
fn resolve_token(ctx: &CommandContext<'_>) -> Result<String> {
    let resolved =
        fastio_cli::auth::token::resolve_token(ctx.flag_token, ctx.profile_name, ctx.config_dir)
            .context("failed to resolve token")?;
    resolved.ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))
}

// ─── WebSocket token ────────────────────────────────────────────────────────

/// Mint a realtime-channel WebSocket token. The token is REDACTED from stdout
/// and written 0600 to `--token-file`, via the shared
/// [`super::secret_output`] helpers.
async fn ws_token(
    ctx: &CommandContext<'_>,
    fileshare_id: &str,
    token_file: Option<&Path>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let mut v = fileshare::websocket_auth(&client, fileshare_id)
        .await
        .map_err(|e| {
            map_fileshare_error(
                e,
                "failed to mint File Share WebSocket token",
                FsOp::ManagementOther,
            )
        })?;
    if let Some(token) = extract_secret(&v, "token").or_else(|| extract_secret(&v, "auth_token")) {
        if let Some(path) = token_file {
            write_secret_file(path, &token, "WebSocket token", ctx.output.quiet)?;
        } else if !ctx.output.quiet {
            eprintln!(
                "WARNING: the WebSocket token is REDACTED from stdout to avoid leaking it into \
                 logs. Re-run with --token-file <path> to capture it (written 0600)."
            );
        }
        redact_secret_field(&mut v, "token", "--token-file");
        redact_secret_field(&mut v, "auth_token", "--token-file");
    }
    ctx.output.render(&v)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastio_cli::error::ApiError;
    use serde_json::json;

    fn api_err(code: u32, http_status: u16) -> CliError {
        CliError::Api(ApiError::new(code, None, "boom".to_owned(), http_status))
    }

    /// Drive a mapped error through the REAL render path (`cli_error_render`),
    /// returning the `(headline, hint)` a user would actually see on stderr.
    ///
    /// This is the load-bearing assertion surface for P2F-5: a `to_string()`
    /// check only inspects the headline, but the regression was the inner
    /// `ApiError`'s GENERIC hint leaking onto the `hint:` line. We must downcast
    /// to the rooted `CliError` exactly as `main()` does, then call the same
    /// helper.
    fn render_mapped(err: &anyhow::Error) -> (String, Option<&'static str>) {
        let cli_err = err
            .downcast_ref::<CliError>()
            .expect("a mapped File Share error must be rooted at a CliError");
        crate::cli_error_render(err, cli_err)
    }

    // ─── resolve_password ───────────────────────────────────────────────────

    /// Precedence + empty-handling for the link password, in ONE test.
    ///
    /// The `FASTIO_FILESHARE_PASSWORD` env var is process-global, so splitting
    /// this across separate `#[test]` fns would race (cargo runs tests in the
    /// same process in parallel by default — one test's `set_var` clobbers
    /// another's read). Keeping every env mutation in a single sequential test
    /// removes the race without needing `--test-threads=1`.
    ///
    /// P2F-3: flag PRESENCE is preserved. An explicitly supplied empty flag
    /// (`--password ""`) flows through as `Some("")` so the library validator
    /// rejects it — it must NEVER be downgraded to "absent" (which would let it
    /// silently fall back to the env value or create an unprotected share).
    #[test]
    fn resolve_password_precedence_and_empty_handling() {
        // SAFETY: single test fn, mutations are sequential within it.
        // 1. Flag wins over env.
        unsafe { std::env::set_var(PASSWORD_ENV, "env-pw") };
        let pw = resolve_password(Some("flag-pw")).expect("flag present");
        assert_eq!(pw.expose_secret(), "flag-pw", "flag must win over env");

        // 2. Env fallback when the flag is absent.
        let pw = resolve_password(None).expect("env present");
        assert_eq!(pw.expose_secret(), "env-pw", "env used when flag absent");

        // 3. P2F-3: an explicitly supplied EMPTY flag flows through as Some("")
        //    EVEN WITH the env var set — it must NOT silently fall back to the
        //    env value. The library validator then rejects the empty value.
        let pw = resolve_password(Some("")).expect("empty flag must be PRESENT, not absent");
        assert_eq!(
            pw.expose_secret(),
            "",
            "an explicit --password \"\" must reach the validator as Some(\"\"), not fall back to env"
        );

        // 4. Neither set → None.
        unsafe { std::env::remove_var(PASSWORD_ENV) };
        assert!(resolve_password(None).is_none(), "no flag, no env → None");

        // 5. P2F-3 again with no env: an empty flag is still Some(""), not None.
        let pw = resolve_password(Some("")).expect("empty flag is present even with no env");
        assert_eq!(pw.expose_secret(), "", "empty flag → Some(\"\")");

        // 6. An empty env var is treated as absent (the env path still filters
        //    empties — only a PRESENT flag preserves an empty value).
        unsafe { std::env::set_var(PASSWORD_ENV, "") };
        assert!(resolve_password(None).is_none(), "empty env → None");
        unsafe { std::env::remove_var(PASSWORD_ENV) };

        // 7. P2F-4: `--clear-password` suppresses env-password resolution. This
        //    lives in THIS test (not a separate one) because it mutates the same
        //    process-global env var — a separate parallel test would race.
        unsafe { std::env::set_var(PASSWORD_ENV, "env-pw") };
        assert!(
            resolve_update_password(None, true).is_none(),
            "--clear-password must suppress the env password so the clear is valid"
        );
        // Not clearing → normal precedence (env used when the flag is absent).
        let pw = resolve_update_password(None, false).expect("env used when not clearing");
        assert_eq!(pw.expose_secret(), "env-pw");
        // Full library path: a valid clear must pass validate() WITH the env set
        // (no longer resolves a phantom env password into the update).
        let params = fileshare::UpdateFileShareParams::new()
            .password(resolve_update_password(None, true))
            .clear_password(true);
        assert!(
            params.validate().is_ok(),
            "a --clear-password update must validate even with the env var set"
        );
        // Sanity: the OLD behavior (resolving env into a clear) would have failed
        // validation — confirm that combination is indeed rejected, so the fix is
        // load-bearing.
        let bad = fileshare::UpdateFileShareParams::new()
            .password(resolve_password(None)) // resolves env → Some("env-pw")
            .clear_password(true);
        assert!(
            bad.validate().is_err(),
            "password=Some(env)+clear must be rejected (the bug P2F-4 fixes)"
        );
        unsafe { std::env::remove_var(PASSWORD_ENV) };

        // 8. FR-2: resolve_consumption_password env interplay. Kept HERE (not a
        //    separate test) to share this test's sequential PASSWORD_ENV
        //    serialization — a parallel env-mutating test would race.
        //    8a. No flag + no env → Ok(None) (a valid unprotected-share read).
        assert!(
            resolve_consumption_password(None)
                .expect("no flag + no env is not an error")
                .is_none(),
            "consumption: no flag + no env → Ok(None)"
        );
        //    8b. An empty env is treated as ABSENT upstream → Ok(None), NOT an
        //        empty-password error.
        unsafe { std::env::set_var(PASSWORD_ENV, "") };
        assert!(
            resolve_consumption_password(None)
                .expect("an empty env is absent, not an empty password")
                .is_none(),
            "consumption: empty env → Ok(None)"
        );
        //    8c. A non-empty env resolves to Ok(Some(env)).
        unsafe { std::env::set_var(PASSWORD_ENV, "env-pw") };
        let pw = resolve_consumption_password(None)
            .expect("a non-empty env is accepted")
            .expect("a non-empty env resolves to Some");
        assert_eq!(
            pw.expose_secret(),
            "env-pw",
            "consumption: non-empty env → Some"
        );
        unsafe { std::env::remove_var(PASSWORD_ENV) };
    }

    // ─── resolve_consumption_password (FR-2) ────────────────────────────────

    /// FR-2: on consumption / write-back paths an EMPTY link password must be
    /// rejected (the library validator never runs there, so an empty
    /// `x-ve-password` header would otherwise be sent). A present non-empty flag
    /// is accepted; a present empty flag is an error. The flag PRESENT cases read
    /// no env var, so this test is race-free against the env-mutating sequential
    /// test above.
    #[test]
    fn resolve_consumption_password_flag_cases() {
        // A present non-empty flag → Ok(Some(value)).
        let pw = resolve_consumption_password(Some("flag-pw"))
            .expect("a non-empty flag must be accepted")
            .expect("a present flag resolves to Some");
        assert_eq!(pw.expose_secret(), "flag-pw");

        // A present EMPTY flag (--password "") → Err on the consumption path
        // (unlike create/update, which defer the empty-string rejection to the
        // library validator). The error must steer the user to omit --password.
        let err = resolve_consumption_password(Some(""))
            .expect_err("an empty flag must be rejected on the consumption path");
        let msg = err.to_string();
        assert!(
            msg.contains("link password cannot be empty"),
            "the empty-password error must explain the fix, got: {msg}"
        );
        assert!(
            msg.contains("omit --password"),
            "the error must steer to omitting --password, got: {msg}"
        );
    }

    // ─── confirm_destructive ────────────────────────────────────────────────

    #[test]
    fn confirm_destructive_yes_proceeds_and_noninteractive_blocks() {
        assert!(confirm_destructive("act", "detail", true).is_ok());
        // In the test harness stdin/stderr are not TTYs, so omitting --yes must
        // hard-error rather than prompt.
        assert!(confirm_destructive("act", "detail", false).is_err());
    }

    // ─── map_fileshare_error ────────────────────────────────────────────────

    #[test]
    fn map_1650_steers_to_password_not_login() {
        // On a LINK-ACCESS op (consumption / write-back) a 1650 is a link-password
        // failure → steer to --password, never to account login.
        let m =
            map_fileshare_error(api_err(1650, 401), "failed to get", FsOp::LinkAccess).to_string();
        assert!(m.contains("--password"), "must steer to --password: {m}");
        assert!(m.contains(PASSWORD_ENV), "must mention the env var: {m}");
        assert!(
            !m.to_lowercase().contains("auth login"),
            "must NOT suggest account login for a link password: {m}"
        );
    }

    #[test]
    fn map_management_1650_is_account_auth_not_password() {
        // P2F-2: on a MANAGEMENT op (account-token auth) a 1650/401 is an
        // account-auth failure, NOT a link password — the headline must NOT
        // mention link passwords, and the rendered hint must be the generic
        // account-login guidance (verified via the real render path below).
        let err = map_fileshare_error(
            api_err(1650, 401),
            "failed to list File Shares",
            FsOp::ManagementOther,
        );
        let chain = format!("{err:#}").to_lowercase();
        assert!(
            !chain.contains("--password") && !chain.contains("link password"),
            "a management 1650 must NOT mention a link password: {chain}"
        );
        // The full render: the hint falls through to the generic 401 "auth login".
        let (_headline, hint) = render_mapped(&err);
        assert!(
            hint.unwrap_or_default()
                .to_lowercase()
                .contains("auth login"),
            "a management 1650 must keep the generic account-login hint: {hint:?}"
        );
    }

    #[test]
    fn map_1700_describes_capability_order() {
        let m = map_fileshare_error(api_err(1700, 403), "failed to upload", FsOp::LinkAccess)
            .to_string();
        assert!(
            m.contains("view") && m.contains("download") && m.contains("edit"),
            "must describe capability order: {m}"
        );
    }

    #[test]
    fn map_1609_is_uniform_unavailable() {
        let m =
            map_fileshare_error(api_err(1609, 404), "failed to get", FsOp::LinkAccess).to_string();
        assert!(m.contains("unavailable"), "must say unavailable: {m}");
        // Must offer ALL three possibilities and never single one out.
        assert!(
            m.contains("not exist") && m.contains("expired") && m.contains("revoked"),
            "must list all three reasons uniformly: {m}"
        );
    }

    #[test]
    fn map_bare_404_is_also_uniform_unavailable() {
        // A 404 without the 1609 code still maps to the uniform message.
        let m = map_fileshare_error(api_err(0, 404), "failed to get", FsOp::LinkAccess).to_string();
        assert!(
            m.contains("unavailable"),
            "bare 404 must be unavailable: {m}"
        );
    }

    // ─── LV CLI-1: preview-specific 404 / 143705 ────────────────────────────

    #[test]
    fn map_preview_143705_is_preview_not_uniform_unavailable() {
        // A 143705 ("Unable to retrieve preview") is a PREVIEW miss — the share
        // exists, the requested preview asset does not. It must NOT collapse into
        // the uniform "share unavailable" wording, and must steer to --type/retry.
        // Op-independent: the code alone keys it (it only ever arises on preview).
        let m = map_fileshare_error(api_err(143_705, 404), "failed to preview", FsOp::Preview)
            .to_string();
        assert!(
            m.contains("no preview of this type"),
            "143705 must use the preview-specific wording: {m}"
        );
        assert!(m.contains("--type"), "143705 must steer to --type: {m}");
        assert!(
            !m.contains("may have expired") && !m.contains("may have been revoked"),
            "143705 must NOT use the uniform share-gone wording: {m}"
        );
    }

    #[test]
    fn map_preview_bare_404_is_preview_not_uniform_unavailable() {
        // A bare 404 (no 1609, no 143705) on the PREVIEW op is a preview miss, not
        // a share-gone — the share was reached but the preview asset is absent.
        let m =
            map_fileshare_error(api_err(0, 404), "failed to preview", FsOp::Preview).to_string();
        assert!(
            m.contains("no preview of this type"),
            "a bare 404 on the preview op must be preview-specific: {m}"
        );
        assert!(
            !m.contains("may have been revoked"),
            "a bare 404 on the preview op must NOT be the uniform share-gone wording: {m}"
        );
    }

    #[test]
    fn map_preview_1609_stays_uniform_unavailable() {
        // A 1609 on the preview op means the SHARE itself is gone — it must KEEP
        // the uniform-unavailable discipline (do NOT weaken it to a preview miss).
        let m =
            map_fileshare_error(api_err(1609, 404), "failed to preview", FsOp::Preview).to_string();
        assert!(
            m.contains("unavailable") && m.contains("not exist") && m.contains("revoked"),
            "a 1609 on the preview op must stay uniform-unavailable: {m}"
        );
        assert!(
            !m.contains("no preview of this type"),
            "a 1609 (share gone) must NOT be reframed as a preview miss: {m}"
        );
    }

    #[test]
    fn map_nonpreview_bare_404_stays_uniform_unavailable() {
        // The uniform-404 discipline for NON-preview consumption ops (info /
        // download / versions) must be untouched: a bare 404 there is share-gone.
        let m = map_fileshare_error(api_err(0, 404), "failed to get", FsOp::LinkAccess).to_string();
        assert!(
            m.contains("unavailable") && m.contains("revoked"),
            "a bare 404 on a non-preview op must stay uniform-unavailable: {m}"
        );
        assert!(
            !m.contains("no preview of this type"),
            "a non-preview 404 must NOT borrow the preview wording: {m}"
        );
    }

    #[test]
    fn render_mapped_preview_143705_shows_preview_hint_not_verify_id() {
        // Driven through the REAL render path: a 143705 must print OUR preview
        // hint (retry / another --type), NOT the generic-404 "Verify the ID …"
        // line and NOT a suppressed (None) hint.
        let err = map_fileshare_error(
            api_err(143_705, 404),
            "failed to download File Share preview",
            FsOp::Preview,
        );
        let (headline, hint) = render_mapped(&err);
        assert!(
            headline.to_lowercase().contains("no preview of this type"),
            "headline must carry the preview miss: {headline}"
        );
        let hint = hint
            .expect("a mapped 143705 must carry a preview hint")
            .to_lowercase();
        assert!(
            hint.contains("--type") && hint.contains("retry"),
            "rendered hint must steer to --type / retry: {hint}"
        );
        assert!(
            !hint.contains("verify the id"),
            "rendered hint must NOT be the generic-404 verify-id line: {hint}"
        );
    }

    #[test]
    fn map_1680_is_not_serveable() {
        let m = map_fileshare_error(api_err(1680, 403), "failed to download", FsOp::LinkAccess)
            .to_string();
        assert!(
            m.contains("cannot be served") || m.contains("not be served"),
            "must say the file cannot be served: {m}"
        );
    }

    #[test]
    fn map_1605_create_hints_node_must_be_file() {
        let m =
            map_fileshare_error(api_err(1605, 400), "failed to create", FsOp::Create).to_string();
        assert!(
            m.contains("FILE node"),
            "create 1605 must hint node-is-file: {m}"
        );
        // Non-create 1605 just surfaces the server message, no file hint.
        let other = map_fileshare_error(
            api_err(1605, 400),
            "failed to update",
            FsOp::ManagementOther,
        )
        .to_string();
        assert!(
            !other.contains("FILE node"),
            "non-create 1605 must not add the file hint: {other}"
        );
    }

    #[test]
    fn map_version_conflict_passes_through_with_current_version() {
        let err = CliError::VersionConflict {
            current_version: "v9-abc".to_owned(),
        };
        let mapped = map_fileshare_error(err, "failed to upload", FsOp::LinkAccess);
        // The mapping must NOT re-key the conflict onto CliError::Api — it stays
        // a VersionConflict (so the pretty renderer fires the current-version
        // hint) and preserves the current version id verbatim.
        let cli = mapped
            .downcast_ref::<CliError>()
            .expect("conflict must remain a CliError so the pretty path fires");
        assert!(
            matches!(cli, CliError::VersionConflict { current_version } if current_version == "v9-abc"),
            "must preserve the current version id on a VersionConflict: {cli:?}"
        );
        // The full chain (alternate format) carries both the operation label and
        // the current-version Display.
        let chain = format!("{mapped:#}");
        assert!(chain.contains("failed to upload"), "chain: {chain}");
        assert!(
            chain.contains("v9-abc"),
            "chain must carry current version: {chain}"
        );
    }

    // ─── P2F-5: render-path hint override / suppression ─────────────────────

    #[test]
    fn render_mapped_1650_shows_password_hint_not_auth_login() {
        // The REAL render path must print OUR override hint, not the inner
        // ApiError's generic 401 "run `fastio auth login`".
        let err = map_fileshare_error(
            api_err(1650, 401),
            "failed to download File Share",
            FsOp::LinkAccess,
        );
        let (headline, hint) = render_mapped(&err);
        // Headline still carries the operation label + full server Display.
        assert!(
            headline.contains("failed to download File Share"),
            "{headline}"
        );
        let hint = hint
            .expect("a mapped 1650 must carry a hint")
            .to_lowercase();
        assert!(
            hint.contains("--password"),
            "rendered hint must steer to --password: {hint}"
        );
        assert!(
            !hint.contains("auth login"),
            "rendered hint must NOT be the generic account-login line: {hint}"
        );
    }

    #[test]
    fn render_mapped_1609_is_uniform_and_suppresses_verify_id_hint() {
        // The uniform 1609 must render its uniform headline and SUPPRESS the
        // generic-404 "Verify the ID or path is correct." hint (which would imply
        // a fixable id typo and re-introduce an enumeration oracle).
        let err = map_fileshare_error(
            api_err(1609, 404),
            "failed to get File Share details",
            FsOp::LinkAccess,
        );
        let (headline, hint) = render_mapped(&err);
        assert!(
            headline.to_lowercase().contains("unavailable"),
            "{headline}"
        );
        assert_eq!(
            hint, None,
            "the uniform 1609 must print NO hint (suppress the generic-404 verify-id line): {hint:?}"
        );
        // And the generic phrasing must appear nowhere in the rendered output.
        assert!(
            !headline.to_lowercase().contains("verify the id"),
            "the 'Verify the ID' phrasing must not leak: {headline}"
        );
    }

    #[test]
    fn render_mapped_preserves_dedup_no_doubled_api_block() {
        // P2F-5 must not regress render_chain_dedup: wrapping in MappedApi (a
        // plain `api` field, like ArtifactNotReady) generates no `source()` link,
        // so the server Display must appear EXACTLY ONCE in the rendered chain.
        let err = map_fileshare_error(api_err(1700, 403), "failed to upload", FsOp::LinkAccess);
        let (headline, _hint) = render_mapped(&err);
        // The inner ApiError Display is "[HTTP 403] boom (code 1700)". It must not
        // appear twice back-to-back.
        let needle = "[HTTP 403] boom (code 1700)";
        let count = headline.matches(needle).count();
        assert_eq!(
            count, 1,
            "the server Display must render exactly once (no #[from] source double): {headline}"
        );
    }

    // ─── P2F-1: chunk order is 1-based ──────────────────────────────────────

    #[test]
    fn writeback_chunk_order_is_one_based() {
        // The FIRST chunk (zero-based index 0) MUST go out as `order=1` per the
        // upload contract (upload.txt:35,246). A 0-based first chunk would be
        // rejected server-side.
        assert_eq!(
            writeback_chunk_order(0).expect("first chunk order"),
            1,
            "the first write-back chunk must be order=1, not 0"
        );
        assert_eq!(writeback_chunk_order(1).expect("second chunk order"), 2);
        assert_eq!(writeback_chunk_order(41).expect("nth chunk order"), 42);
        // Overflow is reported, not panicked.
        assert!(
            writeback_chunk_order(u32::MAX).is_err(),
            "an overflowing chunk index must error, not wrap"
        );
    }

    // ─── P2F-6: write-back source validation ────────────────────────────────

    #[test]
    fn validate_writeback_source_rejects_dir_and_missing_allows_file() {
        // A directory is rejected with the friendly "not a regular file" message.
        let dir = std::env::temp_dir();
        let err = validate_writeback_source(&dir, &dir.display().to_string())
            .expect_err("a directory must be rejected as a write-back source");
        assert!(
            err.to_string().contains("not a regular file"),
            "directory rejection must be friendly: {err}"
        );

        // A missing path is rejected with the not-found message.
        let missing = dir.join("definitely-not-here-fileshare-p2f6.bin");
        let err = validate_writeback_source(&missing, &missing.display().to_string())
            .expect_err("a missing path must be rejected");
        assert!(
            err.to_string().contains("file not found"),
            "missing-path rejection must say not found: {err}"
        );

        // A real regular file is accepted.
        let file = dir.join(format!("fileshare-p2f6-{}.bin", std::process::id()));
        std::fs::write(&file, b"hi").expect("write temp file");
        let ok = validate_writeback_source(&file, &file.display().to_string());
        let _ = std::fs::remove_file(&file);
        assert!(ok.is_ok(), "a regular file must be accepted: {ok:?}");
    }

    // ─── R-1: write-back name validation ────────────────────────────────────

    /// The write-back name MUST pass the same filename validation the normal
    /// upload path applies, BEFORE the confirm gate. An explicit empty `--name`,
    /// a path-separator name, or a CR/LF name must all be rejected; a normal
    /// name passes. `resolve_writeback_name` is the pure gate the destructive
    /// `upload_writeback` calls immediately after deriving the name.
    #[test]
    fn resolve_writeback_name_validates_before_confirm() {
        let path = Path::new("/tmp/local-source.bin");

        // An explicit empty --name is rejected (would otherwise reach the
        // destructive write-back as an empty `file_name`).
        let err = resolve_writeback_name(path, "/tmp/local-source.bin", Some(""))
            .expect_err("an empty --name must be rejected");
        assert!(
            err.to_string().contains("invalid upload name")
                && err.to_string().contains("must not be empty"),
            "empty-name rejection must be the validate_filename message: {err}"
        );

        // A path-separator name is rejected.
        let err = resolve_writeback_name(path, "/tmp/local-source.bin", Some("a/b.txt"))
            .expect_err("a separator name must be rejected");
        assert!(
            err.to_string().contains("path separators"),
            "separator rejection must mention path separators: {err}"
        );

        // A CR/LF name is rejected (would corrupt the multipart envelope).
        let err = resolve_writeback_name(path, "/tmp/local-source.bin", Some("a\r\nb.txt"))
            .expect_err("a CRLF name must be rejected");
        assert!(
            err.to_string().contains("CR or LF"),
            "CRLF rejection must mention CR or LF: {err}"
        );

        // A normal explicit name passes and is returned verbatim.
        let ok =
            resolve_writeback_name(path, "/tmp/local-source.bin", Some("report.pdf")).expect("ok");
        assert_eq!(ok, "report.pdf", "a valid --name passes through verbatim");

        // With no --name, the local file's base name is derived and validated.
        let ok = resolve_writeback_name(path, "/tmp/local-source.bin", None)
            .expect("a valid derived name passes");
        assert_eq!(ok, "local-source.bin", "derived from the source base name");
    }

    // ─── R-2: activity empty-id guard ───────────────────────────────────────

    /// An empty `fileshare_id` must be rejected BEFORE `event::poll_activity` is
    /// reached, so the malformed `/activity/poll//` path is never built. A
    /// non-empty id passes the guard.
    #[test]
    fn require_fileshare_id_rejects_empty() {
        let err = require_fileshare_id("").expect_err("an empty id must be rejected");
        let cli = err
            .downcast_ref::<CliError>()
            .expect("the rejection must be rooted at a CliError::Parse");
        assert!(
            matches!(cli, CliError::Parse(m) if m.contains("File Share id is required")),
            "must be a Parse error mirroring the library require_id wording: {cli:?}"
        );
        assert!(
            require_fileshare_id("9xQ2-abc12").is_ok(),
            "a non-empty id must pass the guard"
        );
    }

    // ─── write-back session inspection ──────────────────────────────────────

    #[test]
    fn check_writeback_session_conflict_yields_version_conflict() {
        let v = json!({
            "result": true,
            "session": {
                "status": "assembly_failed",
                "status_message": "CONFLICT_VERSION_MISMATCH:v9xQ2-abc12"
            }
        });
        let err = check_writeback_session(&v).expect_err("conflict must error");
        let cli = err
            .downcast_ref::<CliError>()
            .expect("must remain a CliError so the pretty path fires");
        assert!(
            matches!(cli, CliError::VersionConflict { current_version } if current_version == "v9xQ2-abc12"),
            "must be a VersionConflict carrying the current version: {cli:?}"
        );
    }

    #[test]
    fn check_writeback_session_other_failure_surfaces_message() {
        let v = json!({
            "session": {"status": "assembly_failed", "status_message": "disk full"}
        });
        let err = check_writeback_session(&v).expect_err("failure must error");
        assert!(
            err.to_string().contains("disk full"),
            "must surface the server message: {err}"
        );
        // A non-conflict failure must NOT be a VersionConflict.
        assert!(
            err.downcast_ref::<CliError>()
                .is_none_or(|c| !matches!(c, CliError::VersionConflict { .. })),
            "a non-conflict failure must not be a VersionConflict"
        );
    }

    #[test]
    fn check_writeback_session_success_is_ok() {
        let v = json!({"session": {"status": "complete"}});
        assert!(check_writeback_session(&v).is_ok());
        let v2 = json!({"session": {"status": "stored"}});
        assert!(check_writeback_session(&v2).is_ok());
    }

    #[test]
    fn writeback_status_reads_session_and_top_level() {
        assert_eq!(
            writeback_status(&json!({"session": {"status": "complete"}})),
            "complete"
        );
        assert_eq!(writeback_status(&json!({"status": "stored"})), "stored");
        assert_eq!(writeback_status(&json!({})), "");
    }
}
