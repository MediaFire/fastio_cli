#![allow(clippy::missing_errors_doc)]

/// Agent Intents API endpoints for the Fast.io REST API.
///
/// Short-lived, workspace-scoped slots that let an agent announce **what it is
/// doing** so peers see it before they collide rather than after. Deliberately
/// not called memory: the content is agent-authored and untrusted.
///
/// Five routes, all under `/workspace/{workspace_id}/intents/`:
/// allocate · browse · fill · expand · release.
///
/// ## Contract points that shape this module
///
/// * **`fill` IS the heartbeat.** Every write pushes `expires_at` forward and
///   there is no renewal verb, by design — so liveness is a side effect of
///   doing the work rather than a claim about it.
/// * **Unfilled slots are OCCUPANCY, not incomplete writes.** `state:
///   "allocated"` with a null topic means *"someone is starting something
///   here"* and must be surfaced, not filtered out.
/// * **`version` is REQUIRED on fill.** A stale one answers `409` / `9667`;
///   re-read and decide again rather than blind-retrying the same payload,
///   which is how a peer's write gets discarded.
/// * **Ownership is the USER, not the agent.** Sibling agents of one operator
///   may fill and release each other's slots. A `404` means *"no such intent,
///   or not yours (the user's)"* — never *"belongs to a different
///   credential"*.
use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Maximum `topic` length, in CHARACTERS (not bytes).
pub const TOPIC_MAX_CHARS: usize = 256;

/// Maximum `message` length, in CHARACTERS (not bytes).
pub const MESSAGE_MAX_CHARS: usize = 8192;

/// Build the `/workspace/{id}/intents/` collection path.
fn collection_path(workspace_id: &str) -> String {
    format!("/workspace/{}/intents/", urlencoding::encode(workspace_id),)
}

/// Build the `/workspace/{id}/intents/{intent_id}/` member path.
fn member_path(workspace_id: &str, intent_id: &str) -> String {
    format!(
        "/workspace/{}/intents/{}/",
        urlencoding::encode(workspace_id),
        urlencoding::encode(intent_id),
    )
}

/// Allocate an intent slot.
///
/// `POST /workspace/{workspace_id}/intents/`
///
/// Content-free by design: an agent allocates **honestly, before it knows what
/// it is doing**. Both parameters are optional scope hints.
///
/// **This is GET-OR-CREATE, keyed on (credential + scope) — it does NOT
/// mint a fresh slot per call.** With no `node_id` the scope is workspace-wide,
/// so a credential that already holds the workspace-wide slot gets **that
/// existing slot back**, content and all — possibly one this process never
/// created and has no memory of.
///
/// **Therefore: compare the returned `id` against the one you were holding
/// BEFORE you treat the slot as yours to overwrite or [`release`].** Releasing
/// an id you assumed was fresh destroys whatever content that slot already
/// carried, and the response cannot warn you — a release of someone else's
/// live intent returns exactly the same clean success as releasing your own.
///
/// *(Measured 2026-08-28: allocating with no `node_id` returned an
/// existing populated slot belonging to that credential; releasing it deleted
/// that content. The "allocate a scratch slot then release it" idiom is
/// therefore unsafe and must not be documented as a pattern.)*
///
/// Re-allocating a **live** slot is a pure heartbeat — nothing is destroyed,
/// and `state` / `version` / `topic` / `sequence` all hold while `expires_at`
/// moves forward (verified on the wire). Re-allocating a **dead** slot opens a
/// NEW generation (new `id`, `version` back to `0`, empty content) and
/// **nothing in the response marks that boundary** — another reason the
/// returned `id` is the only thing you can trust.
pub async fn allocate(
    client: &ApiClient,
    workspace_id: &str,
    node_id: Option<&str>,
    intent: Option<&str>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    if let Some(node_id) = node_id {
        form.insert("node_id".to_owned(), node_id.to_owned());
    }
    if let Some(intent) = intent {
        form.insert("intent".to_owned(), intent.to_owned());
    }
    client.post(&collection_path(workspace_id), &form).await
}

/// Browse intent slots — topics only, never message bodies.
///
/// `GET /workspace/{workspace_id}/intents/`
///
/// Ordered by allocation order, keyset paginated.
///
/// **`cursor: null` does NOT mean end-of-list, and is NOT equivalent to an
/// empty `items` array.** The cursor only advances past the write-visibility
/// boundary, so a page whose rows were all written in roughly the last two
/// seconds comes back with `cursor: null` **and a full `items` array**.
///
/// **`items` being empty is the ONLY end-of-list signal:**
/// * `items` non-empty ⇒ more to read. If `cursor` is null, re-poll with the
///   cursor you already had. **Do not stop.**
/// * `items` empty ⇒ end of list.
///
/// A loop that terminates on a null cursor silently truncates the feed, and
/// does so most often on a *busy* workspace — the case where missing rows
/// matters most.
///
/// **`sequence` is an OPAQUE cursor token**, not a count or a position.
/// Values are not contiguous. Do not render it and do not do arithmetic on it.
pub async fn browse(
    client: &ApiClient,
    workspace_id: &str,
    cursor: Option<&str>,
) -> Result<Value, CliError> {
    let mut params = HashMap::new();
    if let Some(cursor) = cursor {
        params.insert("cursor".to_owned(), cursor.to_owned());
    }
    if params.is_empty() {
        return client.get(&collection_path(workspace_id)).await;
    }
    client
        .get_with_params(&collection_path(workspace_id), &params)
        .await
}

/// Content fields for [`fill`].
///
/// Grouped into a struct rather than passed positionally: every field is
/// optional and same-typed, so a positional list is easy to transpose silently.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FillParams<'a> {
    /// One-line label, capped at [`TOPIC_MAX_CHARS`] CHARACTERS. The browse
    /// surface. Tabs and newlines are rejected — it is a label, not a body.
    pub topic: Option<&'a str>,
    /// Long-form body, capped at [`MESSAGE_MAX_CHARS`] CHARACTERS. Never
    /// returned by browse; read it with [`expand`].
    pub message: Option<&'a str>,
    /// Intent verb. Server-validated closed set — an unknown value is
    /// rejected, not ignored.
    pub intent: Option<&'a str>,
}

impl<'a> FillParams<'a> {
    /// Start from an empty set of content fields.
    ///
    /// The struct is `#[non_exhaustive]`, so the binary crate builds it
    /// through this builder rather than a struct literal — the same shape as
    /// [`crate::api::storage::SearchFilesParams`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the one-line `topic` label.
    #[must_use]
    pub fn topic(mut self, topic: Option<&'a str>) -> Self {
        self.topic = topic;
        self
    }

    /// Set the long-form `message` body.
    #[must_use]
    pub fn message(mut self, message: Option<&'a str>) -> Self {
        self.message = message;
        self
    }

    /// Set the `intent` verb.
    #[must_use]
    pub fn intent(mut self, intent: Option<&'a str>) -> Self {
        self.intent = intent;
        self
    }

    /// True when this fill carries only a `version` — the pure KEEPALIVE shape.
    ///
    /// **This is a valid operation, not an error to refuse.** [`fill`] still
    /// advances `version` and `expires_at` and leaves `state` untouched, so an
    /// unfilled slot is not flipped to `filled` with a null topic. An earlier
    /// revision of this comment called it a "no-op write" and told callers to
    /// refuse it "rather than silently burn a version" — both were wrong, and
    /// the CLI guard that acted on that advice has been removed. Do not
    /// re-introduce it: refusing this shape denies callers the documented way
    /// to stay alive when they have nothing new to say, and treating it as a
    /// no-op invites skipping the POST, which lets the slot expire.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.topic.is_none() && self.message.is_none() && self.intent.is_none()
    }
}

/// Fill or refine an allocated intent, and push its expiry forward.
///
/// `POST /workspace/{workspace_id}/intents/{intent_id}/`
///
/// Repeatable using the CURRENT version — an agent refines its topic as it
/// learns. **Not idempotent:** every successful fill advances `version` and
/// pushes `expires_at`, so re-sending the identical request fails on the stale
/// version rather than replaying harmlessly. **There is no separate renewal
/// endpoint, and fill IS the heartbeat** for a slot you already hold, so a
/// silent agent's intent evaporates because it went silent. (Fill is not the
/// only operation that renews — a repeat `allocate` of a live slot also pushes
/// `expires_at` forward — but fill is the one to use once you hold the slot.)
///
/// **Supplying none of `topic`, `message` or `intent` is a valid pure
/// KEEPALIVE**, not an empty request to refuse: `version` is always sent, so
/// the body is never empty, and the call still advances `version` and
/// `expires_at` while leaving `state` as-is — an unfilled slot is not flipped
/// to `filled` with a null topic. A field you omit is simply not sent; this
/// function inserts only the keys you supply. **What the server does with an
/// absent key is not published for THIS endpoint** — the contract's "leaves
/// `topic` and `message` exactly as they were" guarantee is written about a
/// repeat ALLOCATE, not about fill, so do not lean on a keepalive to preserve
/// a topic you could not re-send.
///
/// `version` is REQUIRED. A stale value is refused with `409` / `9667`
/// carrying the current state: **re-read and decide again**. Blind-retrying
/// the same payload is how a peer's concurrent write gets discarded.
///
/// `topic` is capped at [`TOPIC_MAX_CHARS`] and `message` at
/// [`MESSAGE_MAX_CHARS`], both counted in CHARACTERS. Control characters are
/// REJECTED at intake rather than stripped, and `topic` additionally rejects
/// tabs and newlines because it is a one-line label.
pub async fn fill(
    client: &ApiClient,
    workspace_id: &str,
    intent_id: &str,
    version: u64,
    params: &FillParams<'_>,
) -> Result<Value, CliError> {
    let mut form = HashMap::new();
    form.insert("version".to_owned(), version.to_string());
    if let Some(topic) = params.topic {
        form.insert("topic".to_owned(), topic.to_owned());
    }
    if let Some(message) = params.message {
        form.insert("message".to_owned(), message.to_owned());
    }
    if let Some(intent) = params.intent {
        form.insert("intent".to_owned(), intent.to_owned());
    }
    client
        .post(&member_path(workspace_id, intent_id), &form)
        .await
}

/// Maximum ids the server accepts on one expand call (see the published API docs).
///
/// Beyond this the server **silently ignores** the surplus and still answers
/// `200 OK` — there is no per-id error entry and no truncation flag, so an
/// over-long batch is indistinguishable from a batch whose extra ids had all
/// expired. That is why the client refuses rather than trims: a caller who
/// asked for 300 rows and silently got 250 has no way to notice.
pub const MAX_EXPAND_IDS: usize = 250;

/// Expand one or more intents in a single call, including `message` bodies.
///
/// `GET /workspace/{workspace_id}/intents/{id1},{id2},…/` — any number of ids
/// up to [`MAX_EXPAND_IDS`]; the `{id1},{id2},{id3}` rendering above is
/// illustrative, not an arity.
///
/// Browse deliberately omits `message`; this is how bodies are read. Batching
/// exists so a caller never issues one request per row — the context-budget
/// argument is the whole point.
///
/// Ids are joined with **literal commas**: the separator must not be
/// percent-encoded or the server sees one malformed id rather than several.
/// Each id is still encoded individually.
///
/// # Errors
///
/// Returns [`CliError::Parse`] when `intent_ids` is empty, or when it exceeds
/// [`MAX_EXPAND_IDS`] — see that constant for why this is an error and not a
/// silent trim.
pub async fn expand(
    client: &ApiClient,
    workspace_id: &str,
    intent_ids: &[String],
) -> Result<Value, CliError> {
    let path = expand_path(workspace_id, intent_ids)?;
    client.get(&path).await
}

/// Validate the batch and build the comma-joined expand path.
///
/// Split out from [`expand`] so the arity guard and the comma joining are
/// testable without a live client.
fn expand_path(workspace_id: &str, intent_ids: &[String]) -> Result<String, CliError> {
    if intent_ids.is_empty() {
        return Err(CliError::Parse(
            "expand requires at least one intent ID".to_owned(),
        ));
    }
    if intent_ids.len() > MAX_EXPAND_IDS {
        return Err(CliError::Parse(format!(
            "expand accepts at most {MAX_EXPAND_IDS} intent IDs per call, got {}. \
             The server silently ignores ids past {MAX_EXPAND_IDS} and still answers 200, \
             so the surplus would vanish without an error — split the batch instead.",
            intent_ids.len(),
        )));
    }
    let joined = intent_ids
        .iter()
        .map(|id| urlencoding::encode(id).into_owned())
        .collect::<Vec<String>>()
        .join(",");
    Ok(format!(
        "/workspace/{}/intents/{}/",
        urlencoding::encode(workspace_id),
        joined,
    ))
}

/// Release an intent slot — the work is done.
///
/// `DELETE /workspace/{workspace_id}/intents/{intent_id}/`
///
/// Releasing is advisory housekeeping, not a lock release: a stale intent
/// evaporates on its own once it stops being filled.
///
/// **A `DELETE` body is never read platform-wide** (`$_POST` is not
/// populated on `DELETE`), so any future parameter on this route belongs on
/// the query string. This route currently takes none.
pub async fn release(
    client: &ApiClient,
    workspace_id: &str,
    intent_id: &str,
) -> Result<Value, CliError> {
    client.delete(&member_path(workspace_id, intent_id)).await
}

#[cfg(test)]
mod tests {
    use super::{MAX_EXPAND_IDS, collection_path, expand_path, member_path};

    #[test]
    fn collection_path_encodes_the_workspace_id() {
        assert_eq!(
            collection_path("4627684912776422871"),
            "/workspace/4627684912776422871/intents/"
        );
    }

    #[test]
    fn member_path_encodes_both_segments() {
        assert_eq!(
            member_path("123", "aaztz-llec2-7epf6-qunzm-3ff7a-hcevk"),
            "/workspace/123/intents/aaztz-llec2-7epf6-qunzm-3ff7a-hcevk/"
        );
    }

    /// The hyphenated id form is what the server returns AND accepts back on
    /// writes, so it must survive path construction unchanged — a hyphen is
    /// not a reserved character and must not be percent-encoded.
    #[test]
    fn hyphenated_ids_are_not_mangled() {
        let path = member_path("1", "2y5kq-2owmd-2rlwm-exga5-rdesb-pepu");
        assert!(
            path.contains("2y5kq-2owmd-2rlwm-exga5-rdesb-pepu"),
            "hyphenated id was altered: {path}"
        );
        assert!(!path.contains('%'), "id was percent-encoded: {path}");
    }

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("id{i}")).collect()
    }

    /// Arity is NOT fixed at three — the `{id1},{id2},{id3}` in the doc comment
    /// is illustrative. Separators must be literal commas: percent-encoding
    /// them makes the server read one malformed id instead of several.
    #[test]
    fn expand_path_joins_arbitrarily_many_ids_with_literal_commas() {
        assert_eq!(
            expand_path("42", &ids(1)).expect("one id is valid"),
            "/workspace/42/intents/id0/"
        );
        assert_eq!(
            expand_path("42", &ids(5)).expect("five ids are valid"),
            "/workspace/42/intents/id0,id1,id2,id3,id4/"
        );
        assert!(
            !expand_path("42", &ids(5))
                .expect("five ids are valid")
                .contains("%2C"),
            "the comma separator must never be percent-encoded"
        );
    }

    /// THE REGRESSION THIS GUARD EXISTS FOR.
    ///
    /// The published API docs state: "Up to 250 ids per call; any beyond that
    /// are silently ignored." The server still answers 200 with no per-id error and no
    /// truncation flag, so before this guard a caller asking for 300 rows got
    /// 250 and could not tell. Refusing beats trimming: a silent trim would
    /// reproduce the server's own failure mode on the client side.
    #[test]
    fn expand_path_refuses_more_than_the_server_will_honour() {
        assert!(
            expand_path("42", &ids(MAX_EXPAND_IDS)).is_ok(),
            "exactly {MAX_EXPAND_IDS} must be accepted — the doc says 'up to', inclusive"
        );

        let err = expand_path("42", &ids(MAX_EXPAND_IDS + 1))
            .expect_err("one past the cap must be refused, not silently trimmed");
        let msg = err.to_string();
        assert!(
            msg.contains("251") && msg.contains("250"),
            "the error must name both the cap and what was passed, got: {msg}"
        );
    }

    #[test]
    fn expand_path_rejects_an_empty_batch() {
        assert!(expand_path("42", &[]).is_err());
    }
}
