//! E-signature (`fastio sign`) command handlers.
//!
//! Owner/admin surface over [`fastio_cli::api::signing`]. Every command is
//! workspace-scoped (a required `--workspace <id>`); the former org surface was
//! removed. These handlers enforce the binding signing disciplines:
//!
//! - **Destructive confirmation.** `void` is terminal, and `send` emails REAL
//!   recipients — both require `--yes` (or an interactive y/N confirmation on a
//!   TTY) before proceeding. (There is no `delete`: envelopes are voided, not
//!   deleted.)
//! - **`@file` JSON.** The ergonomic `--*-json` / `--body-json` arguments accept
//!   `@path` to read JSON from a file and are validated as well-formed JSON
//!   client-side before any state-changing call.
//! - **Binary downloads stream to disk.** Document source/preview/signed PDFs
//!   and the audit certificate are streamed via
//!   [`fastio_cli::client::ApiClient::download_file_stream`] (direct-Bearer,
//!   atomic temp write) — a signing node id is NEVER routed through
//!   `/storage/{node}/read/` (`signing.txt:155`).
//! - **Error mapping.** Signing-specific wording lives HERE in
//!   [`map_signing_error`] (never in the global `error.rs` hints). A
//!   `404`/`1609`/`128301`/`146422` is surfaced as "not ready yet" ONLY on a
//!   signed/audit artifact fetch (a source/preview-download or CRUD `404` is a
//!   genuine not-found); `10545`/`115069` are workspace/envelope access denials;
//!   `1680` is an insufficient-permission denial; `1670` is a plan restriction;
//!   `9992` flags a removed/renamed route; `1685` is "insufficient signing
//!   credits"; a `void` on a terminal envelope (`1660`) is surfaced clearly.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use fastio_cli::api::signing::{
    self, CreateEnvelopeParams, DocumentSpec, RecipientSpec, UpdateEnvelopeParams,
};
use fastio_cli::error::CliError;

use crate::cli::{SignAuditCommands, SignCommands, SignDocumentCommands, SignEnvelopeCommands};

use super::CommandContext;

// ─── @file JSON resolution ──────────────────────────────────────────────────

/// Resolve a JSON-string argument, supporting an `@path` form that reads the
/// JSON from a file. A leading `@` selects file mode; `@@` is an escape for a
/// literal value beginning with `@`. The resolved text is parsed and returned
/// as a [`Value`] so a malformed body is rejected client-side before any
/// state-changing call. Mirrors the workflow command's `resolve_json_arg`.
fn resolve_json_value(raw: &str, label: &str) -> Result<Value> {
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
    serde_json::from_str::<Value>(&text).with_context(|| format!("{label} is not valid JSON"))
}

/// Resolve an optional JSON-string argument into a [`Value`] (see
/// [`resolve_json_value`]).
fn resolve_opt_json_value(raw: Option<&str>, label: &str) -> Result<Option<Value>> {
    match raw {
        Some(v) => Ok(Some(resolve_json_value(v, label)?)),
        None => Ok(None),
    }
}

/// Resolve an optional JSON ARRAY argument, rejecting a non-array.
fn resolve_opt_json_array(raw: Option<&str>, label: &str) -> Result<Option<Vec<Value>>> {
    match resolve_opt_json_value(raw, label)? {
        None => Ok(None),
        Some(Value::Array(items)) => Ok(Some(items)),
        Some(_) => anyhow::bail!("{label} must be a JSON array"),
    }
}

/// Resolve an optional JSON OBJECT argument, rejecting a non-object.
///
/// `policy_json` is documented as an OBJECT (`signing.txt:291`); an array /
/// scalar / null is rejected client-side before any state-changing call.
fn resolve_opt_json_object(raw: Option<&str>, label: &str) -> Result<Option<Value>> {
    match resolve_opt_json_value(raw, label)? {
        None => Ok(None),
        Some(v @ Value::Object(_)) => Ok(Some(v)),
        Some(_) => anyhow::bail!("{label} must be a JSON object"),
    }
}

// ─── Spec parsing (JSON array → typed builders) ─────────────────────────────

/// Ensure a JSON array element is an OBJECT, naming the array and index on
/// failure.
///
/// Each spec parser reads its fields via [`Value::get`], which returns `None`
/// for a non-object (scalar / null / array) — so a malformed element like `[1]`
/// or `[null]` would otherwise yield an all-`None` (EMPTY) spec that silently
/// passes the recipients-required guard and ships garbage. Rejecting the
/// non-object up front turns that into a clear client-side error (e.g.
/// `recipients[0] must be a JSON object`). Field-level requirements are left to
/// the server.
fn ensure_object<'a>(v: &'a Value, label: &str, index: usize) -> Result<&'a Value> {
    if v.is_object() {
        Ok(v)
    } else {
        anyhow::bail!("{label}[{index}] must be a JSON object")
    }
}

/// Parse a documents JSON array into [`DocumentSpec`] builders, matching the
/// `signing.txt:298-304` / `:349-352` object shape.
fn parse_documents(items: Vec<Value>) -> Result<Vec<DocumentSpec>> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            ensure_object(&v, "documents", i)?;
            Ok(DocumentSpec::new()
                .id(str_field(&v, "id")?)
                .source_node_id(str_field(&v, "source_node_id")?)
                .source_version_id(str_field(&v, "source_version_id")?)
                .display_order(u64_field(&v, "display_order")?))
        })
        .collect()
}

/// Parse a recipients JSON array into [`RecipientSpec`] builders.
fn parse_recipients(items: Vec<Value>) -> Result<Vec<RecipientSpec>> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            ensure_object(&v, "recipients", i)?;
            Ok(RecipientSpec::new()
                .email(str_field(&v, "email")?)
                .display_name(str_field(&v, "display_name")?)
                .phone_e164(str_field(&v, "phone_e164")?)
                .role(str_field(&v, "role")?)
                .routing_order(u64_field(&v, "routing_order")?)
                .auth_method(str_field(&v, "auth_method")?))
        })
        .collect()
}

/// Parse a fields JSON array into [`signing::FieldSpec`] builders. The `type`
/// key carries the field type (`type` is a Rust keyword, so the struct field is
/// `field_type`).
fn parse_fields(items: Vec<Value>) -> Result<Vec<signing::FieldSpec>> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            ensure_object(&v, "fields", i)?;
            // `value_json` is a JSON STRING; re-serialize a non-string value so
            // an object literal in the input is preserved as a string.
            let value_json = v.get("value_json").and_then(|vj| match vj {
                Value::Null => None,
                Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            });
            Ok(signing::FieldSpec::new()
                .recipient_email(str_field(&v, "recipient_email")?)
                .document_index(u64_field(&v, "document_index")?)
                .page(u64_field(&v, "page")?)
                .bounding_box(
                    f64_field(&v, "x_norm")?,
                    f64_field(&v, "y_norm")?,
                    f64_field(&v, "w_norm")?,
                    f64_field(&v, "h_norm")?,
                )
                .field_type(str_field(&v, "type")?)
                .required(bool_field(&v, "required")?)
                .value_json(value_json))
        })
        .collect()
}

/// Read an optional string field from a JSON object.
///
/// A missing key is `None`. A PRESENT key that is not a JSON string is an
/// error rather than a silent drop, so a mistyped field (e.g. a numeric
/// `email`) is rejected instead of vanishing from the request.
fn str_field(v: &Value, key: &str) -> Result<Option<String>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => anyhow::bail!("field '{key}' must be a JSON string"),
    }
}

/// Read an optional `u64` field (number or string-encoded) from a JSON object.
///
/// A missing key is `None`. A PRESENT key that is neither a `u64` nor a string
/// that parses as one is an error rather than a silent drop.
fn u64_field(v: &Value, key: &str) -> Result<Option<u64>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(f) => f
            .as_u64()
            .or_else(|| f.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("field '{key}' must be a non-negative integer")),
    }
}

/// Read an optional `f64` field (number or string-encoded) from a JSON object.
///
/// A missing key is `None`. A PRESENT key that is neither an `f64` nor a string
/// that parses as one is an error rather than a silent drop, so a malformed
/// coordinate (e.g. `"x_norm":"abc"`) is rejected instead of placing a field
/// at a bogus position.
fn f64_field(v: &Value, key: &str) -> Result<Option<f64>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(f) => f
            .as_f64()
            .or_else(|| f.as_str().and_then(|s| s.parse().ok()))
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("field '{key}' must be a number")),
    }
}

/// Read an optional boolean field from a JSON object.
///
/// A missing key is `None`. A PRESENT key that is not a JSON boolean is an
/// error rather than a silent drop.
fn bool_field(v: &Value, key: &str) -> Result<Option<bool>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => anyhow::bail!("field '{key}' must be a JSON boolean"),
    }
}

// ─── Destructive-action confirmation ─────────────────────────────────────────

/// Gate a destructive / outward-facing action behind explicit confirmation.
///
/// `--yes` proceeds unconditionally. Without it, an interactive TTY is prompted
/// y/N; a non-interactive caller that omitted `--yes` is blocked
/// deterministically (so an unattended script never silently sends or voids).
/// Mirrors the metadata/workflow `confirm_spend` shape.
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

// ─── Command-layer error mapping ──────────────────────────────────────────────

/// Discriminates an async-artifact FETCH (the signed PDF or the audit
/// certificate) from every other signing call.
///
/// Only these two artifacts are generated asynchronously and return
/// `404`/`1609`/`128301`/`146422` ("not ready yet") until the envelope reaches
/// the required (terminal) stage (`signing.txt:520`, `:531`; live signed-PDF
/// not-ready is code `146422`, live audit-not-ready is code `128301`). For CRUD
/// / list / get / update / send AND the source / preview document downloads, a
/// `404`/`1609` instead means a genuine not-found (typo'd id / wrong workspace,
/// `signing.txt:591`), so the "poll and retry" framing would be misleading —
/// those callers fall through to the shared generic not-found handling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SignOp {
    /// A CRUD / list / get / update / send call, or a source/preview document
    /// download — a `404`/`1609` is a genuine not-found here.
    General,
    /// A signed-PDF fetch — a `404`/`1609`/`128301`/`146422` means the signed
    /// document is not ready yet, not that the row is missing. The signed PDF
    /// becomes available once the envelope **completes**, so the not-ready
    /// guidance steers to "poll until it completes".
    SignedFetch,
    /// An audit-certificate fetch — a `404`/`1609`/`128301`/`146422` means the
    /// certificate is not ready yet, not that the row is missing. The audit
    /// certificate becomes available once the envelope reaches **any terminal
    /// state** (completed/declined/voided/expired/failed), so the not-ready
    /// guidance steers to "poll until it reaches a terminal state".
    AuditFetch,
}

impl SignOp {
    /// `true` for an async signed/audit artifact fetch (where a
    /// `404`/`1609`/`128301`/`146422` means "not ready yet" rather than a
    /// genuine not-found). `false` for [`SignOp::General`].
    fn is_artifact_fetch(self) -> bool {
        matches!(self, SignOp::SignedFetch | SignOp::AuditFetch)
    }
}

/// Map a signing API error to an actionable, signing-specific message.
///
/// All signing-specific wording lives HERE (and in the MCP `sign_err_to_result`
/// mirror) — never in the global `error.rs` hints, which stay resource-agnostic
/// (guard test `error.rs`: `HINT_RESTRICTED` etc. contain no "sign"). This layer
/// keys on the actual `error.code`, NOT a bare HTTP status, so an unrelated
/// error with the same status is not mislabeled:
///
/// - An error matching `http_status == 404 || code == 1609 || code == 128301
///   || code == 146422` AND `code != 9992` (the OR-predicate the code
///   evaluates; the live not-ready codes are `1609`/`128301`/`146422`, all also
///   served as HTTP 404 — `146422` and the bare 404 arm are independent so a
///   non-404 `146422` still matches) is reframed as "not ready yet — poll and
///   retry" ONLY for a signed/audit artifact fetch ([`SignOp::SignedFetch`] /
///   [`SignOp::AuditFetch`]); it is re-keyed onto [`CliError::ArtifactNotReady`]
///   so the rendered `hint:` is the poll-and-retry guidance, not the
///   generic-404 "verify the id" (the ids are fine). The poll target is
///   artifact-appropriate: the signed PDF becomes available once the envelope
///   **completes**, the audit certificate once the envelope reaches **any
///   terminal state** (completed/declined/voided/expired/failed). The
///   router-level `9992` ("no such
///   route", also HTTP 404) is the one exclusion, so it falls through to the
///   removed/renamed-route framing instead of "poll a dead route forever". For
///   [`SignOp::General`] the error is left to the shared generic not-found
///   handling.
/// - `10545` (401) → not a member of this workspace. Overrides the generic 401
///   "run fastio auth login" suggestion (the failure is authorization, not a
///   missing login).
/// - `115069` (401) → no access to this specific envelope.
/// - `1680` (403) → workspace permission insufficient for this action (kept
///   generic — the docs disagree on the exact role required).
/// - `1670` (403) → plan does not grant signing; points at `fastio org info`
///   → `capabilities.signing`.
/// - `9992` (404) → the server does not recognize this path (removed/renamed
///   route — check for a CLI update).
/// - `1685` (insufficient credits) / `1660` (terminal-state conflict) keyed on
///   their codes.
/// - Every unmatched code falls through to the shared `CliError::suggestion()`
///   the render layer (`cli_error_render` in `main.rs`) appends on its own
///   `hint:` line — this layer attaches only the operation label there.
fn map_signing_error(err: CliError, ctx: &'static str, op: SignOp) -> anyhow::Error {
    // Take ownership of the inner `ApiError` up front. A non-`Api` `CliError`
    // (auth / io / parse …) has no signing-specific framing — attach only the
    // operation label and return. Matching by value (rather than the old
    // `if let … = &err` + a second `let-else` to move `api` out) means there is
    // no `unreachable!()` in this production path: the shape is proven by the
    // arm we are inside, not re-asserted with a panic-capable macro.
    let api = match err {
        CliError::Api(api) => api,
        other => return anyhow::Error::from(other).context(ctx),
    };

    // Async-artifact "not ready yet" — only correct for a signed/audit fetch.
    // Code 9992 is a router-level "no such route" that also surfaces as HTTP
    // 404; it must be EXCLUDED here so the code-specific match below frames it
    // as a removed/renamed route instead of "poll and retry" (otherwise an
    // agent would poll a dead route forever).
    let is_artifact_not_ready = op.is_artifact_fetch()
        && api.code != 9992
        && (api.http_status == 404
            || api.code == 1609
            || api.code == 128_301
            || api.code == 146_422);
    if is_artifact_not_ready {
        // Re-key onto the dedicated `ArtifactNotReady` variant rather than
        // wrapping `CliError::Api` so (a) the rendered `hint:` line is the
        // poll-and-retry guidance, NOT the misleading generic-404 "Verify
        // the ID or path is correct." (the ids are fine — the artifact just
        // is not generated yet), and (b) the variant WRAPS the `ApiError`
        // by value in a plain field (no `#[source]`), so its `Display` is
        // the FULL server error (status / code / `see:` / `resource:`)
        // rendered exactly ONCE — no duplicate source link, no doubling.
        // The signing-specific phrasing stays in this `.context(...)`. The
        // `ApiError` is MOVED into the variant, so nothing clones.
        //
        // The poll target is artifact-appropriate: the signed PDF is
        // available once the envelope COMPLETES; the audit certificate once
        // the envelope reaches any TERMINAL state
        // (completed/declined/voided/expired/failed).
        let stage = match op {
            SignOp::AuditFetch => {
                "the audit certificate is not generated until the envelope reaches a terminal \
                 state. Poll the envelope (`fastio sign envelope get --workspace <workspace-id> \
                 <envelope-id>`) and retry once it reaches any terminal state (e.g. completed, \
                 declined, voided, expired, or failed)."
            }
            // SignedFetch (General never reaches here — guarded by is_artifact_fetch()).
            _ => {
                "the signed document is not generated until the envelope completes. Poll the \
                 envelope (`fastio sign envelope get --workspace <workspace-id> <envelope-id>`) \
                 and retry once it completes."
            }
        };
        return anyhow::Error::from(CliError::ArtifactNotReady { api })
            .context(format!("{ctx}: not ready yet — {stage}"));
    }

    // Code-specific signing framings (keyed on error.code, not bare status).
    // Each matched arm moves `api` back into `CliError::Api(api)` before
    // attaching its context (the `_` arm falls through to the shared label).
    let note = match api.code {
        // Not a member of the workspace. MUST override the generic 401
        // "run fastio auth login" suggestion — the caller IS authenticated.
        10545 => Some(format!(
            "{ctx}: you are not a member of this workspace (10545). Signing requires \
             workspace membership — ask a workspace admin to add you, or check `--workspace`."
        )),
        // No access to this specific envelope.
        115_069 => Some(format!(
            "{ctx}: you do not have access to this envelope (115069). Confirm the \
             envelope id and that you have permission on its workspace."
        )),
        // Insufficient workspace permission for this action. Kept generic —
        // the docs disagree on the exact role required, so do not overclaim.
        1680 => Some(format!(
            "{ctx}: your workspace permission is insufficient for this signing action \
             (1680). A higher workspace role may be required."
        )),
        // Org plan does not grant signing.
        1670 => Some(format!(
            "{ctx}: signing is not enabled for this workspace's organization (1670). \
             Check the org's plan capability: `fastio org info <org-id>` → `capabilities.signing`."
        )),
        // Removed / renamed route (router-level "no such route").
        9992 => Some(format!(
            "{ctx}: the server does not recognize this API path (9992) — the route may \
             have been removed or renamed. Check for a `fastio` CLI update."
        )),
        // Insufficient signing credits on /send.
        1685 => Some(format!(
            "{ctx}: insufficient signing credits to send this envelope (1685). \
             Check your plan's signing credit balance (`fastio org billing plans`)."
        )),
        // Void / transition not allowed from a terminal state.
        1660 => Some(format!(
            "{ctx}: the envelope is already terminal (completed / declined / voided / \
             expired) and cannot be changed (1660)."
        )),
        // Send precondition not met (checked only at send time, signing.txt:360).
        201_134 => Some(format!(
            "{ctx}: the envelope is not ready to send (201134) — a send precondition \
             failed. The server message names the specific gap (e.g. \"signer but has no \
             fields to complete\"); a sendable draft needs ≥1 document, ≥1 signing \
             recipient, and every signer-role recipient must have ≥1 field. Fix that \
             recipient/field and retry. Branch on the code (201134), not the message text."
        )),
        _ => None,
    };
    match note {
        Some(note) => anyhow::Error::from(CliError::Api(api)).context(note),
        // Everything else: attach ONLY the operation label as context. The
        // render layer (`cli_error_render` in `main.rs`) walks the anyhow chain
        // AND appends the `CliError`'s own `suggestion()` on its own `hint:` line.
        None => anyhow::Error::from(CliError::Api(api)).context(ctx),
    }
}

// ─── Dispatch ─────────────────────────────────────────────────────────────────

/// Execute a `fastio sign` command.
pub async fn execute(command: SignCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        SignCommands::Envelope(c) => execute_envelope(c, ctx).await,
        SignCommands::Document(c) => execute_document(c, ctx).await,
        SignCommands::Audit(c) => execute_audit(c, ctx).await,
    }
}

// ─── Envelope lifecycle ───────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)] // a flat dispatch over the envelope lifecycle surface
async fn execute_envelope(command: SignEnvelopeCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        SignEnvelopeCommands::Create {
            workspace,
            name,
            expires_at,
            body_json,
            policy_json,
            documents_json,
            recipients_json,
            fields_json,
            source_node_id,
            source_version_id,
            recipient_email,
            recipient_name,
            auth_method,
        } => {
            let params = build_create_params(
                name,
                expires_at,
                body_json.as_deref(),
                policy_json.as_deref(),
                documents_json.as_deref(),
                recipients_json.as_deref(),
                fields_json.as_deref(),
                source_node_id,
                source_version_id,
                recipient_email,
                recipient_name,
                auth_method,
            )?;
            // Validate caps client-side before the network for a clear error.
            params
                .validate()
                .map_err(|e| anyhow::Error::from(e).context("invalid create request"))?;
            let client = ctx.build_client()?;
            let v = signing::create_envelope(&client, &workspace, &params)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to create sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::List {
            workspace,
            status,
            created_after,
            created_before,
            limit,
            offset,
        } => {
            // `--status` maps to the `envelope_status` query key; a single
            // status or a CSV is passed through verbatim (server validates).
            let params = signing::ListEnvelopesParams::new()
                .envelope_status(status)
                .created_after(created_after)
                .created_before(created_before)
                .limit(limit)
                .offset(offset);
            let client = ctx.build_client()?;
            let v = signing::list_envelopes(&client, &workspace, &params)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to list sign envelopes", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Get {
            workspace,
            envelope_id,
        } => {
            let client = ctx.build_client()?;
            let v = signing::get_envelope(&client, &workspace, &envelope_id)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to get sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Update {
            workspace,
            envelope_id,
            name,
            expires_at,
            policy_json,
            documents_json,
            recipients_json,
            fields_json,
        } => {
            let params = build_update_params(
                name,
                expires_at,
                policy_json.as_deref(),
                documents_json.as_deref(),
                recipients_json.as_deref(),
                fields_json.as_deref(),
            )?;
            anyhow::ensure!(
                !params.is_empty(),
                "no fields to update were supplied: supply at least --recipients-json (recipients \
                 are a full replacement). --name / --documents-json / --fields-json are kept when \
                 omitted; --expires-at / --policy-json are DECLARATIVE — omitting them clears the \
                 envelope's expiry / policy"
            );
            // An update is a FULL recipient replacement — recipients (>=1) are
            // required (F5). Surface a clear, action-specific error before the
            // network rather than relying on the generic validate() message.
            anyhow::ensure!(
                params.recipients.as_deref().is_some_and(|r| !r.is_empty()),
                "an update is a full recipient replacement: supply --recipients-json with at \
                 least one recipient (an update always replaces the recipient roster)"
            );
            params
                .validate()
                .map_err(|e| anyhow::Error::from(e).context("invalid update request"))?;
            // `expires_at` / `policy_json` are declarative (signing.txt:344):
            // omitting one CLEARS it server-side. Warn before sending so a
            // rename-only update doesn't silently drop the expiry/policy.
            if !ctx.output.quiet
                && let Some(note) = update_declarative_clear_note(
                    params.expires_at.is_some(),
                    params.policy_json.is_some(),
                )
            {
                eprintln!("{note}");
            }
            let client = ctx.build_client()?;
            let v = signing::update_envelope(&client, &workspace, &envelope_id, &params)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to update sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Send {
            workspace,
            envelope_id,
            yes,
        } => {
            confirm_destructive(
                "sign envelope send",
                "transitions the draft to sent and EMAILS REAL RECIPIENTS (credits are reserved \
                 and not refunded)",
                yes,
            )?;
            let client = ctx.build_client()?;
            let v = signing::send_envelope(&client, &workspace, &envelope_id)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to send sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Void {
            workspace,
            envelope_id,
            reason,
            yes,
        } => {
            // Validate the reason BEFORE prompting for confirmation, so an empty
            // / oversize `--reason` fails immediately instead of after the user
            // has confirmed at the y/N prompt (signing.txt:382).
            signing::validate_void_reason(&reason)
                .map_err(|e| anyhow::Error::from(e).context("invalid void request"))?;
            confirm_destructive(
                "sign envelope void",
                "is IRREVERSIBLE — it permanently voids the envelope and signing credits are NOT \
                 refunded",
                yes,
            )?;
            let client = ctx.build_client()?;
            let v = signing::void_envelope(&client, &workspace, &envelope_id, &reason)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to void sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
    }
}

/// Build [`CreateEnvelopeParams`] from the create flags, preferring the
/// ergonomic JSON paths and falling back to the simple single-signer flags.
#[allow(clippy::too_many_arguments)] // a 1:1 fan-out of the clap Create variant's fields
fn build_create_params(
    name: Option<String>,
    expires_at: Option<String>,
    body_json: Option<&str>,
    policy_json: Option<&str>,
    documents_json: Option<&str>,
    recipients_json: Option<&str>,
    fields_json: Option<&str>,
    source_node_id: Option<String>,
    source_version_id: Option<String>,
    recipient_email: Option<String>,
    recipient_name: Option<String>,
    auth_method: Option<String>,
) -> Result<CreateEnvelopeParams> {
    // `--body-json` is the whole request — when present the other flags are
    // ignored and the request is assembled directly from its fields.
    if let Some(raw) = body_json {
        let body = resolve_json_value(raw, "body JSON")?;
        return create_params_from_body(&body);
    }

    let documents = if let Some(items) = resolve_opt_json_array(documents_json, "documents JSON")? {
        parse_documents(items)?
    } else {
        // Simple path: a single source document.
        let node = source_node_id.context(
            "create needs documents: pass --documents-json (or --body-json), or the simple \
             --source-node-id",
        )?;
        vec![
            DocumentSpec::new()
                .source_node_id(Some(node))
                .source_version_id(source_version_id)
                .display_order(Some(0)),
        ]
    };

    let recipients =
        if let Some(items) = resolve_opt_json_array(recipients_json, "recipients JSON")? {
            parse_recipients(items)?
        } else {
            // Simple path: a single signer.
            let email = recipient_email.context(
                "create needs recipients: pass --recipients-json (or --body-json), or the simple \
             --recipient-email",
            )?;
            vec![
                RecipientSpec::new()
                    .email(Some(email))
                    .display_name(recipient_name)
                    .role(Some("signer".to_owned()))
                    .routing_order(Some(1))
                    .auth_method(auth_method),
            ]
        };

    let fields = match resolve_opt_json_array(fields_json, "fields JSON")? {
        Some(items) => parse_fields(items)?,
        None => Vec::new(),
    };

    Ok(CreateEnvelopeParams::new()
        .name(name)
        .expires_at(expires_at)
        .policy_json(resolve_opt_json_object(policy_json, "policy JSON")?)
        .documents(documents)
        .recipients(recipients)
        .fields(fields))
}

/// Build [`CreateEnvelopeParams`] from a whole-request `--body-json` object.
///
/// `body_json` is documented as an OBJECT (`signing.txt:291`); a non-object
/// (array / scalar / null) is rejected before any params are built. A supplied
/// `policy_json` must likewise be an object.
fn create_params_from_body(body: &Value) -> Result<CreateEnvelopeParams> {
    if !body.is_object() {
        anyhow::bail!("body JSON must be a JSON object");
    }
    let documents = match body.get("documents") {
        Some(Value::Array(items)) => parse_documents(items.clone())?,
        Some(_) => anyhow::bail!("body JSON 'documents' must be an array"),
        None => Vec::new(),
    };
    let recipients = match body.get("recipients") {
        Some(Value::Array(items)) => parse_recipients(items.clone())?,
        Some(_) => anyhow::bail!("body JSON 'recipients' must be an array"),
        None => Vec::new(),
    };
    let fields = match body.get("fields") {
        Some(Value::Array(items)) => parse_fields(items.clone())?,
        Some(_) => anyhow::bail!("body JSON 'fields' must be an array"),
        None => Vec::new(),
    };
    let policy_json = match body.get("policy_json") {
        None | Some(Value::Null) => None,
        Some(p @ Value::Object(_)) => Some(p.clone()),
        Some(_) => anyhow::bail!("body JSON 'policy_json' must be a JSON object"),
    };
    Ok(CreateEnvelopeParams::new()
        .name(str_field(body, "name")?)
        .expires_at(str_field(body, "expires_at")?)
        .policy_json(policy_json)
        .documents(documents)
        .recipients(recipients)
        .fields(fields))
}

/// Build the declarative-clear warning for `sign envelope update`.
///
/// `expires_at` and `policy_json` are declarative (`signing.txt:344`): the server
/// rewrites them on every update, so omitting one CLEARS it (resets to `null`).
/// Returns a one-line note naming whichever field is being cleared, or `None`
/// when both were supplied (nothing is cleared). `name` / `documents` / `fields`
/// are preserved when omitted and are intentionally not mentioned.
fn update_declarative_clear_note(expires_at_set: bool, policy_json_set: bool) -> Option<String> {
    let mut cleared = Vec::new();
    if !expires_at_set {
        cleared.push("expiry (--expires-at)");
    }
    if !policy_json_set {
        cleared.push("policy (--policy-json)");
    }
    if cleared.is_empty() {
        return None;
    }
    Some(format!(
        "note: an update is declarative — this clears the envelope's {} (reset to null), since {} \
         not supplied. Re-send the current value(s) — see `sign envelope get` — to keep them.",
        cleared.join(" and "),
        if cleared.len() == 1 {
            "it was"
        } else {
            "they were"
        },
    ))
}

/// Build [`UpdateEnvelopeParams`] from the update flags. A `None` documents or
/// fields argument leaves that set unchanged; `recipients` is a REQUIRED full
/// replacement (validated downstream — a `None`/empty roster is rejected).
fn build_update_params(
    name: Option<String>,
    expires_at: Option<String>,
    policy_json: Option<&str>,
    documents_json: Option<&str>,
    recipients_json: Option<&str>,
    fields_json: Option<&str>,
) -> Result<UpdateEnvelopeParams> {
    let documents = match resolve_opt_json_array(documents_json, "documents JSON")? {
        Some(items) => Some(parse_documents(items)?),
        None => None,
    };
    let recipients = match resolve_opt_json_array(recipients_json, "recipients JSON")? {
        Some(items) => Some(parse_recipients(items)?),
        None => None,
    };
    let fields = match resolve_opt_json_array(fields_json, "fields JSON")? {
        Some(items) => Some(parse_fields(items)?),
        None => None,
    };
    Ok(UpdateEnvelopeParams::new()
        .name(name)
        .expires_at(expires_at)
        .policy_json(resolve_opt_json_object(policy_json, "policy JSON")?)
        .documents(documents)
        .recipients(recipients)
        .fields(fields))
}

// ─── Document downloads ───────────────────────────────────────────────────────

async fn execute_document(command: SignDocumentCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        SignDocumentCommands::Download {
            workspace,
            envelope_id,
            document_id,
            output,
        } => {
            let path = signing::document_download_path(&workspace, &envelope_id, &document_id)
                .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?;
            stream_download(ctx, &path, &output, "source document").await
        }
        SignDocumentCommands::Preview {
            workspace,
            envelope_id,
            document_id,
            output,
        } => {
            let path = signing::document_preview_path(&workspace, &envelope_id, &document_id)
                .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?;
            stream_download(ctx, &path, &output, "document preview").await
        }
        SignDocumentCommands::SignedDownload {
            workspace,
            envelope_id,
            document_id,
            output,
        } => {
            let path =
                signing::signed_document_download_path(&workspace, &envelope_id, &document_id)
                    .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?;
            stream_download(ctx, &path, &output, "signed document").await
        }
    }
}

// ─── Audit download ───────────────────────────────────────────────────────────

async fn execute_audit(command: SignAuditCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        SignAuditCommands::Download {
            workspace,
            envelope_id,
            output,
        } => {
            let path = signing::audit_download_path(&workspace, &envelope_id)
                .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?;
            stream_download(ctx, &path, &output, "audit certificate").await
        }
    }
}

/// Stream a signing binary/JSON artifact to `output` via the Phase-0 streaming
/// helper, mapping the error through [`map_signing_error`] with the per-artifact
/// [`SignOp`] (see [`download_ctx`]) so a `404` (live not-ready codes
/// `1609`/`128301`/`146422`) is only reframed as "not ready yet" for the async
/// signed/audit artifacts. NEVER buffers (signed PDFs and audit certs can be
/// large) and NEVER routes a signing node id through `/storage/{node}/read/`
/// (`signing.txt:155`).
async fn stream_download(
    ctx: &CommandContext<'_>,
    api_path: &str,
    output: &str,
    what: &str,
) -> Result<()> {
    let client = ctx.build_client()?;
    let out = Path::new(output);
    let (ctx_str, op) = download_ctx(what);
    let bytes = client
        .download_file_stream(api_path, out)
        .await
        .map_err(|e| map_signing_error(e, ctx_str, op))?;
    if !ctx.output.quiet {
        eprintln!("downloaded {what} ({bytes} bytes) to '{output}'");
    }
    Ok(())
}

/// Map a download artifact label to its `'static` error context AND its
/// [`SignOp`] discriminator.
///
/// [`map_signing_error`] takes a `&'static str` context, so the per-artifact
/// label is mapped to a fixed string rather than a formatted (non-`'static`)
/// one. The discriminator decides whether a `404` (live not-ready codes
/// `1609`/`128301`/`146422`) is reframed as "not ready yet" and, if so, which
/// poll target the guidance names: only the SIGNED PDF
/// ([`SignOp::SignedFetch`] — available once the envelope completes) and the
/// AUDIT certificate ([`SignOp::AuditFetch`] — available once the envelope
/// reaches a terminal state) are generated asynchronously. A SOURCE-document
/// download is a plain fetch ([`SignOp::General`]) — a `404` there is a genuine
/// not-found (typo'd id / wrong workspace/envelope/document id,
/// `signing.txt:591`), not "not ready".
///
/// (Despite the historical name, this never `Box::leak`s — it returns a string
/// literal, which is already `'static`.)
fn download_ctx(what: &str) -> (&'static str, SignOp) {
    match what {
        "source document" => ("failed to download source document", SignOp::General),
        // The preview returns the SAME source bytes as the download — a plain
        // fetch, so a 404 is a genuine not-found, not "not ready" (F25).
        "document preview" => ("failed to preview document", SignOp::General),
        "signed document" => ("failed to download signed document", SignOp::SignedFetch),
        "audit certificate" => ("failed to download audit certificate", SignOp::AuditFetch),
        _ => ("failed to download", SignOp::General),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_json_value_literal_and_escape() {
        assert_eq!(
            resolve_json_value(r#"{"a":1}"#, "x").unwrap(),
            json!({"a": 1})
        );
        // `@@` escapes a literal leading `@` (the rest must still be valid JSON).
        assert_eq!(resolve_json_value("@@\"lit\"", "x").unwrap(), json!("lit"));
    }

    #[test]
    fn resolve_json_value_rejects_malformed() {
        assert!(resolve_json_value("{not json", "x").is_err());
    }

    #[test]
    fn resolve_opt_json_array_rejects_non_array() {
        assert!(resolve_opt_json_array(Some("{}"), "docs").is_err());
        assert!(
            resolve_opt_json_array(Some("[1,2]"), "docs")
                .unwrap()
                .is_some()
        );
        assert!(resolve_opt_json_array(None, "docs").unwrap().is_none());
    }

    #[test]
    fn parse_documents_maps_fields() {
        let items = vec![json!({
            "id": "keep-1",
            "source_node_id": "node-1",
            "source_version_id": "v1",
            "display_order": 2
        })];
        let docs = parse_documents(items).unwrap();
        assert_eq!(docs[0].id.as_deref(), Some("keep-1"));
        assert_eq!(docs[0].source_node_id.as_deref(), Some("node-1"));
        assert_eq!(docs[0].display_order, Some(2));
    }

    #[test]
    fn parse_fields_uses_type_key_and_stringifies_value_json() {
        let items = vec![json!({
            "recipient_email": "a@b.com",
            "document_index": 0,
            "page": 1,
            "x_norm": 0.5,
            "type": "signature",
            "required": true,
            "value_json": {"value": "x"}
        })];
        let fields = parse_fields(items).unwrap();
        assert_eq!(fields[0].field_type.as_deref(), Some("signature"));
        assert_eq!(fields[0].x_norm, Some(0.5));
        assert_eq!(fields[0].required, Some(true));
        // An object value_json is preserved as a JSON string.
        assert_eq!(fields[0].value_json.as_deref(), Some(r#"{"value":"x"}"#));
    }

    #[test]
    fn build_create_simple_path_single_signer() {
        let p = build_create_params(
            Some("Doc".to_owned()),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("node-1".to_owned()),
            None,
            Some("signer@example.com".to_owned()),
            Some("Alex".to_owned()),
            Some("email_otp".to_owned()),
        )
        .unwrap();
        assert_eq!(p.documents.len(), 1);
        assert_eq!(p.documents[0].source_node_id.as_deref(), Some("node-1"));
        assert_eq!(p.recipients.len(), 1);
        assert_eq!(p.recipients[0].email.as_deref(), Some("signer@example.com"));
        assert_eq!(p.recipients[0].role.as_deref(), Some("signer"));
        assert_eq!(p.recipients[0].auth_method.as_deref(), Some("email_otp"));
        assert!(p.validate().is_ok());
    }

    #[test]
    fn build_create_body_json_path() {
        let body = r#"{
            "name": "MSA",
            "documents": [{"source_node_id": "n1", "display_order": 0}],
            "recipients": [{"email": "a@b.com", "role": "signer", "routing_order": 1}],
            "fields": [{"recipient_email": "a@b.com", "document_index": 0, "type": "signature"}]
        }"#;
        let p = build_create_params(
            None,
            None,
            Some(body),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(p.name.as_deref(), Some("MSA"));
        assert_eq!(p.documents.len(), 1);
        assert_eq!(p.recipients.len(), 1);
        assert_eq!(p.fields.len(), 1);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn build_create_missing_docs_and_recipients_errs() {
        // No JSON, no simple flags → error before any network call.
        assert!(
            build_create_params(
                None, None, None, None, None, None, None, None, None, None, None, None,
            )
            .is_err()
        );
    }

    #[test]
    fn build_update_none_leaves_sets_unchanged() {
        let p = build_update_params(Some("x".to_owned()), None, None, None, None, None).unwrap();
        assert!(p.documents.is_none());
        assert!(p.recipients.is_none());
        assert!(!p.is_empty());
    }

    // ─── FIX 1: empty recipient replace on update rejected pre-network ─────────

    #[test]
    fn build_update_empty_recipients_rejected_by_validate() {
        // `--recipients-json []` builds a Some(empty) recipient replace; the
        // validate() guard must reject it before any update is sent
        // (signing.txt:358).
        let p = build_update_params(None, None, None, None, Some("[]"), None).unwrap();
        assert!(
            p.recipients.as_deref().is_some_and(<[_]>::is_empty),
            "an empty recipients_json must build Some(empty), not None"
        );
        assert!(p.validate().is_err(), "empty recipient replace must fail");
        // A non-empty replace still validates.
        let ok = build_update_params(
            None,
            None,
            None,
            None,
            Some(r#"[{"email":"a@b.com"}]"#),
            None,
        )
        .unwrap();
        assert!(ok.validate().is_ok());
    }

    // ─── FIX 3: present-but-mistyped JSON fields are rejected, not dropped ─────

    #[test]
    fn parse_fields_rejects_non_numeric_coordinate() {
        // A present-but-non-numeric x_norm must be an error, not a silent drop
        // that places the field at a bogus position.
        let items = vec![json!({
            "recipient_email": "a@b.com",
            "x_norm": "abc"
        })];
        assert!(parse_fields(items).is_err());
    }

    #[test]
    fn parse_fields_rejects_non_bool_required() {
        let items = vec![json!({"recipient_email": "a@b.com", "required": "yes"})];
        assert!(parse_fields(items).is_err());
    }

    #[test]
    fn parse_recipients_rejects_non_string_email() {
        let items = vec![json!({"email": 42})];
        assert!(parse_recipients(items).is_err());
    }

    #[test]
    fn parse_documents_rejects_non_numeric_display_order() {
        let items = vec![json!({"source_node_id": "n1", "display_order": "two"})];
        assert!(parse_documents(items).is_err());
    }

    /// A non-object array element (`[1]`, `[null]`, `["x"]`, `[{},1]`) must be
    /// rejected by each parser, naming the array + index — never accepted as an
    /// EMPTY spec that would silently pass the recipients-required guard and ship
    /// garbage to the server (MEDIUM A).
    #[test]
    fn parsers_reject_non_object_array_elements() {
        for bad in [json!(1), json!(null), json!("x"), json!([1, 2])] {
            let err = parse_recipients(vec![bad.clone()]).unwrap_err().to_string();
            assert!(
                err.contains("recipients[0] must be a JSON object"),
                "recipients should reject {bad}: {err}"
            );
            let err = parse_documents(vec![bad.clone()]).unwrap_err().to_string();
            assert!(
                err.contains("documents[0] must be a JSON object"),
                "documents should reject {bad}: {err}"
            );
            let err = parse_fields(vec![bad.clone()]).unwrap_err().to_string();
            assert!(
                err.contains("fields[0] must be a JSON object"),
                "fields should reject {bad}: {err}"
            );
        }
        // A valid first element followed by a malformed one is rejected at the
        // offending index, not silently truncated.
        let mixed = vec![json!({"email": "a@b.com"}), json!(1)];
        let err = parse_recipients(mixed).unwrap_err().to_string();
        assert!(
            err.contains("recipients[1] must be a JSON object"),
            "mixed array should name index 1: {err}"
        );
    }

    #[test]
    fn field_helpers_accept_absent_and_string_encoded() {
        // Absent optional keys remain None; string-encoded numbers still parse.
        let v = json!({"x_norm": "0.5", "display_order": "3"});
        assert_eq!(f64_field(&v, "x_norm").unwrap(), Some(0.5));
        assert_eq!(u64_field(&v, "display_order").unwrap(), Some(3));
        assert_eq!(f64_field(&v, "missing").unwrap(), None);
        assert_eq!(str_field(&v, "missing").unwrap(), None);
        assert_eq!(bool_field(&v, "missing").unwrap(), None);
    }

    // ─── FIX 4: non-object body_json / policy_json rejected ────────────────────

    #[test]
    fn create_body_json_non_object_rejected() {
        // body_json is documented as an OBJECT (signing.txt:291).
        assert!(create_params_from_body(&json!([1, 2, 3])).is_err());
        assert!(create_params_from_body(&json!("scalar")).is_err());
        assert!(create_params_from_body(&json!(null)).is_err());
    }

    #[test]
    fn create_body_json_non_object_policy_rejected() {
        let body = json!({
            "documents": [{"source_node_id": "n1", "display_order": 0}],
            "recipients": [{"email": "a@b.com"}],
            "policy_json": [1, 2]
        });
        assert!(create_params_from_body(&body).is_err());
    }

    #[test]
    fn resolve_opt_json_object_rejects_non_object() {
        assert!(resolve_opt_json_object(Some("[1,2]"), "policy").is_err());
        assert!(resolve_opt_json_object(Some("42"), "policy").is_err());
        assert!(
            resolve_opt_json_object(Some(r#"{"a":1}"#), "policy")
                .unwrap()
                .is_some()
        );
        assert!(resolve_opt_json_object(None, "policy").unwrap().is_none());
    }

    // ─── error mapping ───────────────────────────────────────────────────────

    fn api_err(code: u32, http_status: u16) -> CliError {
        CliError::Api(fastio_cli::error::ApiError::new(
            code,
            None,
            "boom".to_owned(),
            http_status,
        ))
    }

    #[test]
    fn map_artifact_fetch_404_and_1609_say_not_ready() {
        // Only a signed/audit ARTIFACT fetch reframes 404/1609 as "not ready".
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            let m = map_signing_error(api_err(0, 404), "download", op).to_string();
            assert!(m.contains("not ready yet"), "got ({op:?}): {m}");
            // 1609 with a non-404 status still reframes (keyed on code, not status).
            let m = map_signing_error(api_err(1609, 200), "download", op).to_string();
            assert!(m.contains("not ready yet"), "got ({op:?}): {m}");
        }
    }

    #[test]
    fn map_send_precondition_201134_names_the_gap_and_keys_on_code() {
        // F8: a /send precondition failure (201134) gets a hint that names the
        // likely gap (a signer with no field) and steers callers to branch on the
        // numeric code, not the (server-reworded) message text.
        let m = map_signing_error(
            api_err(201_134, 422),
            "failed to send sign envelope",
            SignOp::General,
        )
        .to_string();
        assert!(m.contains("201134"), "must cite the code: {m}");
        assert!(
            m.contains("not ready to send") && m.contains("field"),
            "must name the send-precondition gap: {m}"
        );
        assert!(
            m.contains("Branch on the code"),
            "must steer callers off the message text: {m}"
        );
    }

    #[test]
    fn update_declarative_clear_note_fires_only_for_omitted_fields() {
        // F5: expires_at / policy_json are declarative — omitting one clears it.
        // The note names exactly the omitted field(s); None when both supplied.
        assert!(update_declarative_clear_note(true, true).is_none());
        let exp = update_declarative_clear_note(false, true).expect("expiry omitted → note");
        assert!(
            exp.contains("--expires-at") && !exp.contains("--policy-json"),
            "{exp}"
        );
        let pol = update_declarative_clear_note(true, false).expect("policy omitted → note");
        assert!(
            pol.contains("--policy-json") && !pol.contains("--expires-at"),
            "{pol}"
        );
        let both = update_declarative_clear_note(false, false).expect("both omitted → note");
        assert!(
            both.contains("--expires-at") && both.contains("--policy-json"),
            "{both}"
        );
        assert!(
            both.contains("declarative") && both.contains("sign envelope get"),
            "must be actionable: {both}"
        );
    }

    #[test]
    fn map_artifact_not_ready_wording_is_artifact_appropriate() {
        // Item 2: the signed-PDF not-ready guidance steers to "completes";
        // the audit-certificate guidance steers to "terminal state". The
        // signed message must NOT promise a terminal state (signed PDFs exist
        // only on completion, never on a void), and the audit message must NOT
        // narrow to "completes" (audit certs exist on any terminal state —
        // completed/declined/voided/expired/failed).
        let signed =
            map_signing_error(api_err(0, 404), "download", SignOp::SignedFetch).to_string();
        assert!(
            signed.contains("completes") && signed.contains("signed document"),
            "signed not-ready must steer to completion: {signed}"
        );
        assert!(
            !signed.contains("terminal state"),
            "signed not-ready must not promise a terminal state: {signed}"
        );
        let audit = map_signing_error(api_err(0, 404), "download", SignOp::AuditFetch).to_string();
        assert!(
            audit.contains("terminal state") && audit.contains("audit certificate"),
            "audit not-ready must steer to a terminal state: {audit}"
        );
        assert!(
            audit.contains("voided"),
            "audit not-ready must mention voided as a valid terminal state: {audit}"
        );
    }

    #[test]
    fn map_artifact_fetch_not_ready_uses_artifact_variant_and_poll_hint() {
        // LV-1/LV-2: a not-ready artifact fetch must re-key onto
        // `CliError::ArtifactNotReady` (no `ApiError` source link → no doubled
        // block) whose rendered hint is the poll-and-retry guidance, NOT the
        // generic-404 "Verify the ID or path is correct.". Covers the live
        // signed-PDF code 146422 as well as 404/1609/128301, on BOTH artifact
        // surfaces.
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            for code in [0_u32, 1609, 128_301, 146_422] {
                let mapped = map_signing_error(api_err(code, 404), "failed to download", op);
                let cli = mapped
                    .downcast_ref::<CliError>()
                    .expect("not-ready error must remain a CliError so main's pretty path fires");
                assert!(
                    matches!(cli, CliError::ArtifactNotReady { .. }),
                    "not-ready must re-key onto ArtifactNotReady (code {code}, {op:?})"
                );
                let hint = cli.suggestion().unwrap_or_default();
                assert!(
                    !hint.contains("Verify the ID or path is correct"),
                    "not-ready hint must not be the generic-404 hint (code {code}, {op:?}): {hint}"
                );
                assert!(
                    hint.to_lowercase().contains("poll"),
                    "not-ready hint must steer to poll-and-retry (code {code}, {op:?}): {hint}"
                );
            }
        }
    }

    #[test]
    fn map_artifact_fetch_146422_non_404_status_says_not_ready() {
        // Item 3: code 146422 (signed-PDF not-ready) with a NON-404 http_status
        // must STILL map to not-ready, proving the explicit `code == 146422`
        // predicate arm rather than relying on the bare-404 arm. Covers both
        // artifact surfaces.
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            let mapped = map_signing_error(api_err(146_422, 200), "download", op);
            let m = mapped.to_string();
            assert!(
                m.contains("not ready yet"),
                "146422 with status 200 must say not ready ({op:?}): {m}"
            );
            let cli = mapped
                .downcast_ref::<CliError>()
                .expect("not-ready must remain a CliError");
            assert!(
                matches!(cli, CliError::ArtifactNotReady { .. }),
                "146422/non-404 must re-key onto ArtifactNotReady ({op:?})"
            );
        }
        // And on a General op, 146422 is NOT reframed (not the artifact surface).
        let m =
            map_signing_error(api_err(146_422, 200), "failed to get", SignOp::General).to_string();
        assert!(
            !m.contains("not ready"),
            "general 146422 must not say ready: {m}"
        );
    }

    #[test]
    fn map_general_404_does_not_say_not_ready() {
        // A 404 on get/list/source-download is a genuine not-found, so it
        // must NOT be reframed as "not ready" (signing.txt:591). It defers to
        // the shared 404 suggestion (surfaced by the render layer) instead.
        let mapped = map_signing_error(api_err(0, 404), "failed to get", SignOp::General);
        let m = mapped.to_string();
        assert!(
            !m.contains("not ready"),
            "general 404 must not say ready: {m}"
        );
        // The genuine not-found semantics live in the shared 404 suggestion the
        // render layer prints, not in the inline context chain.
        let hint = mapped
            .downcast_ref::<CliError>()
            .and_then(CliError::suggestion)
            .unwrap_or_default();
        assert!(
            hint.to_lowercase().contains("not found"),
            "general 404 must resolve to the shared not-found hint: {hint}"
        );
        let m = map_signing_error(
            api_err(1609, 404),
            "failed to download source document",
            SignOp::General,
        )
        .to_string();
        assert!(
            !m.contains("not ready"),
            "source-download 404/1609 must not say ready: {m}"
        );
    }

    #[test]
    fn map_artifact_fetch_128301_says_not_ready() {
        // Live audit-not-ready code (P3): 128301 on an artifact fetch must read
        // "not ready yet", same as 404/1609. Tested on the AuditFetch surface
        // (its native code) — SignedFetch coverage lives in the loop above.
        let m =
            map_signing_error(api_err(128_301, 404), "download", SignOp::AuditFetch).to_string();
        assert!(m.contains("not ready yet"), "got: {m}");
        // But on a General op, 128301 is not reframed (it is not the artifact
        // surface), and falls through to the operation label.
        let m =
            map_signing_error(api_err(128_301, 404), "failed to get", SignOp::General).to_string();
        assert!(
            !m.contains("not ready"),
            "general 128301 must not say ready: {m}"
        );
    }

    #[test]
    fn map_artifact_fetch_9992_404_is_removed_route_not_poll() {
        // A router-level 9992 (also HTTP 404) on an artifact fetch must NOT be
        // reframed as "not ready — poll and retry" (an agent would poll a dead
        // route forever); it must surface the removed/renamed-route framing.
        for op in [SignOp::SignedFetch, SignOp::AuditFetch] {
            let m =
                map_signing_error(api_err(9992, 404), "download signed document", op).to_string();
            assert!(
                !m.contains("not ready"),
                "9992 on {op:?} must not say not-ready/poll: {m}"
            );
            assert!(m.contains("9992"), "got ({op:?}): {m}");
            assert!(
                m.to_lowercase().contains("route") && m.to_lowercase().contains("recognize"),
                "9992 on {op:?} must flag an unrecognized/removed route: {m}"
            );
        }
    }

    #[test]
    fn map_workspace_membership_10545_overrides_generic_401() {
        // 10545 (401) is workspace-membership denied; it must override the
        // generic 401 "run fastio auth login" suggestion (the caller IS authed).
        let mapped = map_signing_error(api_err(10545, 401), "failed to list", SignOp::General);
        let m = mapped.to_string();
        assert!(m.contains("10545"), "got: {m}");
        assert!(m.to_lowercase().contains("member"), "got: {m}");
        assert!(
            !m.to_lowercase().contains("auth login"),
            "10545 must not steer to auth login: {m}"
        );
        // RENDER-LEVEL: the actual `hint:` line comes from the underlying
        // CliError's suggestion() (cli_error_render in main.rs), NOT the chain.
        // It must NOT be the misleading auth-login hint, and must carry
        // workspace-access wording.
        let hint = mapped
            .downcast_ref::<CliError>()
            .and_then(CliError::suggestion)
            .unwrap_or_default();
        assert!(
            !hint.to_lowercase().contains("auth login"),
            "10545 rendered hint must not steer to auth login: {hint}"
        );
        assert!(
            hint.to_lowercase().contains("workspace"),
            "10545 rendered hint must carry workspace-access wording: {hint}"
        );
    }

    #[test]
    fn map_envelope_access_115069() {
        let mapped = map_signing_error(api_err(115_069, 401), "failed to get", SignOp::General);
        let m = mapped.to_string();
        assert!(m.contains("115069"), "got: {m}");
        assert!(m.to_lowercase().contains("access"), "got: {m}");
        // RENDER-LEVEL: the rendered `hint:` line (CliError::suggestion) must NOT
        // be the auth-login hint and must carry resource-access wording.
        let hint = mapped
            .downcast_ref::<CliError>()
            .and_then(CliError::suggestion)
            .unwrap_or_default();
        assert!(
            !hint.to_lowercase().contains("auth login"),
            "115069 rendered hint must not steer to auth login: {hint}"
        );
        assert!(
            hint.to_lowercase().contains("access"),
            "115069 rendered hint must carry resource-access wording: {hint}"
        );
    }

    #[test]
    fn map_workspace_permission_1680_generic() {
        // 1680 (403) is a generic permission denial; it must NOT overclaim a
        // specific role (the docs disagree on which role is required).
        let m =
            map_signing_error(api_err(1680, 403), "failed to update", SignOp::General).to_string();
        assert!(m.contains("1680"), "got: {m}");
        assert!(m.to_lowercase().contains("permission"), "got: {m}");
    }

    #[test]
    fn map_plan_restricted_1670_signing_scoped() {
        // 1670 carries a signing-scoped framing pointing at org capabilities.
        let mapped = map_signing_error(api_err(1670, 403), "failed to create", SignOp::General);
        let m = mapped.to_string();
        assert!(m.contains("1670"), "got: {m}");
        assert!(
            m.contains("capabilities.signing"),
            "1670 must point at org capabilities.signing: {m}"
        );
        // The underlying CliError still resolves to the shared restricted hint
        // for the render layer.
        let cli_err = mapped
            .downcast_ref::<CliError>()
            .expect("mapped signing error must remain a CliError");
        assert_eq!(
            cli_err.suggestion(),
            Some(fastio_cli::error::HINT_RESTRICTED)
        );
    }

    #[test]
    fn map_unknown_route_9992_flags_removed_route() {
        let m = map_signing_error(api_err(9992, 404), "failed to get", SignOp::General).to_string();
        assert!(m.contains("9992"), "got: {m}");
        assert!(
            m.to_lowercase().contains("route") && m.to_lowercase().contains("recognize"),
            "9992 must flag an unrecognized/removed route: {m}"
        );
    }

    #[test]
    fn map_insufficient_credits_1685_code_specific() {
        // Keyed on the code, not the bare 412 status.
        let m = map_signing_error(api_err(1685, 412), "send", SignOp::General).to_string();
        assert!(m.contains("1685"), "got: {m}");
        assert!(m.to_lowercase().contains("credit"), "got: {m}");
    }

    #[test]
    fn map_unrelated_412_not_labeled_credits() {
        // A 412 WITHOUT code 1685 must not be mislabeled as insufficient credits.
        let m = map_signing_error(api_err(0, 412), "update", SignOp::General).to_string();
        assert!(
            !m.to_lowercase().contains("insufficient signing credits"),
            "unrelated 412 must not claim credits: {m}"
        );
    }

    #[test]
    fn map_terminal_void_1660_code_specific() {
        let m = map_signing_error(api_err(1660, 409), "void", SignOp::General).to_string();
        assert!(m.contains("1660"), "got: {m}");
        assert!(m.to_lowercase().contains("terminal"), "got: {m}");
    }

    #[test]
    fn map_unrelated_409_not_labeled_terminal() {
        // A 409 WITHOUT code 1660 must not be mislabeled as a terminal envelope.
        let m = map_signing_error(api_err(0, 409), "create", SignOp::General).to_string();
        assert!(
            !m.to_lowercase().contains("already terminal"),
            "unrelated 409 must not claim terminal: {m}"
        );
    }

    #[test]
    fn confirm_destructive_yes_proceeds() {
        assert!(confirm_destructive("x", "does y", true).is_ok());
    }

    #[test]
    fn confirm_destructive_non_interactive_without_yes_blocks() {
        // In the test harness stdin/stderr are not TTYs, so this exercises the
        // non-interactive deterministic block.
        assert!(confirm_destructive("x", "does y", false).is_err());
    }

    #[test]
    fn download_ctx_maps_label_and_signop() {
        assert_eq!(
            download_ctx("source document"),
            ("failed to download source document", SignOp::General)
        );
        // The preview is a plain source fetch → General (a 404 is genuine).
        assert_eq!(
            download_ctx("document preview"),
            ("failed to preview document", SignOp::General)
        );
        assert_eq!(
            download_ctx("signed document"),
            ("failed to download signed document", SignOp::SignedFetch)
        );
        assert_eq!(
            download_ctx("audit certificate"),
            ("failed to download audit certificate", SignOp::AuditFetch)
        );
    }

    #[test]
    fn validate_void_reason_before_prompt() {
        // FIX 7: an empty / oversize reason is rejected by the validator the
        // void flow calls BEFORE confirm_destructive.
        assert!(signing::validate_void_reason("").is_err());
        assert!(signing::validate_void_reason("   ").is_err());
        assert!(signing::validate_void_reason(&"x".repeat(1025)).is_err());
        assert!(signing::validate_void_reason("legit reason").is_ok());
    }
}
