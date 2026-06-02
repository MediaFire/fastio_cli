//! E-signature (`fastio sign`) command handlers.
//!
//! Owner/admin surface over [`fastio_cli::api::signing`]. These handlers
//! enforce the binding signing disciplines:
//!
//! - **Destructive confirmation.** `delete` and `void` are destructive, and
//!   `send` emails REAL recipients — all three require `--yes` (or an
//!   interactive y/N confirmation on a TTY) before proceeding.
//! - **`@file` JSON.** The ergonomic `--*-json` / `--body-json` arguments accept
//!   `@path` to read JSON from a file and are validated as well-formed JSON
//!   client-side before any state-changing call.
//! - **Binary downloads stream to disk.** Document source/signed PDFs and the
//!   audit certificate are streamed via
//!   [`fastio_cli::client::ApiClient::download_file_stream`] (direct-Bearer,
//!   atomic temp write) — a signing node id is NEVER routed through
//!   `/storage/{node}/read/` (`signing.txt:155`).
//! - **Error mapping.** A `404`/`1609` is surfaced as "not ready yet" ONLY on a
//!   signed/audit artifact fetch (a source-download / CRUD `404` is a genuine
//!   not-found); a `1685` is "insufficient signing credits"; a `void` on a
//!   terminal envelope (`1660`) is surfaced clearly; everything else (including
//!   `1670` restricted) defers to the shared `CliError::suggestion()` hints.

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

/// Parse a documents JSON array into [`DocumentSpec`] builders, matching the
/// `signing.txt:298-304` / `:349-352` object shape.
fn parse_documents(items: Vec<Value>) -> Result<Vec<DocumentSpec>> {
    items
        .into_iter()
        .map(|v| {
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
        .map(|v| {
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
        .map(|v| {
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
/// deterministically (so an unattended script never silently sends, voids, or
/// deletes). Mirrors the metadata/workflow `confirm_spend` shape.
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
/// `404`/`1609` ("not ready yet") until the envelope reaches the required
/// (terminal) stage (`signing.txt:520`, `:531`). For CRUD / list / get /
/// update / delete / send AND the SOURCE-document download, a `404`/`1609`
/// instead means a genuine not-found (typo'd id / wrong parent,
/// `signing.txt:591`), so the "poll and retry" framing would be misleading —
/// those callers fall through to the shared generic not-found handling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SignOp {
    /// A CRUD / list / get / update / delete / send call, or a source-document
    /// download — a `404`/`1609` is a genuine not-found here.
    General,
    /// A signed-PDF or audit-certificate fetch — a `404`/`1609` means the
    /// artifact is not ready yet, not that the row is missing.
    ArtifactFetch,
}

/// Map a signing API error to an actionable, signing-specific message.
///
/// This layer adds ONLY the signing *context* that is genuinely correct for the
/// operation; everything else defers to the shared `CliError::suggestion()`
/// hints (`error.rs`):
///
/// - `404`/`1609` is reframed as "not ready yet — poll and retry" ONLY for an
///   [`SignOp::ArtifactFetch`] (signed-download / audit-download). For
///   [`SignOp::General`] (CRUD / list / get / update / delete / send and the
///   source-document download) it is left to the shared generic not-found
///   handling — saying "not ready" there would be wrong (`signing.txt:591`).
/// - `1660` (terminal-state conflict) and `1685` (insufficient credits) are
///   keyed on the actual `error.code`, NOT on a bare `409`/`412` HTTP status,
///   so an unrelated conflict / precondition isn't mislabeled.
/// - For every unmatched code (and every non-signing error) ONLY the operation
///   label is attached as anyhow context. The render layer (`cli_error_render`
///   in `main.rs`) walks the chain and appends the shared `CliError::suggestion()`
///   (which already covers `1670` → restricted and `1685` → feature-limit) on
///   its own `hint:` line, so the hint reaches the user exactly once — this layer
///   no longer re-derives or doubles it.
fn map_signing_error(err: CliError, ctx: &'static str, op: SignOp) -> anyhow::Error {
    if let CliError::Api(api) = &err {
        // Async-artifact "not ready yet" — only correct for a signed/audit fetch.
        if op == SignOp::ArtifactFetch && (api.http_status == 404 || api.code == 1609) {
            return anyhow::Error::from(err).context(format!(
                "{ctx}: not ready yet — the signed artifact / audit certificate is not generated \
                 until the envelope reaches the required (terminal) stage. Poll the envelope \
                 (`fastio sign envelope get`) and retry once it completes."
            ));
        }
        // Code-specific framings (keyed on error.code, not bare HTTP status).
        match api.code {
            // Insufficient signing credits on /send.
            1685 => {
                return anyhow::Error::from(err).context(format!(
                    "{ctx}: insufficient signing credits to send this envelope (1685). \
                     Check your plan's signing credit balance (`fastio org billing plans`)."
                ));
            }
            // Void / transition not allowed from a terminal state.
            1660 => {
                return anyhow::Error::from(err).context(format!(
                    "{ctx}: the envelope is already terminal (completed / declined / voided / \
                     expired) and cannot be changed (1660)."
                ));
            }
            _ => {}
        }
    }
    // Everything else: attach ONLY the operation label as context. The render
    // layer (`cli_error_render` in `main.rs`) walks the anyhow chain AND appends
    // the `CliError`'s own `suggestion()` on its own `hint:` line, so an
    // unmatched code (e.g. 1670 → restricted, 1685 fallbacks) still surfaces its
    // shared hint to the user without this layer re-deriving or doubling it.
    anyhow::Error::from(err).context(ctx)
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
            parent_type,
            parent_id,
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
            let v = signing::create_envelope(&client, &parent_type, &parent_id, &params)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to create sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::List {
            parent_type,
            parent_id,
            limit,
            offset,
        } => {
            let client = ctx.build_client()?;
            let v = signing::list_envelopes(&client, &parent_type, &parent_id, limit, offset)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to list sign envelopes", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Get {
            parent_type,
            parent_id,
            envelope_id,
        } => {
            let client = ctx.build_client()?;
            let v = signing::get_envelope(&client, &parent_type, &parent_id, &envelope_id)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to get sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Update {
            parent_type,
            parent_id,
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
                "no fields to update were supplied (a draft-only PATCH needs at least one of \
                 --name / --expires-at / --policy-json / --documents-json / --recipients-json / \
                 --fields-json)"
            );
            params
                .validate()
                .map_err(|e| anyhow::Error::from(e).context("invalid update request"))?;
            let client = ctx.build_client()?;
            let v =
                signing::update_envelope(&client, &parent_type, &parent_id, &envelope_id, &params)
                    .await
                    .map_err(|e| {
                        map_signing_error(e, "failed to update sign envelope", SignOp::General)
                    })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Delete {
            parent_type,
            parent_id,
            envelope_id,
            yes,
        } => {
            confirm_destructive("sign envelope delete", "soft-deletes a draft envelope", yes)?;
            let client = ctx.build_client()?;
            let v = signing::delete_envelope(&client, &parent_type, &parent_id, &envelope_id)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to delete sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Send {
            parent_type,
            parent_id,
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
            let v = signing::send_envelope(&client, &parent_type, &parent_id, &envelope_id)
                .await
                .map_err(|e| {
                    map_signing_error(e, "failed to send sign envelope", SignOp::General)
                })?;
            ctx.output.render(&v)?;
            Ok(())
        }
        SignEnvelopeCommands::Void {
            parent_type,
            parent_id,
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
                "permanently voids the envelope (credits are NOT refunded)",
                yes,
            )?;
            let client = ctx.build_client()?;
            let v =
                signing::void_envelope(&client, &parent_type, &parent_id, &envelope_id, &reason)
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

/// Build [`UpdateEnvelopeParams`] from the update flags. A `None` documents/
/// recipients/fields argument leaves that set unchanged.
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
            parent_type,
            parent_id,
            envelope_id,
            document_id,
            output,
        } => {
            let path = signing::document_download_path(
                &parent_type,
                &parent_id,
                &envelope_id,
                &document_id,
            )
            .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?;
            stream_download(ctx, &path, &output, "source document").await
        }
        SignDocumentCommands::SignedDownload {
            parent_type,
            parent_id,
            envelope_id,
            document_id,
            output,
        } => {
            let path = signing::signed_document_download_path(
                &parent_type,
                &parent_id,
                &envelope_id,
                &document_id,
            )
            .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?;
            stream_download(ctx, &path, &output, "signed document").await
        }
    }
}

// ─── Audit download ───────────────────────────────────────────────────────────

async fn execute_audit(command: SignAuditCommands, ctx: &CommandContext<'_>) -> Result<()> {
    match command {
        SignAuditCommands::Download {
            parent_type,
            parent_id,
            envelope_id,
            output,
        } => {
            let path = signing::audit_download_path(&parent_type, &parent_id, &envelope_id)
                .map_err(|e| anyhow::Error::from(e).context("invalid download request"))?;
            stream_download(ctx, &path, &output, "audit certificate").await
        }
    }
}

/// Stream a signing binary/JSON artifact to `output` via the Phase-0 streaming
/// helper, mapping the error through [`map_signing_error`] with the per-artifact
/// [`SignOp`] (see [`download_ctx`]) so a `404`/`1609` is only reframed as "not
/// ready yet" for the async signed/audit artifacts. NEVER buffers (signed PDFs
/// and audit certs can be large) and NEVER routes a signing node id through
/// `/storage/{node}/read/` (`signing.txt:155`).
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
/// one. The discriminator decides whether a `404`/`1609` is reframed as "not
/// ready yet": only the SIGNED PDF and the AUDIT certificate are generated
/// asynchronously ([`SignOp::ArtifactFetch`]). A SOURCE-document download is a
/// plain fetch ([`SignOp::General`]) — a `404` there is a genuine not-found
/// (typo'd id / wrong parent, `signing.txt:591`), not "not ready".
///
/// (Despite the historical name, this never `Box::leak`s — it returns a string
/// literal, which is already `'static`.)
fn download_ctx(what: &str) -> (&'static str, SignOp) {
    match what {
        "source document" => ("failed to download source document", SignOp::General),
        "signed document" => ("failed to download signed document", SignOp::ArtifactFetch),
        "audit certificate" => (
            "failed to download audit certificate",
            SignOp::ArtifactFetch,
        ),
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
        // validate() guard must reject it before any PATCH (signing.txt:358).
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
        let m = map_signing_error(api_err(0, 404), "download", SignOp::ArtifactFetch).to_string();
        assert!(m.contains("not ready yet"), "got: {m}");
        let m =
            map_signing_error(api_err(1609, 200), "download", SignOp::ArtifactFetch).to_string();
        assert!(m.contains("not ready yet"), "got: {m}");
    }

    #[test]
    fn map_general_404_does_not_say_not_ready() {
        // A 404 on get/list/delete/source-download is a genuine not-found, so it
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
    fn map_restricted_1670_defers_hint_to_render_layer() {
        // 1670 has no signing-specific framing, so map_signing_error attaches
        // ONLY the operation label as context — it must NOT fold the hint into
        // the chain (the render layer in main.rs emits it on a `hint:` line).
        let mapped = map_signing_error(api_err(1670, 403), "create", SignOp::General);
        let m = mapped.to_string();
        assert!(
            !m.contains(fastio_cli::error::HINT_RESTRICTED),
            "1670 hint must NOT be folded into the chain (render layer owns it): {m}"
        );
        // The error still downcasts to the original CliError, whose own
        // suggestion() is the shared restricted hint the render layer will print.
        let cli_err = mapped
            .downcast_ref::<CliError>()
            .expect("mapped signing error must remain a CliError");
        assert_eq!(
            cli_err.suggestion(),
            Some(fastio_cli::error::HINT_RESTRICTED),
            "1670 must still resolve to the shared restricted hint via suggestion()"
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
        assert_eq!(
            download_ctx("signed document"),
            ("failed to download signed document", SignOp::ArtifactFetch)
        );
        assert_eq!(
            download_ctx("audit certificate"),
            (
                "failed to download audit certificate",
                SignOp::ArtifactFetch
            )
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
