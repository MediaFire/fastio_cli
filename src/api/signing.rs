// Justification: every `pub async fn` here returns `Result<_, CliError>` and
// fails for exactly one reason — the underlying HTTP/envelope call via
// `ApiClient` (network error, non-2xx envelope, or parse failure), already
// fully documented on `CliError`/`ApiError`. Per-function `# Errors` sections
// would be identical copies of "Returns `CliError` if the API request fails."
// that add noise without information, so the doc requirement is allowed off
// module-wide rather than satisfied with boilerplate. This is scoped to this
// builder module; the rest of the crate keeps the lint on.
#![allow(clippy::missing_errors_doc)]

//! E-Signature (SignEnvelope) API — the audit-archive signing surface.
//!
//! Maps to the owner/admin REST surface. A SignEnvelope is a Profile holding up
//! to twenty source PDFs sent to one or more recipients for electronic
//! signature; **every envelope is parented to a Workspace** and flows through a
//! small lifecycle state machine (draft → sent → in_progress → completed /
//! declined / voided / expired / failed). The former org-parented surface was
//! removed — old `/org/{id}/sign_envelopes/…` routes now 404 (router code
//! `9992`); access is gated on workspace membership plus the owning org's
//! billing-plan signing capability (resolved server-side from the workspace).
//!
//! ## Routes (action-suffixed, live-verified 2026-06-03)
//!
//! All paths hang off `/workspace/{workspace_id}/sign_envelopes/` with an
//! explicit action segment — the unsuffixed REST form (`POST .../`, `GET
//! .../{env}/`) 404s with code `9992`:
//!
//! | Op | Method + path | Response key |
//! |---|---|---|
//! | Create | `POST .../create/` (JSON) | `sign_envelope` |
//! | List | `GET .../list/` | `envelopes` |
//! | Details | `GET .../{env}/details/` | `sign_envelope` |
//! | Update | `POST .../{env}/update/` (JSON; PATCH 405) | `sign_envelope` |
//! | Send | `POST .../{env}/send/` (bodyless) | `sign_envelope` |
//! | Void | `POST .../{env}/void/` (JSON `{"reason"}`) | `sign_envelope` |
//! | Doc download/preview | `GET .../{env}/documents/{doc}/download|preview/` | bytes |
//! | Signed PDF | `GET .../{env}/documents/{doc}/signed/download/` | bytes |
//! | Audit cert | `GET .../{env}/audit/download/` | bytes |
//!
//! Source-of-truth note: `signing.txt`'s endpoint-summary table is STALE (its
//! unsuffixed routes 404 live); the suffixed matrix above is live-verified.
//! `signing.txt` remains authoritative for request/response body shapes,
//! lifecycle semantics, field meanings, and identifier formats.
//!
//! ## Encoding (JSON, not form)
//!
//! Unlike most of the Fast.io API (form-encoded), the signing surface takes
//! **`application/json`** request bodies. CRUD/lifecycle therefore route
//! through [`ApiClient::post_json`] (create / update / void) or
//! [`ApiClient::post_empty`] (bodyless send) — NOT the form helpers, and NOT
//! `patch_json` (the `/update/` route is POST-only; PATCH returns 405).
//!
//! ## Response envelope shape
//!
//! Signing responses use a **named-key** envelope, e.g.
//! `{"result": true, "sign_envelope": {…}}` for a single envelope and
//! `{"result": true, "envelopes": […]}` for a list — NOT the standard
//! `{"result": …, "response": {…}}` wrapper. The client's
//! [`ApiClient::handle_response`] only unwraps a `response` key, so for these
//! endpoints it returns the **whole envelope** (minus `current_api_version`).
//! That is the correct render target — the markdown renderer needs the
//! top-level `result` for its preamble and then emits each named key
//! (`sign_envelope` / `envelopes`) as a section. The [`extract_sign_envelope`]
//! / [`extract_sign_envelopes`] helpers are provided for callers/tests that
//! need the bare object/array.
//!
//! ## Binary downloads (stream, do NOT round-trip through storage)
//!
//! Per the signing contract + the plan's Cross-Model Addendum, document and
//! audit byte endpoints **stream the bytes directly under Bearer auth** — a
//! document's `source_node_id` / `signed_pdf_node_id` /
//! `audit_certificate_node_id` lives in the envelope's OWN private storage
//! instance and MUST NOT be routed through the generic `/storage/{node}/read/`
//! endpoint (it resolves nodes in the workspace tree and returns not-found).
//! The command layer therefore streams every download via
//! [`ApiClient::download_file_stream`] (the Phase-0 streaming helper:
//! direct-Bearer, status-based error sniff before streaming, atomic temp
//! write, no overall body timeout). The audit certificate is JSON but may be
//! large, so it is streamed to a file the same way (the streaming helper
//! correctly streams a 2xx `application/json` success body — see
//! [`ApiClient::download_file_stream`] / `client.rs` `stream_response_is_error`).
//! This module exposes the download **paths** (`*_download_path`) and the
//! command layer calls `download_file_stream`; the API module never buffers a
//! document body via `read_user_asset`.
//!
//! ## Identifier formats
//!
//! Three id kinds, all treated as opaque `String` and URL-encoded into the
//! path: the **envelope id** / **workspace id** are 19-digit numeric profile-id
//! strings; the **document id** / **recipient id** / **field id** are base32
//! `OpaqueId`s (34-char hyphenated or 29-char unhyphenated). Never parse or
//! assume structure.

use serde_json::{Value, json};

use crate::client::ApiClient;
use crate::error::CliError;

/// Hard cap on documents per envelope (`signing.txt:336`, `:627`). Exceeding it
/// is rejected at create time server-side with `1605`; this client also rejects
/// it before the network.
pub const MAX_DOCUMENTS: usize = 20;

/// Maximum byte length of a void/decline reason (`signing.txt:382`).
pub const MAX_REASON_BYTES: usize = 1024;

// ─── Workspace path ─────────────────────────────────────────────────────────

/// Build the `/workspace/{id}/sign_envelopes/` base path for an envelope
/// collection.
///
/// Every envelope is workspace-parented; the former org surface was removed.
/// `workspace_id` is URL-encoded. The `Result` return type is preserved so a
/// caller passing an empty workspace id is rejected with a clear
/// [`CliError::Config`] before any network call rather than building a
/// malformed `/workspace//sign_envelopes/` path.
///
/// # Errors
///
/// Returns [`CliError::Config`] when `workspace_id` is empty.
pub fn workspace_path(workspace_id: &str) -> Result<String, CliError> {
    if workspace_id.is_empty() {
        return Err(CliError::Config(
            "a workspace id is required for sign-envelope operations".to_owned(),
        ));
    }
    Ok(format!(
        "/workspace/{}/sign_envelopes/",
        urlencoding::encode(workspace_id)
    ))
}

/// Build the path to a single envelope's action endpoint:
/// `/workspace/{id}/sign_envelopes/{envelope_id}/{action}/`.
///
/// All single-envelope routes are action-suffixed (`details` / `update` /
/// `send` / `void`); the unsuffixed `/{envelope_id}/` form 404s with code
/// `9992`. Both ids are URL-encoded.
fn envelope_action_path(
    workspace_id: &str,
    envelope_id: &str,
    action: &str,
) -> Result<String, CliError> {
    let base = workspace_path(workspace_id)?;
    Ok(format!(
        "{base}{}/{action}/",
        urlencoding::encode(envelope_id)
    ))
}

// ─── Envelope-unwrap extractors ─────────────────────────────────────────────────

/// Pull the bare `sign_envelope` object out of a signing response envelope.
///
/// Tolerates the named-key shape `{"result": true, "sign_envelope": {…}}`
/// (create / details / update / send / void) AND the standard
/// `{"response": {"sign_envelope": {…}}}` wrapper, returning `None` when
/// neither is present.
#[must_use]
pub fn extract_sign_envelope(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("sign_envelope")
}

/// Pull the bare list of envelopes out of a signing list response.
///
/// The live list endpoint keys the array on **`envelopes`** (verified
/// 2026-06-03); this is the primary key. A legacy `sign_envelopes` key and the
/// standard `{"response": {…}}` wrapper are tolerated as fallbacks so a doc /
/// server variance does not silently drop the list. Returns `None` when no
/// recognized key is present.
#[must_use]
pub fn extract_sign_envelopes(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload
        .get("envelopes")
        .or_else(|| payload.get("sign_envelopes"))
}

// ─── Document / Recipient / Field builders ──────────────────────────────────────

/// One source document placement in a create/update request
/// (`signing.txt:298-304`, `:349-352`).
///
/// On create, supply `source_node_id` (+ optional `source_version_id` and a
/// `display_order`). On a declarative update an entry either KEEPS an existing
/// document (carry its `id`) or ADDS a new one (`source_node_id`)
/// (`signing.txt:358-362`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DocumentSpec {
    /// Existing document id to KEEP on a declarative update (`signing.txt:350`).
    pub id: Option<String>,
    /// Source storage node id to copy into the envelope (create / update-add).
    pub source_node_id: Option<String>,
    /// Pinned source version id (`signing.txt:301`).
    pub source_version_id: Option<String>,
    /// 0-based display order within the envelope.
    pub display_order: Option<u64>,
}

impl DocumentSpec {
    /// An empty spec (equivalent to [`Default::default`]). Provided so the
    /// binary crate can build this `#[non_exhaustive]` struct without
    /// struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the existing-document id to KEEP (declarative update).
    #[must_use]
    pub fn id(mut self, id: Option<String>) -> Self {
        self.id = id;
        self
    }

    /// Set the source storage node id (create / update-add).
    #[must_use]
    pub fn source_node_id(mut self, node: Option<String>) -> Self {
        self.source_node_id = node;
        self
    }

    /// Set the pinned source version id.
    #[must_use]
    pub fn source_version_id(mut self, ver: Option<String>) -> Self {
        self.source_version_id = ver;
        self
    }

    /// Set the 0-based display order.
    #[must_use]
    pub fn display_order(mut self, order: Option<u64>) -> Self {
        self.display_order = order;
        self
    }

    /// Serialize to the exact JSON object shape signing expects.
    #[must_use]
    fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(id) = &self.id {
            obj.insert("id".to_owned(), Value::String(id.clone()));
        }
        if let Some(node) = &self.source_node_id {
            obj.insert("source_node_id".to_owned(), Value::String(node.clone()));
        }
        if let Some(ver) = &self.source_version_id {
            obj.insert("source_version_id".to_owned(), Value::String(ver.clone()));
        }
        if let Some(order) = self.display_order {
            obj.insert("display_order".to_owned(), json!(order));
        }
        Value::Object(obj)
    }
}

/// One recipient in a create/update request (`signing.txt:305-313`, `:337`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RecipientSpec {
    /// Recipient email address.
    pub email: Option<String>,
    /// Human-readable display name.
    pub display_name: Option<String>,
    /// E.164 phone (REQUIRED when `auth_method=sms_otp`, `signing.txt:337`).
    pub phone_e164: Option<String>,
    /// Role: `signer` / `cc` / `viewer` / `approver` / `certified_recipient`.
    pub role: Option<String>,
    /// 1-based routing order (identical numbers run in parallel).
    pub routing_order: Option<u64>,
    /// Per-recipient auth method: `none` / `email_otp` / `sms_otp`.
    pub auth_method: Option<String>,
}

impl RecipientSpec {
    /// An empty spec (equivalent to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the recipient email address.
    #[must_use]
    pub fn email(mut self, email: Option<String>) -> Self {
        self.email = email;
        self
    }

    /// Set the display name.
    #[must_use]
    pub fn display_name(mut self, name: Option<String>) -> Self {
        self.display_name = name;
        self
    }

    /// Set the E.164 phone (required for `sms_otp`).
    #[must_use]
    pub fn phone_e164(mut self, phone: Option<String>) -> Self {
        self.phone_e164 = phone;
        self
    }

    /// Set the role.
    #[must_use]
    pub fn role(mut self, role: Option<String>) -> Self {
        self.role = role;
        self
    }

    /// Set the 1-based routing order.
    #[must_use]
    pub fn routing_order(mut self, order: Option<u64>) -> Self {
        self.routing_order = order;
        self
    }

    /// Set the auth method (`none` / `email_otp` / `sms_otp`).
    #[must_use]
    pub fn auth_method(mut self, auth: Option<String>) -> Self {
        self.auth_method = auth;
        self
    }

    /// Serialize to the exact JSON object shape signing expects.
    #[must_use]
    fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(email) = &self.email {
            obj.insert("email".to_owned(), Value::String(email.clone()));
        }
        if let Some(name) = &self.display_name {
            obj.insert("display_name".to_owned(), Value::String(name.clone()));
        }
        if let Some(phone) = &self.phone_e164 {
            obj.insert("phone_e164".to_owned(), Value::String(phone.clone()));
        }
        if let Some(role) = &self.role {
            obj.insert("role".to_owned(), Value::String(role.clone()));
        }
        if let Some(order) = self.routing_order {
            obj.insert("routing_order".to_owned(), json!(order));
        }
        if let Some(auth) = &self.auth_method {
            obj.insert("auth_method".to_owned(), Value::String(auth.clone()));
        }
        Value::Object(obj)
    }
}

/// One field placement in a create/update request (`signing.txt:315-328`).
///
/// `recipient_email` and `document_index` cross-reference the recipients /
/// documents lists by value and by 0-based position respectively
/// (`signing.txt:338`, `:362`). Coordinates are normalized to `0..1`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FieldSpec {
    /// Cross-reference to a recipient by email.
    pub recipient_email: Option<String>,
    /// 0-based index into the documents list AS SENT.
    pub document_index: Option<u64>,
    /// 1-based page number.
    pub page: Option<u64>,
    /// Top-left x in `0..1`.
    pub x_norm: Option<f64>,
    /// Top-left y in `0..1`.
    pub y_norm: Option<f64>,
    /// Bounding-box width in `0..1`.
    pub w_norm: Option<f64>,
    /// Bounding-box height in `0..1`.
    pub h_norm: Option<f64>,
    /// Field type: `signature` / `initial` / `date` / `text` / `checkbox`.
    pub field_type: Option<String>,
    /// Whether the field is required.
    pub required: Option<bool>,
    /// Optional pre-fill value as a JSON string (`signing.txt:327`).
    pub value_json: Option<String>,
}

impl FieldSpec {
    /// An empty spec (equivalent to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the recipient cross-reference email.
    #[must_use]
    pub fn recipient_email(mut self, email: Option<String>) -> Self {
        self.recipient_email = email;
        self
    }

    /// Set the 0-based document index.
    #[must_use]
    pub fn document_index(mut self, idx: Option<u64>) -> Self {
        self.document_index = idx;
        self
    }

    /// Set the 1-based page number.
    #[must_use]
    pub fn page(mut self, page: Option<u64>) -> Self {
        self.page = page;
        self
    }

    /// Set the normalized bounding box (`x_norm`, `y_norm`, `w_norm`, `h_norm`).
    #[must_use]
    pub fn bounding_box(
        mut self,
        x: Option<f64>,
        y: Option<f64>,
        w: Option<f64>,
        h: Option<f64>,
    ) -> Self {
        self.x_norm = x;
        self.y_norm = y;
        self.w_norm = w;
        self.h_norm = h;
        self
    }

    /// Set the field type (`signature` / `initial` / `date` / `text` /
    /// `checkbox`).
    #[must_use]
    pub fn field_type(mut self, ty: Option<String>) -> Self {
        self.field_type = ty;
        self
    }

    /// Set whether the field is required.
    #[must_use]
    pub fn required(mut self, required: Option<bool>) -> Self {
        self.required = required;
        self
    }

    /// Set the pre-fill value as a JSON string.
    #[must_use]
    pub fn value_json(mut self, value: Option<String>) -> Self {
        self.value_json = value;
        self
    }

    /// Serialize to the exact JSON object shape signing expects (the `type`
    /// key carries `field_type` — `type` is a Rust keyword).
    #[must_use]
    fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(email) = &self.recipient_email {
            obj.insert("recipient_email".to_owned(), Value::String(email.clone()));
        }
        if let Some(idx) = self.document_index {
            obj.insert("document_index".to_owned(), json!(idx));
        }
        if let Some(page) = self.page {
            obj.insert("page".to_owned(), json!(page));
        }
        if let Some(x) = self.x_norm {
            obj.insert("x_norm".to_owned(), json!(x));
        }
        if let Some(y) = self.y_norm {
            obj.insert("y_norm".to_owned(), json!(y));
        }
        if let Some(w) = self.w_norm {
            obj.insert("w_norm".to_owned(), json!(w));
        }
        if let Some(h) = self.h_norm {
            obj.insert("h_norm".to_owned(), json!(h));
        }
        if let Some(ty) = &self.field_type {
            obj.insert("type".to_owned(), Value::String(ty.clone()));
        }
        if let Some(req) = self.required {
            obj.insert("required".to_owned(), json!(req));
        }
        // `value_json` is documented as a JSON STRING (signing.txt:327); the
        // caller passes the already-encoded string and the server stores it
        // verbatim, so it is sent as a JSON string value (not re-parsed here).
        if let Some(v) = &self.value_json {
            obj.insert("value_json".to_owned(), Value::String(v.clone()));
        }
        Value::Object(obj)
    }
}

// ─── Create / Update param structs ──────────────────────────────────────────────

/// Parameters for creating a draft envelope (`signing.txt:291-329`).
///
/// `#[non_exhaustive]` because the create surface may grow. `policy_json` is
/// passed through as an already-parsed JSON object the caller built (often from
/// an `@file`); this module does not validate its internal shape.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CreateEnvelopeParams {
    /// Optional display name.
    pub name: Option<String>,
    /// Optional UTC auto-expiry timestamp (`null`/omitted uses the policy).
    pub expires_at: Option<String>,
    /// Optional policy bag as a JSON value (`auth_method`, reminder cadence, …).
    pub policy_json: Option<Value>,
    /// Source documents (1..=20 required at create time).
    pub documents: Vec<DocumentSpec>,
    /// Recipients (>= 1 required).
    pub recipients: Vec<RecipientSpec>,
    /// Optional field placements.
    pub fields: Vec<FieldSpec>,
}

impl CreateEnvelopeParams {
    /// An empty parameter set (equivalent to [`Default::default`]). Provided so
    /// the binary crate can build this `#[non_exhaustive]` struct without
    /// struct-literal syntax.
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

    /// Set the UTC auto-expiry timestamp.
    #[must_use]
    pub fn expires_at(mut self, expires_at: Option<String>) -> Self {
        self.expires_at = expires_at;
        self
    }

    /// Set the policy bag JSON value.
    #[must_use]
    pub fn policy_json(mut self, policy: Option<Value>) -> Self {
        self.policy_json = policy;
        self
    }

    /// Set the document list (1..=20 required at create time).
    #[must_use]
    pub fn documents(mut self, documents: Vec<DocumentSpec>) -> Self {
        self.documents = documents;
        self
    }

    /// Set the recipient list (>= 1 required).
    #[must_use]
    pub fn recipients(mut self, recipients: Vec<RecipientSpec>) -> Self {
        self.recipients = recipients;
        self
    }

    /// Set the field-placement list.
    #[must_use]
    pub fn fields(mut self, fields: Vec<FieldSpec>) -> Self {
        self.fields = fields;
        self
    }

    /// Validate the client-side caps (`signing.txt:336-337`, `:627`) BEFORE any
    /// network call: 1..=20 documents and >= 1 recipient.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Config`] when there are zero or more than
    /// [`MAX_DOCUMENTS`] documents, or zero recipients.
    pub fn validate(&self) -> Result<(), CliError> {
        if self.documents.is_empty() {
            return Err(CliError::Config(
                "a sign envelope needs at least one document".to_owned(),
            ));
        }
        if self.documents.len() > MAX_DOCUMENTS {
            return Err(CliError::Config(format!(
                "too many documents: {} (max {MAX_DOCUMENTS} per envelope)",
                self.documents.len()
            )));
        }
        if self.recipients.is_empty() {
            return Err(CliError::Config(
                "a sign envelope needs at least one recipient".to_owned(),
            ));
        }
        Ok(())
    }

    /// Serialize to the exact create-request JSON body (`signing.txt:291-329`).
    #[must_use]
    fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(name) = &self.name {
            obj.insert("name".to_owned(), Value::String(name.clone()));
        }
        if let Some(exp) = &self.expires_at {
            obj.insert("expires_at".to_owned(), Value::String(exp.clone()));
        }
        if let Some(policy) = &self.policy_json {
            obj.insert("policy_json".to_owned(), policy.clone());
        }
        obj.insert(
            "documents".to_owned(),
            Value::Array(self.documents.iter().map(DocumentSpec::to_json).collect()),
        );
        obj.insert(
            "recipients".to_owned(),
            Value::Array(self.recipients.iter().map(RecipientSpec::to_json).collect()),
        );
        if !self.fields.is_empty() {
            obj.insert(
                "fields".to_owned(),
                Value::Array(self.fields.iter().map(FieldSpec::to_json).collect()),
            );
        }
        Value::Object(obj)
    }
}

/// Parameters for updating a draft envelope (`signing.txt:344-362`).
///
/// Only `draft` envelopes are editable (a non-draft returns 403). **`recipients`
/// is REQUIRED** — an update is a full recipient replacement (≥1), so the field
/// is modeled as `Option` only to share builder ergonomics, but [`validate`]
/// rejects an absent or empty list (the live server 400s an empty update, and
/// both docs require recipients on update). `fields` is a full replacement when
/// supplied (omit to keep, `[]` to clear). `documents` is optional — omit
/// (`None`) to leave the set unchanged, or supply it for a full declarative
/// replacement (1..=20 must remain).
///
/// [`validate`]: UpdateEnvelopeParams::validate
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct UpdateEnvelopeParams {
    /// New display name (optional).
    pub name: Option<String>,
    /// New expiry (optional).
    pub expires_at: Option<String>,
    /// New policy bag (optional).
    pub policy_json: Option<Value>,
    /// `None` leaves documents unchanged; `Some(list)` is a declarative replace.
    pub documents: Option<Vec<DocumentSpec>>,
    /// REQUIRED full recipient replacement (≥1). `None`/empty is rejected by
    /// [`validate`](UpdateEnvelopeParams::validate) — an update always replaces
    /// the recipient roster (`signing.txt:358`).
    pub recipients: Option<Vec<RecipientSpec>>,
    /// `None` leaves fields unchanged; `Some(list)` is a full replace
    /// (`Some([])` clears them).
    pub fields: Option<Vec<FieldSpec>>,
}

impl UpdateEnvelopeParams {
    /// An empty parameter set (equivalent to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the new display name.
    #[must_use]
    pub fn name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    /// Set the new expiry timestamp.
    #[must_use]
    pub fn expires_at(mut self, expires_at: Option<String>) -> Self {
        self.expires_at = expires_at;
        self
    }

    /// Set the new policy bag JSON value.
    #[must_use]
    pub fn policy_json(mut self, policy: Option<Value>) -> Self {
        self.policy_json = policy;
        self
    }

    /// Set the declarative document replacement (`None` leaves it unchanged).
    #[must_use]
    pub fn documents(mut self, documents: Option<Vec<DocumentSpec>>) -> Self {
        self.documents = documents;
        self
    }

    /// Set the recipient roster (a full replacement).
    ///
    /// Recipients are REQUIRED on an update: an update always replaces the entire
    /// recipient roster, so [`UpdateEnvelopeParams::validate`] rejects a `None`
    /// or empty list. Passing `None` here therefore does NOT "leave recipients
    /// unchanged" — it fails validation before the request is sent.
    #[must_use]
    pub fn recipients(mut self, recipients: Option<Vec<RecipientSpec>>) -> Self {
        self.recipients = recipients;
        self
    }

    /// Set the full field replacement (`None` leaves it unchanged).
    #[must_use]
    pub fn fields(mut self, fields: Option<Vec<FieldSpec>>) -> Self {
        self.fields = fields;
        self
    }

    /// Validate the client-side caps that apply to an update:
    ///
    /// - **`recipients` is REQUIRED** (≥1). An update is a full recipient
    ///   replacement (`signing.txt:358`); the live server 400s an empty update,
    ///   so an absent OR empty recipients list is rejected before the network.
    /// - When `documents` is supplied it must hold 1..=20 entries
    ///   (`signing.txt:361`); a `None` document set leaves it unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Config`] when `recipients` is absent or empty, or
    /// when a supplied documents list is empty or exceeds [`MAX_DOCUMENTS`].
    pub fn validate(&self) -> Result<(), CliError> {
        // Recipients are required on update (full replacement, >= 1).
        match &self.recipients {
            None => {
                return Err(CliError::Config(
                    "an update requires recipients: supply a full recipient replacement \
                     (>= 1) — an update always replaces the recipient roster"
                        .to_owned(),
                ));
            }
            Some(recips) if recips.is_empty() => {
                return Err(CliError::Config(
                    "a recipient replace must keep at least one recipient".to_owned(),
                ));
            }
            Some(_) => {}
        }
        if let Some(docs) = &self.documents {
            if docs.is_empty() {
                return Err(CliError::Config(
                    "a declarative document replace must keep at least one document".to_owned(),
                ));
            }
            if docs.len() > MAX_DOCUMENTS {
                return Err(CliError::Config(format!(
                    "too many documents: {} (max {MAX_DOCUMENTS} per envelope)",
                    docs.len()
                )));
            }
        }
        Ok(())
    }

    /// Whether any mutable field was supplied (an empty update is rejected by
    /// the command layer rather than sent).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.expires_at.is_none()
            && self.policy_json.is_none()
            && self.documents.is_none()
            && self.recipients.is_none()
            && self.fields.is_none()
    }

    /// Serialize to the exact update-request JSON body (`signing.txt:344-362`).
    #[must_use]
    fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(name) = &self.name {
            obj.insert("name".to_owned(), Value::String(name.clone()));
        }
        if let Some(exp) = &self.expires_at {
            obj.insert("expires_at".to_owned(), Value::String(exp.clone()));
        }
        if let Some(policy) = &self.policy_json {
            obj.insert("policy_json".to_owned(), policy.clone());
        }
        // `documents` is only emitted when supplied; an absent key leaves the
        // document set unchanged (signing.txt:360).
        if let Some(docs) = &self.documents {
            obj.insert(
                "documents".to_owned(),
                Value::Array(docs.iter().map(DocumentSpec::to_json).collect()),
            );
        }
        if let Some(recips) = &self.recipients {
            obj.insert(
                "recipients".to_owned(),
                Value::Array(recips.iter().map(RecipientSpec::to_json).collect()),
            );
        }
        if let Some(fields) = &self.fields {
            obj.insert(
                "fields".to_owned(),
                Value::Array(fields.iter().map(FieldSpec::to_json).collect()),
            );
        }
        Value::Object(obj)
    }
}

/// Filters for the envelope-list endpoint (`signing.txt:182-184`,
/// `SIGN_UPDATES` §7).
///
/// All filters are optional and passed through as query parameters; the server
/// validates them. `#[non_exhaustive]` because the filter surface may grow.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ListEnvelopesParams {
    /// Lifecycle status filter, mapped to the `envelope_status` query key.
    ///
    /// A single status (e.g. `"draft"`) or a CSV of statuses (e.g.
    /// `"draft,sent,completed"`); valid values are `draft`, `sent`,
    /// `in_progress`, `completed`, `declined`, `expired`, `voided`, `failed`.
    /// Passed through verbatim — NOT enum-validated client-side, since the CSV
    /// form is a server feature and the server owns validation.
    pub envelope_status: Option<String>,
    /// `created_after` filter — a `Y-m-d H:i:s UTC` timestamp lower bound.
    pub created_after: Option<String>,
    /// `created_before` filter — a `Y-m-d H:i:s UTC` timestamp upper bound.
    pub created_before: Option<String>,
    /// Pagination limit (server-validated range; no client-side clamp).
    pub limit: Option<u32>,
    /// Pagination offset (≥0).
    pub offset: Option<u32>,
}

impl ListEnvelopesParams {
    /// An empty filter set (equivalent to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the `envelope_status` filter (single status or CSV).
    #[must_use]
    pub fn envelope_status(mut self, status: Option<String>) -> Self {
        self.envelope_status = status;
        self
    }

    /// Set the `created_after` timestamp lower bound.
    #[must_use]
    pub fn created_after(mut self, after: Option<String>) -> Self {
        self.created_after = after;
        self
    }

    /// Set the `created_before` timestamp upper bound.
    #[must_use]
    pub fn created_before(mut self, before: Option<String>) -> Self {
        self.created_before = before;
        self
    }

    /// Set the pagination limit.
    #[must_use]
    pub fn limit(mut self, limit: Option<u32>) -> Self {
        self.limit = limit;
        self
    }

    /// Set the pagination offset.
    #[must_use]
    pub fn offset(mut self, offset: Option<u32>) -> Self {
        self.offset = offset;
        self
    }

    /// Assemble the query-parameter map for the list request.
    #[must_use]
    fn to_query(&self) -> std::collections::HashMap<String, String> {
        let mut params = std::collections::HashMap::new();
        if let Some(status) = &self.envelope_status {
            params.insert("envelope_status".to_owned(), status.clone());
        }
        if let Some(after) = &self.created_after {
            params.insert("created_after".to_owned(), after.clone());
        }
        if let Some(before) = &self.created_before {
            params.insert("created_before".to_owned(), before.clone());
        }
        if let Some(l) = self.limit {
            params.insert("limit".to_owned(), l.to_string());
        }
        if let Some(o) = self.offset {
            params.insert("offset".to_owned(), o.to_string());
        }
        params
    }
}

// ─── CRUD / lifecycle ───────────────────────────────────────────────────────────

/// Create a draft envelope.
///
/// `POST /workspace/{id}/sign_envelopes/create/` (JSON body,
/// `signing.txt:288-340`). The caller-side caps are validated first via
/// [`CreateEnvelopeParams::validate`].
pub async fn create_envelope(
    client: &ApiClient,
    workspace_id: &str,
    params: &CreateEnvelopeParams,
) -> Result<Value, CliError> {
    params.validate()?;
    let base = workspace_path(workspace_id)?;
    let path = format!("{base}create/");
    client.post_json(&path, &params.to_json()).await
}

/// List envelopes for the workspace (offset-paginated, server-sorted by
/// `created` desc).
///
/// `GET /workspace/{id}/sign_envelopes/list/` with the [`ListEnvelopesParams`]
/// filters as query parameters.
pub async fn list_envelopes(
    client: &ApiClient,
    workspace_id: &str,
    params: &ListEnvelopesParams,
) -> Result<Value, CliError> {
    let base = workspace_path(workspace_id)?;
    let path = format!("{base}list/");
    let query = params.to_query();
    client.get_with_params(&path, &query).await
}

/// Get a single envelope (documents/recipients/fields inlined).
///
/// `GET /workspace/{id}/sign_envelopes/{envelope_id}/details/`.
pub async fn get_envelope(
    client: &ApiClient,
    workspace_id: &str,
    envelope_id: &str,
) -> Result<Value, CliError> {
    let path = envelope_action_path(workspace_id, envelope_id, "details")?;
    client.get(&path).await
}

/// Update mutable fields on a draft envelope (draft-only; non-draft → 403).
///
/// `POST /workspace/{id}/sign_envelopes/{envelope_id}/update/` (JSON body,
/// `signing.txt:344-362`). The route is **POST-only** — a PATCH returns 405.
pub async fn update_envelope(
    client: &ApiClient,
    workspace_id: &str,
    envelope_id: &str,
    params: &UpdateEnvelopeParams,
) -> Result<Value, CliError> {
    params.validate()?;
    let path = envelope_action_path(workspace_id, envelope_id, "update")?;
    client.post_json(&path, &params.to_json()).await
}

/// Send a draft envelope (draft → sent; idempotent retry, `signing.txt:364-371`).
///
/// `POST /workspace/{id}/sign_envelopes/{envelope_id}/send/` (no body). A second
/// `/send/` on an already-sent envelope returns success without re-emitting
/// events; insufficient credits surface as `1685` (HTTP 412).
pub async fn send_envelope(
    client: &ApiClient,
    workspace_id: &str,
    envelope_id: &str,
) -> Result<Value, CliError> {
    let path = envelope_action_path(workspace_id, envelope_id, "send")?;
    // `/send/` is a bodyless POST per the contract (`signing.txt:364-371`):
    // send no body and no `Content-Type`, matching the documented shape exactly
    // rather than posting `{}` with a JSON content-type. The send response is a
    // named-key boolean envelope (`{"result": true, …}` — the post-send envelope
    // shape, NOT a standard `response`-wrapped payload), which `post_empty`
    // preserves verbatim since there is no `response` key to unwrap.
    client.post_empty(&path).await
}

/// Validate a void/decline `reason` against the contract caps
/// (`signing.txt:382`): it must be non-blank and at most [`MAX_REASON_BYTES`].
///
/// Exposed so a caller (e.g. the CLI void flow) can reject a bad reason BEFORE
/// prompting for destructive confirmation, rather than prompting and only then
/// failing. [`void_envelope`] calls this as its client-side guard.
///
/// # Errors
///
/// Returns [`CliError::Config`] when `reason` is blank or exceeds
/// [`MAX_REASON_BYTES`].
pub fn validate_void_reason(reason: &str) -> Result<(), CliError> {
    if reason.trim().is_empty() {
        return Err(CliError::Config(
            "void requires a non-empty --reason".to_owned(),
        ));
    }
    if reason.len() > MAX_REASON_BYTES {
        return Err(CliError::Config(format!(
            "void reason is too long: {} bytes (max {MAX_REASON_BYTES})",
            reason.len()
        )));
    }
    Ok(())
}

/// Void a non-terminal envelope (terminal → `1660`, `signing.txt:373-382`).
///
/// `POST /workspace/{id}/sign_envelopes/{envelope_id}/void/` (JSON body
/// `{"reason": …}`). `reason` is REQUIRED and capped at [`MAX_REASON_BYTES`];
/// the cap is enforced client-side (via [`validate_void_reason`]) before the
/// network. Credits are not refunded.
///
/// # Errors
///
/// Returns [`CliError::Config`] when `reason` is blank or exceeds
/// [`MAX_REASON_BYTES`], in addition to the usual API errors.
pub async fn void_envelope(
    client: &ApiClient,
    workspace_id: &str,
    envelope_id: &str,
    reason: &str,
) -> Result<Value, CliError> {
    validate_void_reason(reason)?;
    let path = envelope_action_path(workspace_id, envelope_id, "void")?;
    client.post_json(&path, &json!({ "reason": reason })).await
}

// ─── Download paths (streamed by the command layer) ─────────────────────────────

/// Build the path to a document's byte endpoint:
/// `/workspace/{id}/sign_envelopes/{env}/documents/{doc}/{suffix}`.
///
/// `suffix` is the trailing route segment(s) (`download/`, `preview/`,
/// `signed/download/`). Both ids are URL-encoded.
fn document_path(
    workspace_id: &str,
    envelope_id: &str,
    document_id: &str,
    suffix: &str,
) -> Result<String, CliError> {
    let base = workspace_path(workspace_id)?;
    Ok(format!(
        "{base}{}/documents/{}/{suffix}",
        urlencoding::encode(envelope_id),
        urlencoding::encode(document_id)
    ))
}

/// Build the source-PDF download path for one document.
///
/// `GET /workspace/{id}/sign_envelopes/{env}/documents/{doc}/download/`. The
/// caller streams it via [`ApiClient::download_file_stream`] — do NOT route the
/// document's `source_node_id` through `/storage/{node}/read/` (`signing.txt:155`).
pub fn document_download_path(
    workspace_id: &str,
    envelope_id: &str,
    document_id: &str,
) -> Result<String, CliError> {
    document_path(workspace_id, envelope_id, document_id, "download/")
}

/// Build the source-PDF preview path for one document.
///
/// `GET /workspace/{id}/sign_envelopes/{env}/documents/{doc}/preview/`. Returns
/// the same source PDF bytes as `download` (served for in-app rendering rather
/// than as an attachment); streamed by the command layer the same way.
pub fn document_preview_path(
    workspace_id: &str,
    envelope_id: &str,
    document_id: &str,
) -> Result<String, CliError> {
    document_path(workspace_id, envelope_id, document_id, "preview/")
}

/// Build the signed-PDF download path for one document.
///
/// `GET /workspace/{id}/sign_envelopes/{env}/documents/{doc}/signed/download/`.
/// Returns HTTP 404 (live code `146422`, historically `1609`) until the
/// envelope completes (`signing.txt:520`); `403` until the envelope is fully
/// completed.
pub fn signed_document_download_path(
    workspace_id: &str,
    envelope_id: &str,
    document_id: &str,
) -> Result<String, CliError> {
    document_path(workspace_id, envelope_id, document_id, "signed/download/")
}

/// Build the audit-certificate download path for an envelope.
///
/// `GET /workspace/{id}/sign_envelopes/{env}/audit/download/`. The certificate
/// is JSON but may be large, so the command layer streams it to a file via
/// [`ApiClient::download_file_stream`] (the streaming helper correctly streams a
/// 2xx `application/json` success body). Returns HTTP 404 (live code `128301`,
/// historically `1609`) until the envelope reaches a terminal state and the
/// certificate is rendered (`signing.txt:531`).
pub fn audit_download_path(workspace_id: &str, envelope_id: &str) -> Result<String, CliError> {
    envelope_action_path(workspace_id, envelope_id, "audit/download")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─── workspace_path validation ──────────────────────────────────────────

    #[test]
    fn workspace_path_builds_base() {
        assert_eq!(
            workspace_path("4011234567890123456").unwrap(),
            "/workspace/4011234567890123456/sign_envelopes/"
        );
    }

    #[test]
    fn workspace_path_rejects_empty_id() {
        let err = workspace_path("").unwrap_err();
        assert!(
            matches!(err, CliError::Config(_)),
            "expected Config error for an empty workspace id, got {err:?}"
        );
    }

    #[test]
    fn workspace_path_urlencodes_the_id() {
        assert_eq!(
            workspace_path("a/b c").unwrap(),
            "/workspace/a%2Fb%20c/sign_envelopes/"
        );
    }

    // ─── path builders (exact suffixed routes + encoding) ─────────────────────

    #[test]
    fn action_paths_build_details_update_send_void() {
        assert_eq!(
            envelope_action_path("ws1", "env/1", "details").unwrap(),
            "/workspace/ws1/sign_envelopes/env%2F1/details/"
        );
        assert_eq!(
            envelope_action_path("ws1", "env1", "update").unwrap(),
            "/workspace/ws1/sign_envelopes/env1/update/"
        );
        assert_eq!(
            envelope_action_path("ws1", "env1", "send").unwrap(),
            "/workspace/ws1/sign_envelopes/env1/send/"
        );
        assert_eq!(
            envelope_action_path("ws1", "env1", "void").unwrap(),
            "/workspace/ws1/sign_envelopes/env1/void/"
        );
    }

    #[test]
    fn create_and_list_routes_are_exact_suffixed_strings() {
        // `create_envelope` / `list_envelopes` build their paths inline as
        // `{workspace_path}create/` and `{workspace_path}list/`. Lock the exact
        // full route strings so a regression to an unsuffixed (9992-prone) route
        // is caught (F12).
        let base = workspace_path("4011234567890123456").unwrap();
        assert_eq!(
            format!("{base}create/"),
            "/workspace/4011234567890123456/sign_envelopes/create/"
        );
        assert_eq!(
            format!("{base}list/"),
            "/workspace/4011234567890123456/sign_envelopes/list/"
        );
        // The workspace id is URL-encoded into the base for these routes too.
        let enc = workspace_path("a/b c").unwrap();
        assert_eq!(
            format!("{enc}create/"),
            "/workspace/a%2Fb%20c/sign_envelopes/create/"
        );
        assert_eq!(
            format!("{enc}list/"),
            "/workspace/a%2Fb%20c/sign_envelopes/list/"
        );
    }

    #[test]
    fn download_paths_build_and_encode() {
        assert_eq!(
            document_download_path("ws1", "env1", "doc-a-b").unwrap(),
            "/workspace/ws1/sign_envelopes/env1/documents/doc-a-b/download/"
        );
        assert_eq!(
            document_preview_path("ws1", "env1", "doc-a-b").unwrap(),
            "/workspace/ws1/sign_envelopes/env1/documents/doc-a-b/preview/"
        );
        assert_eq!(
            signed_document_download_path("ws1", "env1", "doc 1").unwrap(),
            "/workspace/ws1/sign_envelopes/env1/documents/doc%201/signed/download/"
        );
        assert_eq!(
            audit_download_path("ws1", "env1").unwrap(),
            "/workspace/ws1/sign_envelopes/env1/audit/download/"
        );
    }

    #[test]
    fn download_paths_reject_empty_workspace() {
        assert!(document_download_path("", "env1", "doc1").is_err());
        assert!(document_preview_path("", "env1", "doc1").is_err());
        assert!(audit_download_path("", "env1").is_err());
    }

    // ─── list-filter query assembly (order-independent) ──────────────────────

    #[test]
    fn list_params_to_query_maps_status_to_envelope_status() {
        let q = ListEnvelopesParams::new()
            .envelope_status(Some("draft,sent".to_owned()))
            .created_after(Some("2026-06-01 00:00:00 UTC".to_owned()))
            .created_before(Some("2026-06-30 23:59:59 UTC".to_owned()))
            .limit(Some(25))
            .offset(Some(10))
            .to_query();
        // `--status` → query key `envelope_status` (F22), CSV passthrough.
        assert_eq!(
            q.get("envelope_status").map(String::as_str),
            Some("draft,sent")
        );
        assert_eq!(
            q.get("created_after").map(String::as_str),
            Some("2026-06-01 00:00:00 UTC")
        );
        assert_eq!(
            q.get("created_before").map(String::as_str),
            Some("2026-06-30 23:59:59 UTC")
        );
        assert_eq!(q.get("limit").map(String::as_str), Some("25"));
        assert_eq!(q.get("offset").map(String::as_str), Some("10"));
    }

    #[test]
    fn list_params_to_query_omits_absent_filters() {
        let q = ListEnvelopesParams::new().limit(Some(5)).to_query();
        assert_eq!(q.get("limit").map(String::as_str), Some("5"));
        assert!(!q.contains_key("envelope_status"));
        assert!(!q.contains_key("created_after"));
        assert!(!q.contains_key("created_before"));
        assert!(!q.contains_key("offset"));
        assert!(ListEnvelopesParams::new().to_query().is_empty());
    }

    // ─── Param → JSON shape (signing.txt:291-329) ─────────────────────────────

    #[test]
    fn create_params_serialize_to_documented_shape() {
        let params = CreateEnvelopeParams {
            name: Some("Master Services Agreement".to_owned()),
            expires_at: Some("2026-06-15 14:30:00 UTC".to_owned()),
            policy_json: Some(json!({"auth_method": "email_otp", "reminder_cadence_hours": 24})),
            documents: vec![DocumentSpec {
                source_node_id: Some("f3jm5-zqzfx".to_owned()),
                source_version_id: Some("v1abc".to_owned()),
                display_order: Some(0),
                ..Default::default()
            }],
            recipients: vec![RecipientSpec {
                email: Some("signer@example.com".to_owned()),
                display_name: Some("Alex Signer".to_owned()),
                phone_e164: Some("+15555550123".to_owned()),
                role: Some("signer".to_owned()),
                routing_order: Some(1),
                auth_method: Some("email_otp".to_owned()),
            }],
            fields: vec![FieldSpec {
                recipient_email: Some("signer@example.com".to_owned()),
                document_index: Some(0),
                page: Some(1),
                x_norm: Some(0.5),
                y_norm: Some(0.5),
                w_norm: Some(0.2),
                h_norm: Some(0.05),
                field_type: Some("signature".to_owned()),
                required: Some(true),
                value_json: None,
            }],
        };
        let body = params.to_json();
        // Top-level keys per signing.txt:291-329.
        assert_eq!(body["name"], json!("Master Services Agreement"));
        assert_eq!(body["expires_at"], json!("2026-06-15 14:30:00 UTC"));
        assert_eq!(body["policy_json"]["auth_method"], json!("email_otp"));
        // documents[0]
        let doc = &body["documents"][0];
        assert_eq!(doc["source_node_id"], json!("f3jm5-zqzfx"));
        assert_eq!(doc["source_version_id"], json!("v1abc"));
        assert_eq!(doc["display_order"], json!(0));
        // recipients[0]
        let rec = &body["recipients"][0];
        assert_eq!(rec["email"], json!("signer@example.com"));
        assert_eq!(rec["phone_e164"], json!("+15555550123"));
        assert_eq!(rec["routing_order"], json!(1));
        assert_eq!(rec["auth_method"], json!("email_otp"));
        // fields[0] — the `type` key carries field_type.
        let fld = &body["fields"][0];
        assert_eq!(fld["recipient_email"], json!("signer@example.com"));
        assert_eq!(fld["document_index"], json!(0));
        assert_eq!(fld["type"], json!("signature"));
        assert_eq!(fld["x_norm"], json!(0.5));
        assert_eq!(fld["required"], json!(true));
        // value_json omitted when None.
        assert!(fld.get("value_json").is_none());
    }

    #[test]
    fn field_value_json_serializes_as_string() {
        let f = FieldSpec {
            value_json: Some(r#"{"value":"x"}"#.to_owned()),
            ..Default::default()
        };
        let v = f.to_json();
        assert_eq!(v["value_json"], json!(r#"{"value":"x"}"#));
    }

    #[test]
    fn update_omits_documents_when_none_but_includes_recipients_replace() {
        let params = UpdateEnvelopeParams {
            recipients: Some(vec![RecipientSpec {
                email: Some("a@b.com".to_owned()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let body = params.to_json();
        // documents absent → set unchanged (signing.txt:360).
        assert!(body.get("documents").is_none());
        // recipients present → full replace.
        assert!(body["recipients"].is_array());
        assert_eq!(body["recipients"][0]["email"], json!("a@b.com"));
    }

    #[test]
    fn update_documents_declarative_keep_and_add() {
        let params = UpdateEnvelopeParams {
            documents: Some(vec![
                DocumentSpec {
                    id: Some("existing-doc".to_owned()),
                    display_order: Some(0),
                    ..Default::default()
                },
                DocumentSpec {
                    source_node_id: Some("new-node".to_owned()),
                    display_order: Some(1),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let body = params.to_json();
        assert_eq!(body["documents"][0]["id"], json!("existing-doc"));
        assert_eq!(body["documents"][1]["source_node_id"], json!("new-node"));
    }

    // ─── client-side caps ─────────────────────────────────────────────────────

    #[test]
    fn create_validate_rejects_zero_documents() {
        let params = CreateEnvelopeParams {
            recipients: vec![RecipientSpec::new()],
            ..Default::default()
        };
        assert!(matches!(params.validate(), Err(CliError::Config(_))));
    }

    #[test]
    fn create_validate_rejects_over_twenty_documents() {
        let params = CreateEnvelopeParams {
            documents: vec![DocumentSpec::new(); MAX_DOCUMENTS + 1],
            recipients: vec![RecipientSpec::new()],
            ..Default::default()
        };
        assert!(matches!(params.validate(), Err(CliError::Config(_))));
    }

    #[test]
    fn create_validate_rejects_zero_recipients() {
        let params = CreateEnvelopeParams {
            documents: vec![DocumentSpec::new()],
            ..Default::default()
        };
        assert!(matches!(params.validate(), Err(CliError::Config(_))));
    }

    #[test]
    fn create_validate_accepts_exactly_twenty_documents() {
        let params = CreateEnvelopeParams {
            documents: vec![DocumentSpec::new(); MAX_DOCUMENTS],
            recipients: vec![RecipientSpec::new()],
            ..Default::default()
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn update_validate_rejects_empty_or_oversize_document_replace() {
        // Recipients are supplied so the document-replace check is what fails.
        let empty = UpdateEnvelopeParams {
            documents: Some(vec![]),
            recipients: Some(vec![RecipientSpec::new()]),
            ..Default::default()
        };
        assert!(matches!(empty.validate(), Err(CliError::Config(_))));
        let over = UpdateEnvelopeParams {
            documents: Some(vec![DocumentSpec::new(); MAX_DOCUMENTS + 1]),
            recipients: Some(vec![RecipientSpec::new()]),
            ..Default::default()
        };
        assert!(matches!(over.validate(), Err(CliError::Config(_))));
    }

    #[test]
    fn update_validate_requires_recipients() {
        // Recipients are REQUIRED on update (full replacement, >= 1; F5). An
        // absent OR empty list is rejected before the network.
        let absent = UpdateEnvelopeParams {
            name: Some("x".to_owned()),
            ..Default::default()
        };
        assert!(
            matches!(absent.validate(), Err(CliError::Config(_))),
            "an update without recipients must be rejected"
        );
        let empty = UpdateEnvelopeParams {
            recipients: Some(vec![]),
            ..Default::default()
        };
        assert!(matches!(empty.validate(), Err(CliError::Config(_))));
        let ok = UpdateEnvelopeParams {
            recipients: Some(vec![RecipientSpec::new()]),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn update_validate_allows_none_documents_with_recipients() {
        // documents=None leaves the doc set unchanged; recipients must still be
        // supplied for the update to validate.
        let p = UpdateEnvelopeParams {
            name: Some("x".to_owned()),
            recipients: Some(vec![RecipientSpec::new()]),
            ..Default::default()
        };
        assert!(p.validate().is_ok());
        assert!(!p.is_empty());
        assert!(UpdateEnvelopeParams::new().is_empty());
    }

    // ─── envelope-unwrap extractors ──────────────────────────────────────────

    #[test]
    fn extract_sign_envelope_named_key_and_response_wrapper() {
        // Named-key shape (signing.txt:176-282) — what the client returns when
        // there is no `response` key.
        let named = json!({"result": true, "sign_envelope": {"id": "env1"}});
        assert_eq!(extract_sign_envelope(&named).unwrap()["id"], json!("env1"));
        // Standard wrapper, defensive.
        let wrapped = json!({"response": {"sign_envelope": {"id": "env2"}}});
        assert_eq!(
            extract_sign_envelope(&wrapped).unwrap()["id"],
            json!("env2")
        );
        // Absent → None.
        assert!(extract_sign_envelope(&json!({"result": true})).is_none());
    }

    #[test]
    fn extract_sign_envelopes_prefers_envelopes_key() {
        // Live list shape keys on `envelopes` (P2/F18).
        let v = json!({"result": true, "envelopes": [{"id": "a"}, {"id": "b"}]});
        let list = extract_sign_envelopes(&v).unwrap();
        assert!(list.is_array());
        assert_eq!(list[1]["id"], json!("b"));
    }

    #[test]
    fn extract_sign_envelopes_tolerates_legacy_and_wrapper() {
        // Tolerant fallback to the legacy `sign_envelopes` key …
        let legacy = json!({"result": true, "sign_envelopes": [{"id": "x"}]});
        assert_eq!(
            extract_sign_envelopes(&legacy).unwrap()[0]["id"],
            json!("x")
        );
        // … and to the standard `response` wrapper.
        let wrapped = json!({"response": {"envelopes": [{"id": "y"}]}});
        assert_eq!(
            extract_sign_envelopes(&wrapped).unwrap()[0]["id"],
            json!("y")
        );
        // Absent → None.
        assert!(extract_sign_envelopes(&json!({"result": true})).is_none());
    }
}
