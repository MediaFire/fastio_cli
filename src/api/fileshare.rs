// Justification: every `pub async fn` here returns `Result<_, CliError>` and
// fails for exactly one reason — the underlying HTTP/envelope call via
// `ApiClient` (network error, non-2xx envelope, or parse failure), already
// fully documented on `CliError`/`ApiError`. Per-function `# Errors` sections
// would be identical copies of "Returns `CliError` if the API request fails."
// that add noise without information, so the doc requirement is allowed off
// module-wide rather than satisfied with boilerplate. This is scoped to this
// builder module; the rest of the crate keeps the lint on. The path-builder /
// param-validate functions that fail for a CLIENT-SIDE reason (empty id,
// mutually-exclusive flags) document their `# Errors` individually below.
#![allow(clippy::missing_errors_doc)]

//! File Share (durable single-file link) API — management + consumption.
//!
//! A **File Share** is a durable, link-shareable view of one workspace file —
//! the successor to the retired QuickShare. It is bound to a single file node
//! at creation (the binding is immutable) and serves recipients (anonymous →
//! named-people, per the access tier) without workspace membership.
//!
//! Two endpoint families with different auth and body conventions:
//!
//! ## Management (authenticated workspace member, FORM-encoded)
//!
//! Create / list / update / delete plus grants list/add/remove. Request bodies
//! are **form-encoded** (the Fast.io default), NOT JSON. Routes:
//!
//! | Op | Method + path | Response key |
//! |---|---|---|
//! | Create | `POST /workspace/{ws}/create/fileshare/` (form) | `fileshare` |
//! | List | `GET /workspace/{ws}/list/fileshares/` | `fileshares` |
//! | Update | `POST /fileshare/{id}/update/` (form; PATCH also accepted) | `fileshare` |
//! | Delete | `DELETE /fileshare/{id}/delete/` | (bare `result`) |
//! | Grants list | `GET /fileshare/{id}/grants/` | `grants` |
//! | Grant add | `POST /fileshare/{id}/grants/` (form) | `user` / `grant` / bare |
//! | Grant remove | `DELETE /fileshare/{id}/grants/?user=…` (QUERY) | (bare `result`) |
//!
//! Grants take **exactly one** of `user` (numeric id) or `email`. `capability`
//! is required on add but NOT on remove. The DELETE uses **query params**, never
//! a body (DELETE bodies are not reliably parsed server-side).
//!
//! ## Consumption (recipient; tier + optional password + optional grant)
//!
//! Details / storage read / preview / versions. These may be **anonymous**
//! (tier-dependent — `anyone_with_link` needs no token). An optional link
//! password travels ONLY in the `x-ve-password` request header, threaded through
//! the password-capable client helpers ([`ApiClient::get_with_password`] /
//! [`ApiClient::download_file_stream_with_password`] /
//! [`ApiClient::download_preview_following_redirect`]). `details` carries
//! `effective_capability` (the caller's highest of `view`/`download`/`edit`).
//!
//! | Op | Method + path | Needs |
//! |---|---|---|
//! | Details | `GET /fileshare/{id}/details/` | view |
//! | Read (download) | `GET /fileshare/{id}/storage/read/` | download |
//! | Preview | `GET /fileshare/{id}/storage/preview/{type}/read/` | view |
//! | Versions | `GET /fileshare/{id}/storage/versions/` | view |
//! | Version read | `GET /fileshare/{id}/storage/versions/{vid}/read/` | download |
//! | WebSocket auth | `GET /websocket/auth/{id}` | member |
//!
//! ## Response envelope shape
//!
//! Responses use a **boolean `result` + named-key** envelope (`fileshare`,
//! `fileshares`, `grants`, `grant`, `user`; write-back `session`) — NOT the
//! standard `{"result": …, "response": {…}}` wrapper. The client's
//! [`ApiClient::handle_response`] only unwraps a `response` key, so for these
//! endpoints it returns the WHOLE envelope (minus `current_api_version`) — the
//! correct render target (the markdown renderer needs the top-level `result`).
//! The `extract_*` helpers below return the bare object/array for callers/tests
//! that need it; they tolerate an outer `response` wrapper for robustness.
//!
//! ## Expiry
//!
//! `expires` (relative seconds, `1..=3155760000`) XOR `expires_at` (absolute; a
//! value without a timezone is UTC). Both-present is rejected (even
//! `expires=… & expires_at=null`). On update, clearing the password sends
//! `password=""`; clearing the expiry sends `expires_at=""`.
//!
//! ## Identifier formats
//!
//! The File Share id is a 19-digit numeric profile id; the bound node id is an
//! opaque base32 `OpaqueId`. Both are treated as opaque `String` and URL-encoded
//! into the path — never parsed or assumed structured.

use std::collections::HashMap;

use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Minimum relative-expiry value accepted client-side (`expires` > 0 second).
pub const MIN_EXPIRES_SECS: u64 = 1;

/// Maximum relative-expiry value accepted client-side (~100 years,
/// `workspaces.txt` / `FILE_SHARING.md` §7).
pub const MAX_EXPIRES_SECS: u64 = 3_155_760_000;

/// The three documented access tiers (`access_option` values). Exposed for the
/// command/MCP layers' value parsers and enforced client-side by
/// [`validate_access_option`] (used in create/update `validate`) so a library
/// consumer gets a clear error before the server 400; the server remains the
/// final authority.
pub const ACCESS_OPTIONS: &[&str] = &["anyone_with_link", "any_registered", "named_people"];

/// The three ordered grant capabilities (`view` < `download` < `edit`).
pub const CAPABILITIES: &[&str] = &["view", "download", "edit"];

/// Prefix the server stamps on a write-back session's `status_message` when an
/// `if_version_id` compare-and-swap precondition fails (`upload.txt:682`).
pub const CONFLICT_VERSION_PREFIX: &str = "CONFLICT_VERSION_MISMATCH:";

// ─── Path builders ──────────────────────────────────────────────────────────

/// Reject an empty id with a clear client-side error before any network call.
///
/// Uses [`CliError::Parse`], NOT [`CliError::Config`] (M3): a missing/empty CLI
/// argument is an input problem, and `Config`'s global hint ("Run `fastio
/// configure init` … or check your config file.") would mis-steer the user to
/// their profile/config when the actual fix is to supply the argument. `Parse`
/// emits no hint, which reads better here than an actively-misleading one. (The
/// pre-existing `signing.rs` empty-id checks still use `Config`; aligning them
/// is out of this diff's scope and recorded as a follow-up.)
fn require_id(id: &str, what: &str) -> Result<(), CliError> {
    if id.is_empty() {
        return Err(CliError::Parse(format!(
            "a {what} is required for File Share operations"
        )));
    }
    Ok(())
}

/// Build the create path `/workspace/{ws}/create/fileshare/`.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `workspace_id` is empty.
pub fn create_fileshare_path(workspace_id: &str) -> Result<String, CliError> {
    require_id(workspace_id, "workspace id")?;
    Ok(format!(
        "/workspace/{}/create/fileshare/",
        urlencoding::encode(workspace_id)
    ))
}

/// Build the list path `/workspace/{ws}/list/fileshares/`.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `workspace_id` is empty.
pub fn list_fileshares_path(workspace_id: &str) -> Result<String, CliError> {
    require_id(workspace_id, "workspace id")?;
    Ok(format!(
        "/workspace/{}/list/fileshares/",
        urlencoding::encode(workspace_id)
    ))
}

/// Build a single File Share action path `/fileshare/{id}/{action}/`.
///
/// Used for `update` / `delete` / `details` / `grants`. The id is URL-encoded;
/// `action` is ALWAYS one of a small set of fixed string literals supplied by
/// this module's own management fns (never user-controlled), so it is
/// interpolated verbatim. PRIVATE by design (L3): exposing it publicly with a
/// caller-supplied `action` would invite an unencoded segment from outside the
/// module — callers use the specific public builders / management fns instead.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `fileshare_id` is empty.
fn fileshare_path(fileshare_id: &str, action: &str) -> Result<String, CliError> {
    require_id(fileshare_id, "File Share id")?;
    Ok(format!(
        "/fileshare/{}/{action}/",
        urlencoding::encode(fileshare_id)
    ))
}

/// Build the storage-read (download) path `/fileshare/{id}/storage/read/`.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `fileshare_id` is empty.
pub fn storage_read_path(fileshare_id: &str) -> Result<String, CliError> {
    require_id(fileshare_id, "File Share id")?;
    Ok(format!(
        "/fileshare/{}/storage/read/",
        urlencoding::encode(fileshare_id)
    ))
}

/// Build the preview path
/// `/fileshare/{id}/storage/preview/{preview_type}/read/`.
///
/// Both `fileshare_id` and `preview_type` are URL-encoded.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `fileshare_id` or `preview_type` is empty.
pub fn storage_preview_path(fileshare_id: &str, preview_type: &str) -> Result<String, CliError> {
    require_id(fileshare_id, "File Share id")?;
    require_id(preview_type, "preview type")?;
    Ok(format!(
        "/fileshare/{}/storage/preview/{}/read/",
        urlencoding::encode(fileshare_id),
        urlencoding::encode(preview_type)
    ))
}

/// Build the version-list path `/fileshare/{id}/storage/versions/`.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `fileshare_id` is empty.
pub fn storage_versions_path(fileshare_id: &str) -> Result<String, CliError> {
    require_id(fileshare_id, "File Share id")?;
    Ok(format!(
        "/fileshare/{}/storage/versions/",
        urlencoding::encode(fileshare_id)
    ))
}

/// Build the version-read path
/// `/fileshare/{id}/storage/versions/{version_id}/read/`.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `fileshare_id` or `version_id` is empty.
pub fn storage_version_read_path(fileshare_id: &str, version_id: &str) -> Result<String, CliError> {
    require_id(fileshare_id, "File Share id")?;
    require_id(version_id, "version id")?;
    Ok(format!(
        "/fileshare/{}/storage/versions/{}/read/",
        urlencoding::encode(fileshare_id),
        urlencoding::encode(version_id)
    ))
}

/// Build the WebSocket-auth token path `/websocket/auth/{id}`.
///
/// NOTE: NO trailing slash — this matches the `orchestration::realtime_token`
/// precedent (`/websocket/auth/{workflow_id}`) and the local llms docs.
/// `FILE_SHARING.md:97` shows a trailing slash; that conflict is flagged for
/// live-verify (try no-slash first, record any variance).
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `fileshare_id` is empty.
pub fn websocket_auth_path(fileshare_id: &str) -> Result<String, CliError> {
    require_id(fileshare_id, "File Share id")?;
    Ok(format!(
        "/websocket/auth/{}",
        urlencoding::encode(fileshare_id)
    ))
}

// ─── Param structs ──────────────────────────────────────────────────────────

/// Parameters for creating a File Share (`workspaces.txt:1679-1689`).
///
/// `#[non_exhaustive]` because the create surface may grow. The password is held
/// as a [`SecretString`] and exposed ONLY at [`CreateFileShareParams::to_form`]
/// time (the redacting form trace-log still masks the `password` key).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct CreateFileShareParams {
    /// `OpaqueId` of the **file** node to share (required; must be a file).
    pub node: Option<String>,
    /// Optional display title (max 255 chars server-side).
    pub title: Option<String>,
    /// Access tier: `anyone_with_link` / `any_registered` / `named_people`.
    pub access_option: Option<String>,
    /// Optional link password. Held as a [`SecretString`]; exposed only in
    /// [`CreateFileShareParams::to_form`].
    pub password: Option<SecretString>,
    /// Optional RELATIVE expiry in seconds (`1..=3155760000`). Mutually
    /// exclusive with `expires_at`.
    pub expires: Option<u64>,
    /// Optional ABSOLUTE expiry datetime (no timezone = UTC). Mutually exclusive
    /// with `expires`.
    pub expires_at: Option<String>,
}

impl CreateFileShareParams {
    /// An empty parameter set (equivalent to [`Default::default`]). Provided so
    /// the binary crate can build this `#[non_exhaustive]` struct without
    /// struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the bound file node id (required).
    #[must_use]
    pub fn node(mut self, node: Option<String>) -> Self {
        self.node = node;
        self
    }

    /// Set the display title.
    #[must_use]
    pub fn title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    /// Set the access tier.
    #[must_use]
    pub fn access_option(mut self, access_option: Option<String>) -> Self {
        self.access_option = access_option;
        self
    }

    /// Set the optional link password.
    #[must_use]
    pub fn password(mut self, password: Option<SecretString>) -> Self {
        self.password = password;
        self
    }

    /// Set the relative expiry (seconds).
    #[must_use]
    pub fn expires(mut self, expires: Option<u64>) -> Self {
        self.expires = expires;
        self
    }

    /// Set the absolute expiry datetime.
    #[must_use]
    pub fn expires_at(mut self, expires_at: Option<String>) -> Self {
        self.expires_at = expires_at;
        self
    }

    /// Validate client-side BEFORE any network call: `node` is required, a
    /// supplied `title` / `password` / `expires_at` must be non-empty, a supplied
    /// `access_option` must be one of the documented tiers, the two expiry inputs
    /// are mutually exclusive, and a relative `expires` is in range.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Parse`] (NOT [`CliError::Config`] — see [`require_id`])
    /// when `node` is missing/empty, a supplied `title` / `password` /
    /// `expires_at` is empty, `access_option` is not one of [`ACCESS_OPTIONS`],
    /// both `expires` and `expires_at` are set, or `expires` is outside
    /// `1..=3155760000`.
    pub fn validate(&self) -> Result<(), CliError> {
        match &self.node {
            None => {
                return Err(CliError::Parse(
                    "a File Share requires a --node (the file to share)".to_owned(),
                ));
            }
            Some(node) if node.is_empty() => {
                return Err(CliError::Parse(
                    "the --node value (file to share) must not be empty".to_owned(),
                ));
            }
            Some(_) => {}
        }
        // F3-2: an empty title (`title=""`) is the server's title-CLEAR sentinel.
        // Title-clearing is deliberately unsupported (see the field doc), so a
        // directly-supplied empty title must be rejected rather than silently
        // reintroducing the clear path. Exact-empty only — a whitespace title is
        // the server's business.
        if self.title.as_deref() == Some("") {
            return Err(CliError::Parse(
                "the --title value must not be empty".to_owned(),
            ));
        }
        // F3-7: validate the access tier client-side so library consumers get a
        // clear error (the CLI layer also enforces this via clap).
        validate_access_option(self.access_option.as_deref())?;
        // M2: an empty password is meaningless on create — the contract is a
        // 1-255 char link password — and an empty `expires_at` is not a valid
        // absolute datetime. Reject both rather than POSTing a blank value (a
        // blank `password=""` would create an UNPROTECTED share from what the
        // user meant to be password-protected, e.g. an empty env var).
        if self
            .password
            .as_ref()
            .is_some_and(|p| p.expose_secret().is_empty())
        {
            return Err(CliError::Parse(
                "the --password value must not be empty (1-255 characters)".to_owned(),
            ));
        }
        if self.expires_at.as_deref().is_some_and(str::is_empty) {
            return Err(CliError::Parse(
                "the --expires-at value must not be empty".to_owned(),
            ));
        }
        // F2-6: a directly-supplied `expires_at` that trims to a case-insensitive
        // "null" would act as the documented clear sentinel, creating a DURABLE
        // (never-expiring) share from what the user meant to be a timed one. On
        // create there is no clear flag to honor, so reject it rather than let the
        // literal smuggle in the clear behavior.
        if self
            .expires_at
            .as_deref()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("null"))
        {
            return Err(CliError::Parse(
                "the --expires-at value must not be the literal \"null\"".to_owned(),
            ));
        }
        validate_expiry_pair(self.expires, self.expires_at.as_deref())?;
        Ok(())
    }

    /// Serialize to the create form. Emits only present keys; the password is
    /// exposed here (and only here).
    #[must_use]
    fn to_form(&self) -> HashMap<String, String> {
        let mut form = HashMap::new();
        if let Some(node) = &self.node {
            form.insert("node".to_owned(), node.clone());
        }
        if let Some(title) = &self.title {
            form.insert("title".to_owned(), title.clone());
        }
        if let Some(access) = &self.access_option {
            form.insert("access_option".to_owned(), access.clone());
        }
        if let Some(password) = &self.password {
            form.insert("password".to_owned(), password.expose_secret().to_owned());
        }
        if let Some(expires) = self.expires {
            form.insert("expires".to_owned(), expires.to_string());
        }
        if let Some(expires_at) = &self.expires_at {
            form.insert("expires_at".to_owned(), expires_at.clone());
        }
        form
    }
}

/// Parameters for updating a File Share's mutable settings
/// (`workspaces.txt:1793-1801`).
///
/// `clear_password` sends `password=""` (clears the link password);
/// `clear_expires` sends `expires_at=""` (clears the expiry → durable again).
/// These are explicit booleans rather than empty-string sentinels so the
/// command layer's UX is unambiguous. `#[non_exhaustive]` because the update
/// surface may grow.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
#[non_exhaustive]
pub struct UpdateFileShareParams {
    /// New display title (max 255).
    ///
    /// F2-11: title CLEARING (sending an empty title) is deliberately
    /// unsupported in v1 — a title can be changed but not cleared, so there is no
    /// `clear_title` flag and an empty title is rejected like any other empty
    /// value. This is a conscious decision (a File Share always shows a title),
    /// not an oversight; add a clear flag only if a blank-title product need
    /// appears.
    pub title: Option<String>,
    /// New access tier.
    pub access_option: Option<String>,
    /// New link password (held as a [`SecretString`]; exposed only in
    /// [`UpdateFileShareParams::to_form`]).
    pub password: Option<SecretString>,
    /// New RELATIVE expiry (seconds). Mutually exclusive with `expires_at` and
    /// `clear_expires`.
    pub expires: Option<u64>,
    /// New ABSOLUTE expiry datetime. Mutually exclusive with `expires` and
    /// `clear_expires`.
    pub expires_at: Option<String>,
    /// Clear the link password (sends `password=""`). Mutually exclusive with
    /// `password`.
    pub clear_password: bool,
    /// Clear the expiry → durable again (sends `expires_at=""`). Mutually
    /// exclusive with `expires` / `expires_at`.
    pub clear_expires: bool,
}

impl UpdateFileShareParams {
    /// An empty parameter set (equivalent to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the new title.
    #[must_use]
    pub fn title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    /// Set the new access tier.
    #[must_use]
    pub fn access_option(mut self, access_option: Option<String>) -> Self {
        self.access_option = access_option;
        self
    }

    /// Set the new link password.
    #[must_use]
    pub fn password(mut self, password: Option<SecretString>) -> Self {
        self.password = password;
        self
    }

    /// Set the new relative expiry (seconds).
    #[must_use]
    pub fn expires(mut self, expires: Option<u64>) -> Self {
        self.expires = expires;
        self
    }

    /// Set the new absolute expiry datetime.
    #[must_use]
    pub fn expires_at(mut self, expires_at: Option<String>) -> Self {
        self.expires_at = expires_at;
        self
    }

    /// Set the clear-password flag.
    #[must_use]
    pub fn clear_password(mut self, clear: bool) -> Self {
        self.clear_password = clear;
        self
    }

    /// Set the clear-expiry flag.
    #[must_use]
    pub fn clear_expires(mut self, clear: bool) -> Self {
        self.clear_expires = clear;
        self
    }

    /// Whether the update carries no change at all (so the command layer can
    /// reject a no-op rather than send an empty request).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.access_option.is_none()
            && self.password.is_none()
            && self.expires.is_none()
            && self.expires_at.is_none()
            && !self.clear_password
            && !self.clear_expires
    }

    /// Validate the mutually-exclusive update inputs BEFORE the network call:
    /// a non-empty `title`, a valid `access_option`, at most one of `expires` /
    /// `expires_at` / `clear_expires`, at most one of `password` /
    /// `clear_password`, a non-empty directly-supplied `password` / `expires_at`,
    /// and an in-range relative `expires`.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Parse`] (NOT [`CliError::Config`] — see [`require_id`])
    /// when a supplied `title` is empty, `access_option` is not one of
    /// [`ACCESS_OPTIONS`], the exclusivity rules are violated, a directly-supplied
    /// `password` or `expires_at` is empty, or `expires` is out of range.
    pub fn validate(&self) -> Result<(), CliError> {
        // F3-2: title-clearing is deliberately unsupported (see the field doc), so
        // a directly-supplied empty title (`title=""`, the server's clear
        // sentinel) is rejected rather than silently reintroducing the clear path.
        // Exact-empty only — a whitespace title is the server's business.
        if self.title.as_deref() == Some("") {
            return Err(CliError::Parse(
                "the --title value must not be empty".to_owned(),
            ));
        }
        // F3-7: validate the access tier client-side (the CLI layer also enforces
        // this via clap).
        validate_access_option(self.access_option.as_deref())?;
        // At most one expiry intent.
        let expiry_intents = u8::from(self.expires.is_some())
            + u8::from(self.expires_at.is_some())
            + u8::from(self.clear_expires);
        if expiry_intents > 1 {
            return Err(CliError::Parse(
                "choose at most one of --expires, --expires-at, or --clear-expires".to_owned(),
            ));
        }
        if self.password.is_some() && self.clear_password {
            return Err(CliError::Parse(
                "choose either --password or --clear-password, not both".to_owned(),
            ));
        }
        // M2: clearing the password / expiry is ONLY via the explicit
        // `clear_password` / `clear_expires` flags. A directly-supplied empty
        // `password` / `expires_at` is rejected so an empty env var (e.g.
        // `--password "$FASTIO_FILESHARE_PASSWORD"` with the var unset) can NEVER
        // silently strip password protection or the expiry by sending a blank
        // value — `to_form` only emits the `password=""` / `expires_at=""` clear
        // sentinels from the clear flags.
        if self
            .password
            .as_ref()
            .is_some_and(|p| p.expose_secret().is_empty())
        {
            return Err(CliError::Parse(
                "the --password value must not be empty; use --clear-password to remove the password"
                    .to_owned(),
            ));
        }
        if self.expires_at.as_deref().is_some_and(str::is_empty) {
            return Err(CliError::Parse(
                "the --expires-at value must not be empty; use --clear-expires to remove the expiry"
                    .to_owned(),
            ));
        }
        // F2-6: a directly-supplied `expires_at` that trims to a case-insensitive
        // "null" would act as the documented clear sentinel, silently clearing the
        // expiry while bypassing the explicit `clear_expires` flag. Clearing stays
        // flag-only — reject the literal here too.
        if self
            .expires_at
            .as_deref()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("null"))
        {
            return Err(CliError::Parse(
                "the --expires-at value must not be the literal \"null\"; \
                 use --clear-expires to remove the expiry"
                    .to_owned(),
            ));
        }
        if let Some(expires) = self.expires {
            validate_expires_range(expires)?;
        }
        Ok(())
    }

    /// Serialize to the update form. Emits only the keys the caller set; the
    /// clear flags emit empty-string sentinels (`password=""` / `expires_at=""`)
    /// that the server reads as "clear this field".
    #[must_use]
    fn to_form(&self) -> HashMap<String, String> {
        let mut form = HashMap::new();
        if let Some(title) = &self.title {
            form.insert("title".to_owned(), title.clone());
        }
        if let Some(access) = &self.access_option {
            form.insert("access_option".to_owned(), access.clone());
        }
        if self.clear_password {
            form.insert("password".to_owned(), String::new());
        } else if let Some(password) = &self.password {
            form.insert("password".to_owned(), password.expose_secret().to_owned());
        }
        if self.clear_expires {
            form.insert("expires_at".to_owned(), String::new());
        } else if let Some(expires) = self.expires {
            form.insert("expires".to_owned(), expires.to_string());
        } else if let Some(expires_at) = &self.expires_at {
            form.insert("expires_at".to_owned(), expires_at.clone());
        }
        form
    }
}

/// Parameters for a grant add (POST) or remove (DELETE) — `workspaces.txt:1871`.
///
/// Supply **exactly one** of `user` (numeric id) or `email`. `capability` is
/// required on add and ignored on remove (validation is split via
/// [`GrantParams::validate_add`] / [`GrantParams::validate_remove`]).
/// `#[non_exhaustive]` because the surface may grow.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GrantParams {
    /// Grantee's 19-digit user profile id. Mutually exclusive with `email`.
    pub user: Option<String>,
    /// Grantee's email address. Mutually exclusive with `user`.
    pub email: Option<String>,
    /// Capability to grant (`view` / `download` / `edit`). Required on add.
    pub capability: Option<String>,
}

impl GrantParams {
    /// An empty parameter set (equivalent to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the grantee user id.
    #[must_use]
    pub fn user(mut self, user: Option<String>) -> Self {
        self.user = user;
        self
    }

    /// Set the grantee email.
    #[must_use]
    pub fn email(mut self, email: Option<String>) -> Self {
        self.email = email;
        self
    }

    /// Set the capability.
    #[must_use]
    pub fn capability(mut self, capability: Option<String>) -> Self {
        self.capability = capability;
        self
    }

    /// Validate the `user` XOR `email` rule shared by add and remove.
    fn validate_target(&self) -> Result<(), CliError> {
        match (
            self.user.as_deref().filter(|s| !s.is_empty()),
            self.email.as_deref().filter(|s| !s.is_empty()),
        ) {
            (Some(_), Some(_)) => Err(CliError::Parse(
                "supply exactly one of --user or --email, not both".to_owned(),
            )),
            (None, None) => Err(CliError::Parse(
                "supply one of --user or --email".to_owned(),
            )),
            _ => Ok(()),
        }
    }

    /// Validate an ADD: requires the `user` XOR `email` target AND a
    /// `capability` in `{view, download, edit}`.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Parse`] (NOT [`CliError::Config`] — see [`require_id`])
    /// when the target rule is violated, the capability is missing, or the
    /// capability is not one of the three.
    pub fn validate_add(&self) -> Result<(), CliError> {
        self.validate_target()?;
        match self.capability.as_deref() {
            None | Some("") => Err(CliError::Parse(
                "a grant requires --capability (view, download, or edit)".to_owned(),
            )),
            Some(cap) if CAPABILITIES.contains(&cap) => Ok(()),
            Some(cap) => Err(CliError::Parse(format!(
                "invalid --capability '{cap}' (expected view, download, or edit)"
            ))),
        }
    }

    /// Validate a REMOVE: requires ONLY the `user` XOR `email` target
    /// (`capability` is not used on DELETE).
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Parse`] (NOT [`CliError::Config`] — see [`require_id`])
    /// when the target rule is violated.
    pub fn validate_remove(&self) -> Result<(), CliError> {
        self.validate_target()
    }

    /// Serialize to the grant-add POST form (`user`/`email` + `capability`).
    #[must_use]
    fn to_form(&self) -> HashMap<String, String> {
        let mut form = HashMap::new();
        if let Some(user) = self.user.as_deref().filter(|s| !s.is_empty()) {
            form.insert("user".to_owned(), user.to_owned());
        }
        if let Some(email) = self.email.as_deref().filter(|s| !s.is_empty()) {
            form.insert("email".to_owned(), email.to_owned());
        }
        if let Some(cap) = &self.capability {
            form.insert("capability".to_owned(), cap.clone());
        }
        form
    }

    /// Serialize to the grant-remove DELETE QUERY params (`user` XOR `email`;
    /// never `capability`). The grants DELETE reads its target from the query
    /// string, never a body (`workspaces.txt:1822`, §12.6).
    #[must_use]
    fn to_query(&self) -> HashMap<String, String> {
        let mut params = HashMap::new();
        if let Some(user) = self.user.as_deref().filter(|s| !s.is_empty()) {
            params.insert("user".to_owned(), user.to_owned());
        }
        if let Some(email) = self.email.as_deref().filter(|s| !s.is_empty()) {
            params.insert("email".to_owned(), email.to_owned());
        }
        params
    }
}

/// Validate a supplied `access_option` against the documented tiers.
///
/// F3-7: `ACCESS_OPTIONS` previously existed only for the command/MCP value
/// parsers; the library validators never checked the supplied value, so a
/// library consumer could POST an unknown tier and only learn of it from a
/// server 400. This protects those consumers (the CLI layer ALSO enforces the
/// set via clap in Wave 2). `None` is valid — the field is optional and the
/// server applies its default. Uses [`CliError::Parse`] (see [`require_id`]); the
/// error lists the accepted values.
fn validate_access_option(access_option: Option<&str>) -> Result<(), CliError> {
    match access_option {
        None => Ok(()),
        Some(opt) if ACCESS_OPTIONS.contains(&opt) => Ok(()),
        Some(opt) => Err(CliError::Parse(format!(
            "invalid --access-option '{opt}' (expected one of {})",
            ACCESS_OPTIONS.join(", ")
        ))),
    }
}

/// Validate the create-time expiry pair: at most one of `expires` / `expires_at`
/// may be set, and a relative `expires` must be in range.
///
/// Uses [`CliError::Parse`] (NOT [`CliError::Config`] — see [`require_id`]): an
/// input-validation failure is an argument problem, and `Config`'s global hint
/// ("Run `fastio configure init` …") would mis-steer the user.
fn validate_expiry_pair(expires: Option<u64>, expires_at: Option<&str>) -> Result<(), CliError> {
    if expires.is_some() && expires_at.is_some() {
        return Err(CliError::Parse(
            "choose either --expires or --expires-at, not both".to_owned(),
        ));
    }
    if let Some(expires) = expires {
        validate_expires_range(expires)?;
    }
    Ok(())
}

/// Validate a relative `expires` value is within `1..=3155760000`.
///
/// Uses [`CliError::Parse`] (see [`validate_expiry_pair`]).
fn validate_expires_range(expires: u64) -> Result<(), CliError> {
    if !(MIN_EXPIRES_SECS..=MAX_EXPIRES_SECS).contains(&expires) {
        return Err(CliError::Parse(format!(
            "--expires must be between {MIN_EXPIRES_SECS} and {MAX_EXPIRES_SECS} seconds"
        )));
    }
    Ok(())
}

// ─── Envelope extractors ────────────────────────────────────────────────────

/// Pull the bare `fileshare` object out of a create/update/details response.
///
/// Tolerates `{"result": true, "fileshare": {…}}` and the standard
/// `{"response": {"fileshare": {…}}}` wrapper; returns `None` when absent.
#[must_use]
pub fn extract_fileshare(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("fileshare")
}

/// Pull the bare `fileshares` array out of a list response.
#[must_use]
pub fn extract_fileshares(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("fileshares")
}

/// Pull the bare `grants` array out of a grants-list response.
#[must_use]
pub fn extract_grants(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("grants")
}

/// Pull the bare `grant` object out of a grant-add response for an UNREGISTERED
/// email (a pending invitation; same shape as a grants-list row).
#[must_use]
pub fn extract_grant(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("grant")
}

/// Pull the resolved `user` object out of a grant-add response for a REGISTERED
/// email (`{"result": true, "user": {"id": …}}`).
#[must_use]
pub fn extract_user(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("user")
}

/// Pull the bare `versions` array out of a version-list response.
#[must_use]
pub fn extract_versions(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("versions")
}

/// Pull the bare write-back `session` object out of an upload-session response.
#[must_use]
pub fn extract_session(value: &Value) -> Option<&Value> {
    let payload = value.get("response").unwrap_or(value);
    payload.get("session")
}

/// Parse the current version id out of a write-back conflict `status_message`.
///
/// Returns `Some(version_id)` when `status_message` starts with
/// [`CONFLICT_VERSION_PREFIX`] (`CONFLICT_VERSION_MISMATCH:{vid}`), else `None`.
/// The trailing version id is returned trimmed of surrounding whitespace.
#[must_use]
pub fn parse_conflict_version(status_message: &str) -> Option<&str> {
    status_message
        .strip_prefix(CONFLICT_VERSION_PREFIX)
        .map(str::trim)
        .filter(|vid| !vid.is_empty())
}

/// Extract the bound file's name from a `details` response envelope.
///
/// The bound file name lives at `fileshare.file.name` in the details envelope
/// (`shares.txt:1857-1862`) — `download::extract_filename` looks for top-level
/// `name`/`filename` keys and does NOT find it. Returns the raw name; the
/// command layer is responsible for any `sanitize_filename` step before using it
/// as an output path. Tolerates an outer `response` wrapper.
#[must_use]
pub fn fileshare_file_name(details: &Value) -> Option<String> {
    extract_fileshare(details)
        .and_then(|fs| fs.get("file"))
        .and_then(|file| file.get("name"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

// ─── Management API fns (Bearer, form) ──────────────────────────────────────

/// Create a File Share bound to a workspace file node.
///
/// `POST /workspace/{ws}/create/fileshare/` (form). Caller-side validation runs
/// first via [`CreateFileShareParams::validate`].
///
/// F4-1: routed through [`ApiClient::post_sensitive_form`] (fail-closed on any
/// 3xx) rather than the redirect-FOLLOWING [`ApiClient::post`] — the create form
/// can carry a link `password` field, and a 307/308 on the ordinary client would
/// replay the entire form body (password included) to the redirect `Location`.
pub async fn create_fileshare(
    client: &ApiClient,
    workspace_id: &str,
    params: &CreateFileShareParams,
) -> Result<Value, CliError> {
    params.validate()?;
    let path = create_fileshare_path(workspace_id)?;
    client.post_sensitive_form(&path, &params.to_form()).await
}

/// List a workspace's File Shares (offset-paginated).
///
/// `GET /workspace/{ws}/list/fileshares/` with optional `offset` / `limit`.
pub async fn list_fileshares(
    client: &ApiClient,
    workspace_id: &str,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<Value, CliError> {
    let path = list_fileshares_path(workspace_id)?;
    let mut params = HashMap::new();
    if let Some(offset) = offset {
        params.insert("offset".to_owned(), offset.to_string());
    }
    if let Some(limit) = limit {
        params.insert("limit".to_owned(), limit.to_string());
    }
    if params.is_empty() {
        client.get(&path).await
    } else {
        client.get_with_params(&path, &params).await
    }
}

/// Update a File Share's mutable settings.
///
/// `POST /fileshare/{id}/update/` (form; PATCH is also accepted server-side —
/// POST is the portable default). Caller-side validation runs first.
///
/// F4-1: routed through [`ApiClient::post_sensitive_form`] (fail-closed on any
/// 3xx) rather than the redirect-FOLLOWING [`ApiClient::post`] — the update form
/// can carry a link `password` field, and a 307/308 on the ordinary client would
/// replay the entire form body (password included) to the redirect `Location`.
pub async fn update_fileshare(
    client: &ApiClient,
    fileshare_id: &str,
    params: &UpdateFileShareParams,
) -> Result<Value, CliError> {
    params.validate()?;
    let path = fileshare_path(fileshare_id, "update")?;
    client.post_sensitive_form(&path, &params.to_form()).await
}

/// Delete a File Share (revokes the link, cascades its grants).
///
/// `DELETE /fileshare/{id}/delete/`. The bound file is never touched.
pub async fn delete_fileshare(client: &ApiClient, fileshare_id: &str) -> Result<Value, CliError> {
    let path = fileshare_path(fileshare_id, "delete")?;
    client.delete(&path).await
}

/// List a File Share's live named-people grants.
///
/// `GET /fileshare/{id}/grants/` (no pagination; first 1000).
pub async fn list_grants(client: &ApiClient, fileshare_id: &str) -> Result<Value, CliError> {
    let path = fileshare_path(fileshare_id, "grants")?;
    client.get(&path).await
}

/// Grant or raise a user's capability on a File Share.
///
/// `POST /fileshare/{id}/grants/` (form). Caller-side validation runs first via
/// [`GrantParams::validate_add`].
pub async fn add_grant(
    client: &ApiClient,
    fileshare_id: &str,
    params: &GrantParams,
) -> Result<Value, CliError> {
    params.validate_add()?;
    let path = fileshare_path(fileshare_id, "grants")?;
    client.post(&path, &params.to_form()).await
}

/// Revoke a user's grant on a File Share (idempotent).
///
/// `DELETE /fileshare/{id}/grants/?user=…` (or `?email=…`). The target is sent
/// as QUERY params, NEVER a body. Caller-side validation runs first via
/// [`GrantParams::validate_remove`].
pub async fn remove_grant(
    client: &ApiClient,
    fileshare_id: &str,
    params: &GrantParams,
) -> Result<Value, CliError> {
    params.validate_remove()?;
    let path = fileshare_path(fileshare_id, "grants")?;
    client.delete_with_params(&path, &params.to_query()).await
}

// ─── Consumption API fns (password-capable, may be anonymous) ───────────────

/// Get a File Share's public viewer details (`effective_capability`, bound file
/// metadata).
///
/// `GET /fileshare/{id}/details/` with an optional `x-ve-password` header. The
/// bearer token is attached only when the client holds one, so this serves both
/// authenticated and anonymous consumers.
pub async fn get_details(
    client: &ApiClient,
    fileshare_id: &str,
    password: Option<&SecretString>,
) -> Result<Value, CliError> {
    let path = fileshare_path(fileshare_id, "details")?;
    client.get_with_password(&path, password).await
}

/// List the bound file's versions.
///
/// `GET /fileshare/{id}/storage/versions/` with an optional `x-ve-password`
/// header. Anonymous-capable like [`get_details`].
pub async fn list_versions(
    client: &ApiClient,
    fileshare_id: &str,
    password: Option<&SecretString>,
) -> Result<Value, CliError> {
    let path = storage_versions_path(fileshare_id)?;
    client.get_with_password(&path, password).await
}

/// Mint a short-lived realtime-channel WebSocket token for a File Share
/// (workspace members only).
///
/// `GET /websocket/auth/{id}` (authed; no trailing slash — see
/// [`websocket_auth_path`]). Mints the token ONLY — no in-CLI WebSocket client
/// ships. The command layer wraps the returned token in a [`SecretString`].
pub async fn websocket_auth(client: &ApiClient, fileshare_id: &str) -> Result<Value, CliError> {
    let path = websocket_auth_path(fileshare_id)?;
    client.get(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Path builders ──────────────────────────────────────────────────

    #[test]
    fn path_builders_emit_exact_strings() {
        assert_eq!(
            create_fileshare_path("123").expect("ws"),
            "/workspace/123/create/fileshare/"
        );
        assert_eq!(
            list_fileshares_path("123").expect("ws"),
            "/workspace/123/list/fileshares/"
        );
        assert_eq!(
            fileshare_path("999", "update").expect("id"),
            "/fileshare/999/update/"
        );
        assert_eq!(
            fileshare_path("999", "grants").expect("id"),
            "/fileshare/999/grants/"
        );
        assert_eq!(
            storage_read_path("999").expect("id"),
            "/fileshare/999/storage/read/"
        );
        assert_eq!(
            storage_preview_path("999", "hls_stream").expect("id"),
            "/fileshare/999/storage/preview/hls_stream/read/"
        );
        assert_eq!(
            storage_versions_path("999").expect("id"),
            "/fileshare/999/storage/versions/"
        );
        assert_eq!(
            storage_version_read_path("999", "v7").expect("ids"),
            "/fileshare/999/storage/versions/v7/read/"
        );
        // WebSocket auth has NO trailing slash (orchestration precedent).
        assert_eq!(
            websocket_auth_path("999").expect("id"),
            "/websocket/auth/999"
        );
    }

    #[test]
    fn path_builders_url_encode_ids() {
        // An id with reserved characters must be percent-encoded, never raw.
        let p = fileshare_path("a/b c", "details").expect("encoded");
        assert!(p.contains("a%2Fb%20c"), "got: {p}");
        let pv = storage_preview_path("id", "weird type").expect("encoded");
        assert!(pv.contains("weird%20type"), "got: {pv}");
    }

    #[test]
    fn path_builders_reject_empty_ids() {
        assert!(create_fileshare_path("").is_err());
        assert!(list_fileshares_path("").is_err());
        assert!(fileshare_path("", "update").is_err());
        assert!(storage_read_path("").is_err());
        assert!(storage_preview_path("", "pdf").is_err());
        assert!(storage_preview_path("id", "").is_err());
        assert!(storage_versions_path("").is_err());
        assert!(storage_version_read_path("", "v1").is_err());
        assert!(storage_version_read_path("id", "").is_err());
        assert!(websocket_auth_path("").is_err());
    }

    #[test]
    fn empty_id_rejection_is_parse_not_config() {
        // M3: an empty id is a missing-argument problem, NOT a config problem.
        // It must surface as CliError::Parse (no misleading "run configure init"
        // hint), never CliError::Config (whose global hint mis-steers the user).
        let err = fileshare_path("", "update").expect_err("empty id must reject");
        assert!(
            matches!(err, CliError::Parse(_)),
            "empty id must be CliError::Parse, got: {err:?}"
        );
        // The Config hint must NOT be reachable for this error.
        assert!(
            err.suggestion().is_none(),
            "empty-id error must carry no (misleading) hint, got: {:?}",
            err.suggestion()
        );
        // A few more path builders share require_id — confirm the variant.
        assert!(matches!(
            create_fileshare_path("").expect_err("ws"),
            CliError::Parse(_)
        ));
        assert!(matches!(
            websocket_auth_path("").expect_err("id"),
            CliError::Parse(_)
        ));
    }

    // ─── CreateFileShareParams ──────────────────────────────────────────

    #[test]
    fn create_to_form_emits_only_present_keys() {
        let form = CreateFileShareParams::new()
            .node(Some("node1".to_owned()))
            .title(Some("Title".to_owned()))
            .access_option(Some("anyone_with_link".to_owned()))
            .password(Some(SecretString::from("pw".to_owned())))
            .expires(Some(3600))
            .to_form();
        assert_eq!(form.get("node").map(String::as_str), Some("node1"));
        assert_eq!(form.get("title").map(String::as_str), Some("Title"));
        assert_eq!(
            form.get("access_option").map(String::as_str),
            Some("anyone_with_link")
        );
        assert_eq!(form.get("password").map(String::as_str), Some("pw"));
        assert_eq!(form.get("expires").map(String::as_str), Some("3600"));
        assert!(!form.contains_key("expires_at"));

        // A minimal create emits only `node`.
        let minimal = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .to_form();
        assert_eq!(minimal.len(), 1);
        assert!(minimal.contains_key("node"));
    }

    #[test]
    fn create_validate_requires_node() {
        assert!(CreateFileShareParams::new().validate().is_err());
        assert!(
            CreateFileShareParams::new()
                .node(Some(String::new()))
                .validate()
                .is_err()
        );
        assert!(
            CreateFileShareParams::new()
                .node(Some("n".to_owned()))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn create_validate_rejects_empty_title() {
        // F3-2: an empty title is the server's title-CLEAR sentinel; clearing is
        // deliberately unsupported, so `title=""` must be rejected (as Parse).
        let p = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .title(Some(String::new()));
        assert!(
            matches!(p.validate(), Err(CliError::Parse(_))),
            "empty title must be rejected as Parse"
        );
        // A non-empty title is fine; a whitespace title is the server's business.
        assert!(
            CreateFileShareParams::new()
                .node(Some("n".to_owned()))
                .title(Some("My Share".to_owned()))
                .validate()
                .is_ok()
        );
        assert!(
            CreateFileShareParams::new()
                .node(Some("n".to_owned()))
                .title(Some("   ".to_owned()))
                .validate()
                .is_ok(),
            "a whitespace title is not the exact-empty clear sentinel"
        );
    }

    #[test]
    fn create_validate_checks_access_option_membership() {
        // F3-7: an unknown access tier must be rejected client-side (as Parse).
        let bad = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .access_option(Some("public".to_owned()));
        assert!(
            matches!(bad.validate(), Err(CliError::Parse(_))),
            "invalid access_option must be rejected as Parse"
        );
        // Each documented tier validates; None (default) is also accepted.
        for opt in ACCESS_OPTIONS {
            assert!(
                CreateFileShareParams::new()
                    .node(Some("n".to_owned()))
                    .access_option(Some((*opt).to_owned()))
                    .validate()
                    .is_ok(),
                "access_option {opt:?} must validate"
            );
        }
        assert!(
            CreateFileShareParams::new()
                .node(Some("n".to_owned()))
                .validate()
                .is_ok(),
            "a missing access_option (server default) is valid"
        );
    }

    #[test]
    fn create_validate_rejects_both_expiry_inputs() {
        let p = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .expires(Some(3600))
            .expires_at(Some("2026-12-31 00:00:00".to_owned()));
        assert!(p.validate().is_err());
    }

    #[test]
    fn create_validate_enforces_expires_range() {
        let too_low = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .expires(Some(0));
        assert!(too_low.validate().is_err());
        let too_high = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .expires(Some(MAX_EXPIRES_SECS + 1));
        assert!(too_high.validate().is_err());
        let ok = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .expires(Some(MAX_EXPIRES_SECS));
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn create_validate_rejects_empty_password_and_expires_at() {
        // M2: an empty password (e.g. an unset env var) must be rejected rather
        // than POSTing `password=""` and creating an UNPROTECTED share.
        let empty_pw = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .password(Some(SecretString::from(String::new())));
        assert!(
            matches!(empty_pw.validate(), Err(CliError::Parse(_))),
            "empty password must be rejected"
        );
        // A non-empty password is fine.
        assert!(
            CreateFileShareParams::new()
                .node(Some("n".to_owned()))
                .password(Some(SecretString::from("pw".to_owned())))
                .validate()
                .is_ok()
        );
        // An empty expires_at is not a valid absolute datetime.
        let empty_exp = CreateFileShareParams::new()
            .node(Some("n".to_owned()))
            .expires_at(Some(String::new()));
        assert!(
            matches!(empty_exp.validate(), Err(CliError::Parse(_))),
            "empty expires_at must be rejected"
        );
    }

    #[test]
    fn create_validate_rejects_literal_null_expires_at() {
        // F2-6: a directly-supplied `expires_at` trimming to case-insensitive
        // "null" must NOT smuggle in the clear sentinel on create.
        for literal in ["null", "NULL", "  Null  "] {
            let p = CreateFileShareParams::new()
                .node(Some("n".to_owned()))
                .expires_at(Some(literal.to_owned()));
            assert!(
                matches!(p.validate(), Err(CliError::Parse(_))),
                "expires_at={literal:?} must be rejected as a clear-sentinel bypass"
            );
        }
        // A genuine absolute datetime is still accepted.
        assert!(
            CreateFileShareParams::new()
                .node(Some("n".to_owned()))
                .expires_at(Some("2027-01-01 00:00:00".to_owned()))
                .validate()
                .is_ok()
        );
    }

    // ─── UpdateFileShareParams ──────────────────────────────────────────

    #[test]
    fn update_is_empty_detects_no_op() {
        assert!(UpdateFileShareParams::new().is_empty());
        assert!(
            !UpdateFileShareParams::new()
                .title(Some("t".to_owned()))
                .is_empty()
        );
        assert!(!UpdateFileShareParams::new().clear_password(true).is_empty());
        assert!(!UpdateFileShareParams::new().clear_expires(true).is_empty());
    }

    #[test]
    fn update_to_form_emits_clear_sentinels() {
        // clear_password → password="" ; clear_expires → expires_at="".
        let form = UpdateFileShareParams::new()
            .clear_password(true)
            .clear_expires(true)
            .to_form();
        assert_eq!(form.get("password").map(String::as_str), Some(""));
        assert_eq!(form.get("expires_at").map(String::as_str), Some(""));
        assert!(!form.contains_key("expires"));
    }

    #[test]
    fn update_to_form_prefers_explicit_password_over_no_clear() {
        let form = UpdateFileShareParams::new()
            .password(Some(SecretString::from("newpw".to_owned())))
            .to_form();
        assert_eq!(form.get("password").map(String::as_str), Some("newpw"));
    }

    #[test]
    fn update_to_form_relative_expiry_uses_expires_key() {
        let form = UpdateFileShareParams::new().expires(Some(7200)).to_form();
        assert_eq!(form.get("expires").map(String::as_str), Some("7200"));
        assert!(!form.contains_key("expires_at"));
    }

    #[test]
    fn update_validate_rejects_conflicting_expiry_intents() {
        assert!(
            UpdateFileShareParams::new()
                .expires(Some(3600))
                .clear_expires(true)
                .validate()
                .is_err()
        );
        assert!(
            UpdateFileShareParams::new()
                .expires_at(Some("2026-12-31 00:00:00".to_owned()))
                .clear_expires(true)
                .validate()
                .is_err()
        );
        assert!(
            UpdateFileShareParams::new()
                .expires(Some(3600))
                .expires_at(Some("2026-12-31 00:00:00".to_owned()))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn update_validate_rejects_password_and_clear_password() {
        assert!(
            UpdateFileShareParams::new()
                .password(Some(SecretString::from("p".to_owned())))
                .clear_password(true)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn update_validate_accepts_single_intents() {
        assert!(
            UpdateFileShareParams::new()
                .clear_expires(true)
                .validate()
                .is_ok()
        );
        assert!(
            UpdateFileShareParams::new()
                .clear_password(true)
                .validate()
                .is_ok()
        );
        assert!(
            UpdateFileShareParams::new()
                .expires(Some(60))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn update_validate_rejects_empty_title() {
        // F3-2: a directly-supplied empty title (`title=""`) is the server's
        // title-CLEAR sentinel; clearing is unsupported, so reject it (as Parse).
        let p = UpdateFileShareParams::new().title(Some(String::new()));
        assert!(
            matches!(p.validate(), Err(CliError::Parse(_))),
            "empty title must be rejected as Parse"
        );
        // A non-empty title still validates and round-trips through to_form.
        let ok = UpdateFileShareParams::new().title(Some("Renamed".to_owned()));
        assert!(ok.validate().is_ok());
        assert_eq!(
            ok.to_form().get("title").map(String::as_str),
            Some("Renamed")
        );
        // A whitespace title is the server's business, not the clear sentinel.
        assert!(
            UpdateFileShareParams::new()
                .title(Some("  ".to_owned()))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn update_validate_checks_access_option_membership() {
        // F3-7: an unknown access tier must be rejected client-side (as Parse).
        let bad = UpdateFileShareParams::new().access_option(Some("public".to_owned()));
        assert!(
            matches!(bad.validate(), Err(CliError::Parse(_))),
            "invalid access_option must be rejected as Parse"
        );
        for opt in ACCESS_OPTIONS {
            assert!(
                UpdateFileShareParams::new()
                    .access_option(Some((*opt).to_owned()))
                    .validate()
                    .is_ok(),
                "access_option {opt:?} must validate"
            );
        }
    }

    #[test]
    fn update_validate_rejects_directly_supplied_empty_password_and_expires_at() {
        // M2: clearing is ONLY via the explicit flags. A directly-supplied empty
        // `password` / `expires_at` (e.g. an unset env var) must be rejected so
        // it can never silently strip protection by sending a blank value.
        let empty_pw =
            UpdateFileShareParams::new().password(Some(SecretString::from(String::new())));
        assert!(
            matches!(empty_pw.validate(), Err(CliError::Parse(_))),
            "directly-supplied empty password must be rejected"
        );
        let empty_exp = UpdateFileShareParams::new().expires_at(Some(String::new()));
        assert!(
            matches!(empty_exp.validate(), Err(CliError::Parse(_))),
            "directly-supplied empty expires_at must be rejected"
        );
        // The clear FLAGS remain the sole way to clear — and they validate fine,
        // emitting the `password=""` / `expires_at=""` sentinel in to_form.
        assert!(
            UpdateFileShareParams::new()
                .clear_password(true)
                .validate()
                .is_ok()
        );
        let cleared = UpdateFileShareParams::new()
            .clear_password(true)
            .clear_expires(true)
            .to_form();
        assert_eq!(cleared.get("password").map(String::as_str), Some(""));
        assert_eq!(cleared.get("expires_at").map(String::as_str), Some(""));
    }

    #[test]
    fn update_validate_rejects_literal_null_expires_at() {
        // F2-6: a directly-supplied `expires_at` trimming to case-insensitive
        // "null" must NOT bypass the explicit --clear-expires flag.
        for literal in ["null", "NULL", "  nUlL  "] {
            let p = UpdateFileShareParams::new().expires_at(Some(literal.to_owned()));
            assert!(
                matches!(p.validate(), Err(CliError::Parse(_))),
                "expires_at={literal:?} must be rejected as a clear-sentinel bypass"
            );
        }
        // Clearing remains flag-only and still validates.
        assert!(
            UpdateFileShareParams::new()
                .clear_expires(true)
                .validate()
                .is_ok()
        );
        // A genuine absolute datetime is still accepted.
        assert!(
            UpdateFileShareParams::new()
                .expires_at(Some("2027-01-01 00:00:00".to_owned()))
                .validate()
                .is_ok()
        );
    }

    // ─── GrantParams ────────────────────────────────────────────────────

    #[test]
    fn grant_validate_add_requires_target_xor_and_capability() {
        // Neither target → error.
        assert!(
            GrantParams::new()
                .capability(Some("view".to_owned()))
                .validate_add()
                .is_err()
        );
        // Both targets → error.
        assert!(
            GrantParams::new()
                .user(Some("1".to_owned()))
                .email(Some("a@b.c".to_owned()))
                .capability(Some("view".to_owned()))
                .validate_add()
                .is_err()
        );
        // Missing capability → error (add).
        assert!(
            GrantParams::new()
                .user(Some("1".to_owned()))
                .validate_add()
                .is_err()
        );
        // Invalid capability → error.
        assert!(
            GrantParams::new()
                .user(Some("1".to_owned()))
                .capability(Some("admin".to_owned()))
                .validate_add()
                .is_err()
        );
        // Valid.
        for cap in CAPABILITIES {
            assert!(
                GrantParams::new()
                    .user(Some("1".to_owned()))
                    .capability(Some((*cap).to_owned()))
                    .validate_add()
                    .is_ok()
            );
        }
    }

    #[test]
    fn grant_validators_return_parse_not_config() {
        // F3-3: input-validation failures must be CliError::Parse (no misleading
        // "run configure init" hint), not CliError::Config.
        let no_target = GrantParams::new()
            .capability(Some("view".to_owned()))
            .validate_add()
            .expect_err("missing target");
        assert!(matches!(no_target, CliError::Parse(_)), "got {no_target:?}");

        let bad_cap = GrantParams::new()
            .user(Some("1".to_owned()))
            .capability(Some("admin".to_owned()))
            .validate_add()
            .expect_err("bad capability");
        assert!(matches!(bad_cap, CliError::Parse(_)), "got {bad_cap:?}");

        let remove_err = GrantParams::new()
            .validate_remove()
            .expect_err("missing target on remove");
        assert!(
            matches!(remove_err, CliError::Parse(_)),
            "got {remove_err:?}"
        );
    }

    #[test]
    fn grant_validate_remove_ignores_capability() {
        // Remove needs only the XOR target — capability is not required.
        assert!(
            GrantParams::new()
                .user(Some("1".to_owned()))
                .validate_remove()
                .is_ok()
        );
        assert!(
            GrantParams::new()
                .email(Some("a@b.c".to_owned()))
                .validate_remove()
                .is_ok()
        );
        assert!(GrantParams::new().validate_remove().is_err());
        assert!(
            GrantParams::new()
                .user(Some("1".to_owned()))
                .email(Some("a@b.c".to_owned()))
                .validate_remove()
                .is_err()
        );
    }

    #[test]
    fn grant_to_form_and_to_query_shapes() {
        // Add form: user + capability.
        let form = GrantParams::new()
            .user(Some("42".to_owned()))
            .capability(Some("edit".to_owned()))
            .to_form();
        assert_eq!(form.get("user").map(String::as_str), Some("42"));
        assert_eq!(form.get("capability").map(String::as_str), Some("edit"));
        assert!(!form.contains_key("email"));

        // Remove query: email only, NEVER capability.
        let query = GrantParams::new()
            .email(Some("a@b.c".to_owned()))
            .capability(Some("edit".to_owned()))
            .to_query();
        assert_eq!(query.get("email").map(String::as_str), Some("a@b.c"));
        assert!(!query.contains_key("user"));
        assert!(
            !query.contains_key("capability"),
            "grants DELETE query must never carry capability"
        );
    }

    // ─── Extractors ─────────────────────────────────────────────────────

    #[test]
    fn extractors_prefer_named_key_and_tolerate_response_wrapper() {
        let named = serde_json::json!({"result": true, "fileshare": {"fileshare": "1"}});
        assert!(extract_fileshare(&named).is_some());
        let wrapped = serde_json::json!({"response": {"fileshare": {"fileshare": "1"}}});
        assert!(extract_fileshare(&wrapped).is_some());

        let shares = serde_json::json!({"result": true, "fileshares": [{"fileshare": "1"}]});
        assert!(
            extract_fileshares(&shares)
                .and_then(Value::as_array)
                .is_some()
        );

        let grants = serde_json::json!({"result": true, "grants": [{"user": "1"}]});
        assert!(extract_grants(&grants).and_then(Value::as_array).is_some());

        let grant = serde_json::json!({"result": true, "grant": {"state": "pending"}});
        assert!(extract_grant(&grant).is_some());

        let user = serde_json::json!({"result": true, "user": {"id": "9"}});
        assert_eq!(
            extract_user(&user)
                .and_then(|u| u.get("id"))
                .and_then(Value::as_str),
            Some("9")
        );

        let versions = serde_json::json!({"result": true, "versions": [{"id": "v1"}]});
        assert!(
            extract_versions(&versions)
                .and_then(Value::as_array)
                .is_some()
        );

        let session = serde_json::json!({"result": true, "session": {"status": "complete"}});
        assert!(extract_session(&session).is_some());
    }

    #[test]
    fn extractors_return_none_when_absent() {
        let empty = serde_json::json!({"result": true});
        assert!(extract_fileshare(&empty).is_none());
        assert!(extract_fileshares(&empty).is_none());
        assert!(extract_grants(&empty).is_none());
        assert!(extract_grant(&empty).is_none());
        assert!(extract_user(&empty).is_none());
        assert!(extract_versions(&empty).is_none());
        assert!(extract_session(&empty).is_none());
    }

    #[test]
    fn parse_conflict_version_extracts_id_else_none() {
        assert_eq!(
            parse_conflict_version("CONFLICT_VERSION_MISMATCH:v9xQ2-abc12"),
            Some("v9xQ2-abc12")
        );
        // F3-8(b): the full hyphenated doc-shape id must round-trip intact (the
        // hyphens are part of the id, not delimiters the parser splits on).
        assert_eq!(
            parse_conflict_version("CONFLICT_VERSION_MISMATCH:v9xQ2-abc12-de345-fg678-hi901-jk234"),
            Some("v9xQ2-abc12-de345-fg678-hi901-jk234")
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(
            parse_conflict_version("CONFLICT_VERSION_MISMATCH: v7 "),
            Some("v7")
        );
        // Prefix present but no id → None (no false positive).
        assert_eq!(parse_conflict_version("CONFLICT_VERSION_MISMATCH:"), None);
        // A non-conflict message → None.
        assert_eq!(parse_conflict_version("complete"), None);
        assert_eq!(parse_conflict_version(""), None);
    }

    #[test]
    fn fileshare_file_name_reads_nested_file_name() {
        let details = serde_json::json!({
            "result": true,
            "fileshare": {
                "fileshare": "1",
                "file": {"id": "f1", "name": "report.pdf", "size": 10}
            }
        });
        assert_eq!(fileshare_file_name(&details).as_deref(), Some("report.pdf"));

        // Tolerates the response wrapper.
        let wrapped = serde_json::json!({
            "response": {"fileshare": {"file": {"name": "x.txt"}}}
        });
        assert_eq!(fileshare_file_name(&wrapped).as_deref(), Some("x.txt"));

        // Absent → None.
        let bare = serde_json::json!({"result": true, "fileshare": {"fileshare": "1"}});
        assert!(fileshare_file_name(&bare).is_none());
    }

    // ─── F4-1: management writes fail closed on a redirect (no body replay) ──

    /// Serve a fixed `status_line` + `Location` on every accepted connection,
    /// COUNTING each request, until the test drops the returned handle. Returns
    /// the bound `127.0.0.1:<port>` and a shared counter.
    ///
    /// Mirrors the `upload.rs` counting-server pattern (F4-2 fix): it DRAINS the
    /// full request (headers + declared `Content-Length` body) before responding,
    /// so under parallel test load the client's still-in-flight form write does
    /// not race the close and get a TCP RST (which would surface as a spurious
    /// `CliError::Parse` instead of the asserted fail-closed redirect error). The
    /// loop accepts repeatedly so a (buggy) body replay would be counted, not hang.
    async fn spawn_counting_redirect_server(
        status_line: &'static str,
        location: &'static str,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        let count = Arc::new(AtomicUsize::new(0));
        let count_srv = Arc::clone(&count);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let _ = count_srv.fetch_add(1, Ordering::SeqCst);
                // Drain the FULL request before responding (see doc above).
                let mut req_buf: Vec<u8> = Vec::with_capacity(4096);
                let mut chunk = [0u8; 4096];
                let mut header_end: Option<usize> = None;
                let mut content_length: Option<usize> = None;
                loop {
                    if let (Some(he), Some(cl)) = (header_end, content_length)
                        && req_buf.len() >= he + cl
                    {
                        break;
                    }
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            req_buf.extend_from_slice(&chunk[..n]);
                            if header_end.is_none()
                                && let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n")
                            {
                                let he = pos + 4;
                                header_end = Some(he);
                                let headers =
                                    String::from_utf8_lossy(&req_buf[..he]).to_ascii_lowercase();
                                content_length = headers
                                    .lines()
                                    .find_map(|line| {
                                        line.strip_prefix("content-length:")
                                            .map(str::trim)
                                            .and_then(|v| v.parse::<usize>().ok())
                                    })
                                    .or(Some(0));
                            }
                        }
                    }
                }
                let header = format!(
                    "HTTP/1.1 {status_line}\r\nLocation: {location}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (addr, count)
    }

    #[tokio::test]
    async fn create_fileshare_fails_closed_on_redirect_without_replaying_password() {
        use std::sync::atomic::Ordering;
        // F4-1: the create form can carry a `password` field. A 3xx on the
        // redirect-FOLLOWING client would replay the WHOLE form body (password
        // included) to the Location target. Routing through
        // `post_sensitive_form` (no-redirect, fail-closed) must surface a TERMINAL
        // CliError::Parse after EXACTLY ONE request, leaking neither the redirect
        // URL nor the password value.
        let (addr, count) =
            spawn_counting_redirect_server("307 Temporary Redirect", "http://example.invalid/leak")
                .await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let params = CreateFileShareParams::new()
            .node(Some("node1".to_owned()))
            .password(Some(SecretString::from("hunter2-SECRET".to_owned())));
        let err = create_fileshare(&client, "ws1", &params)
            .await
            .expect_err("a 3xx on create must fail closed, not be followed");

        match &err {
            CliError::Parse(msg) => {
                assert!(
                    !msg.contains("hunter2-SECRET"),
                    "the password must never appear in the error: {msg}"
                );
                assert!(
                    !msg.contains("example.invalid") && !msg.contains(&addr),
                    "neither the Location URL nor the request URL may appear in the error: {msg}"
                );
            }
            other => panic!("expected a fail-closed CliError::Parse, got {other:?}"),
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "a redirect must be served exactly once and the form body never replayed"
        );
    }

    #[tokio::test]
    async fn update_fileshare_fails_closed_on_redirect_without_replaying_password() {
        use std::sync::atomic::Ordering;
        // F4-1: identical posture for the update form, which can also carry a
        // `password` field.
        let (addr, count) =
            spawn_counting_redirect_server("308 Permanent Redirect", "http://example.invalid/leak")
                .await;
        let client = ApiClient::new(&format!("http://{addr}"), Some("tok".to_owned()))
            .expect("client builds");

        let params = UpdateFileShareParams::new()
            .password(Some(SecretString::from("hunter2-SECRET".to_owned())));
        let err = update_fileshare(&client, "FS1", &params)
            .await
            .expect_err("a 3xx on update must fail closed, not be followed");

        match &err {
            CliError::Parse(msg) => {
                assert!(
                    !msg.contains("hunter2-SECRET"),
                    "the password must never appear in the error: {msg}"
                );
                assert!(
                    !msg.contains("example.invalid") && !msg.contains(&addr),
                    "neither the Location URL nor the request URL may appear in the error: {msg}"
                );
            }
            other => panic!("expected a fail-closed CliError::Parse, got {other:?}"),
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "a redirect must be served exactly once and the form body never replayed"
        );
    }
}
