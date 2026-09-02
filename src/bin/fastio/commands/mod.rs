//! Command implementations for the Fast.io CLI.
//!
//! Each sub-module handles a top-level command group (e.g., `auth`, `user`).

use std::path::Path;

use anyhow::Context;

use fastio_cli::output::OutputConfig;

/// Common parameters shared by all authenticated command handlers.
pub struct CommandContext<'a> {
    pub output: &'a OutputConfig,
    pub profile_name: &'a str,
    pub api_base: &'a str,
    pub flag_token: Option<&'a str>,
    pub config_dir: &'a Path,
}

impl CommandContext<'_> {
    /// Resolve authentication and build an API client.
    ///
    /// The client inherits the `--detail` server-verbosity level from the
    /// active [`OutputConfig`], so allowlisted envelope GETs append
    /// `?output=<detail>` automatically.
    pub fn build_client(&self) -> anyhow::Result<fastio_cli::client::ApiClient> {
        build_client_with_detail(
            self.api_base,
            self.profile_name,
            self.flag_token,
            self.config_dir,
            self.output.detail,
        )
    }
}

/// Resolve authentication and build an API client with an explicit
/// server-verbosity [`fastio_cli::output::OutputDetail`].
///
/// This is the shared helper used by every command module that needs an
/// authenticated HTTP client. Prefer [`CommandContext::build_client`], which
/// threads the active `--detail` automatically; call this directly only when
/// you have a detail level outside a [`CommandContext`].
pub fn build_client_with_detail(
    api_base: &str,
    profile_name: &str,
    flag_token: Option<&str>,
    config_dir: &Path,
    detail: Option<fastio_cli::output::OutputDetail>,
) -> anyhow::Result<fastio_cli::client::ApiClient> {
    let resolved = fastio_cli::auth::token::resolve_token(flag_token, profile_name, config_dir)
        .context("failed to resolve token")?;
    let t = resolved
        .ok_or_else(|| anyhow::anyhow!("authentication required. Run: fastio auth login"))?;
    fastio_cli::client::ApiClient::with_detail(api_base, Some(t), detail)
        .context("failed to create API client")
}

impl CommandContext<'_> {
    /// Build an API client that tolerates the absence of authentication, for
    /// File Share **consumption** reads (details / download / versions /
    /// preview) which may be served anonymously per the share's access tier.
    ///
    /// Auth resolution (File Share addendum F5):
    ///
    /// - A resolved token (`--token` / env / a live profile) → an authenticated
    ///   client, exactly as [`Self::build_client`].
    /// - No credentials at all (`resolve_token` → `Ok(None)`) → an ANONYMOUS
    ///   client (no bearer). An `anyone_with_link` share still serves.
    /// - EXPIRED stored PROFILE credentials (`resolve_token` →
    ///   `Err(CliError::Auth)`) → fall back to an anonymous client with a single
    ///   stderr warning (suppressed under `--quiet`). The user explicitly asked
    ///   to read a public link; an expired stored token should not hard-block a
    ///   read that may not need auth at all.
    ///
    /// An EXPLICIT `--token` / env token failure can only arrive here as an
    /// authenticated client (those are returned by `resolve_token` as
    /// `Ok(Some)` before any expiry check), so this never silently downgrades an
    /// explicit credential. Management, upload, ws-token, and activity stay on
    /// the always-authed [`Self::build_client`].
    pub fn build_client_allow_anonymous(&self) -> anyhow::Result<fastio_cli::client::ApiClient> {
        let token = match fastio_cli::auth::token::resolve_token(
            self.flag_token,
            self.profile_name,
            self.config_dir,
        ) {
            Ok(token) => token,
            Err(fastio_cli::error::CliError::Auth(_)) => {
                // Expired PROFILE credentials: proceed anonymously (the link may
                // be public). Warn once so the user knows the read was not
                // authenticated.
                if !self.output.quiet {
                    eprintln!(
                        "warning: stored credentials expired — proceeding without \
                         authentication (the File Share may require a link password or a \
                         grant; run `fastio auth login` to authenticate)"
                    );
                }
                None
            }
            Err(other) => {
                return Err(anyhow::Error::from(other).context("failed to resolve token"));
            }
        };
        fastio_cli::client::ApiClient::with_detail(self.api_base, token, self.output.detail)
            .context("failed to create API client")
    }

    /// Warn on stderr when the server reports that content search cannot work
    /// on this profile, so an empty result set is not mistaken for "no files
    /// matched."
    ///
    /// The distinction matters most to an agent: without this, a content search
    /// on a profile that cannot search content returns `200` with an empty list
    /// and reads as a confident "nothing found." The warning goes to **stderr**
    /// so `--format json|csv` output on stdout stays machine-parseable.
    ///
    /// `search_metadata` sits at the top level on the `/storage/search/`
    /// responses but inside the files bucket (`buckets.files.search_metadata`)
    /// on the unified ones, and is emitted only when `search_in` was explicitly
    /// supplied — both placements are handled, and its absence is not an error.
    pub fn warn_if_content_search_unavailable(&self, value: &serde_json::Value) {
        if self.output.quiet {
            return;
        }
        let meta = value
            .get("search_metadata")
            .or_else(|| value.pointer("/buckets/files/search_metadata"));
        let Some(meta) = meta else { return };
        if meta.get("content_search_available") != Some(&serde_json::Value::Bool(false)) {
            return;
        }
        // `reason` is share-only and its value set may grow; unknown values are
        // treated as opaque and fall back to the generic advice.
        let detail = match meta.get("reason").and_then(serde_json::Value::as_str) {
            Some("intelligence_disabled") => {
                " (AI intelligence is disabled here — an org admin can enable it)"
            }
            Some("summary_permission_denied") => {
                " (this share does not grant access to file summaries)"
            }
            _ => "",
        };
        eprintln!(
            "warning: content search is not available on this profile{detail} — \
             any empty result reflects that, not an absence of matching files. \
             Re-run with --filename-only to search filenames instead."
        );
    }

    /// Warn on stderr about the trustworthiness of a **metadata-filtered**
    /// search result. Call only when a `filters` value was actually sent.
    ///
    /// Three separate conditions, deliberately never collapsed into one
    /// "results may be incomplete" line, because the correct user action
    /// differs for each (see the published API docs):
    ///
    /// 1. **`metadata_filter` absent** — the filter never ran. The results are
    ///    **unfiltered**, not empty-because-nothing-matched. This is the one
    ///    that silently corrupts a conclusion: the response is a `200` with a
    ///    plausible file list and nothing else on it differs.
    /// 2. **`truncated`** — deterministic. The candidate set hit a cap and was
    ///    clipped; re-running returns the identical answer, so the advice is to
    ///    narrow, **never** to retry.
    /// 3. **`scope_incomplete`** — transient. A fault dropped genuinely
    ///    matching candidates, so re-running the same request is worth it.
    ///
    /// (2) and (3) are independent and can both be true at once. Warnings go to
    /// **stderr** so `--format json|csv` on stdout stays machine-parseable.
    pub fn warn_about_metadata_filter(&self, value: &serde_json::Value) {
        if self.output.quiet {
            return;
        }
        let Some(block) = fastio_cli::api::storage::metadata_filter_block(value) else {
            eprintln!(
                "warning: this response carries no `metadata_filter` acknowledgement, so the \
                 --filters predicate DID NOT run — these results are UNFILTERED, not a filtered \
                 result that matched few files. Either an intermediary dropped the `filters` \
                 parameter or this deployment does not accept it yet; do not read the list below \
                 as satisfying your filter."
            );
            return;
        };
        let flag = |name: &str| block.get(name) == Some(&serde_json::Value::Bool(true));
        if flag("truncated") {
            eprintln!(
                "warning: the filter's candidate set exceeded a cap and was clipped \
                 (`truncated`) — results are incomplete and `matched` is a floor, not an exact \
                 count. This is deterministic: re-running returns the same answer. Narrow the \
                 filter (add or tighten a clause) instead of retrying."
            );
        }
        if flag("scope_incomplete") {
            eprintln!(
                "warning: a transient fault dropped candidates that genuinely match \
                 (`scope_incomplete`) — this list is short for a reason unrelated to your filter. \
                 Re-run the same request; it may return more. Do not report this as complete."
            );
        }
    }
}

/// Parse an optional JSON-object argument, supporting an `@path` form that reads
/// the JSON from a file (`@@` escapes a literal leading `@`).
///
/// Shared by the command modules that accept inline `{json}` or `@file.json`
/// object arguments (e.g. `comment create`'s `--reference` / `--properties`), so
/// the resolver stays in one place rather than being re-implemented per module.
/// Returns `Ok(None)` when `raw` is absent.
pub(crate) fn parse_json_object_arg(
    raw: Option<&str>,
    label: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let Some(text) = resolve_json_arg_text(raw, label)? else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("{label} must be valid JSON"))?;
    anyhow::ensure!(
        value.is_object(),
        "{label} must be a JSON object (e.g. {{\"key\":\"value\"}})"
    );
    Ok(Some(value))
}

/// Resolve an optional JSON argument's raw text, supporting the `@path` form
/// that reads it from a file (`@@` escapes a literal leading `@`).
///
/// Extracted from [`parse_json_object_arg`] so the array variant shares the
/// exact same `@file` semantics rather than growing a second, subtly different
/// copy.
fn resolve_json_arg_text(raw: Option<&str>, label: &str) -> anyhow::Result<Option<String>> {
    let Some(raw) = raw else { return Ok(None) };
    let text = if let Some(path) = raw.strip_prefix('@') {
        if let Some(literal) = path.strip_prefix('@') {
            literal.to_owned()
        } else {
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {label} from file '{path}'"))?
        }
    } else {
        raw.to_owned()
    };
    Ok(Some(text))
}

/// Parse an optional JSON-**array**-of-objects argument, supporting the same
/// `@path` file form as [`parse_json_object_arg`].
///
/// Returns the argument's **original text** alongside the parsed value: callers
/// that forward the value to the API must send the text **verbatim** rather
/// than re-serializing the parsed `Value`, because a clause value may be an
/// integer above the range a round trip preserves exactly (see the published
/// API docs).
///
/// Validation is deliberately limited to **shape** — valid JSON, an array,
/// every element an object. Element *content* (clause count, operator names,
/// field vocabulary) is server policy that the contract flags as changeable, so
/// validating it here would make the CLI refuse requests the platform had begun
/// to accept.
///
/// An **empty array yields `None`**, not an error. The `filters` bullet list in
/// the published API docs defines `[]` as *"no filter at all: the search runs
/// unfiltered and no `metadata_filter` block is returned"*, so dropping it
/// reproduces the server's
/// own semantics exactly — and it keeps the caller out of the
/// "results are UNFILTERED" warning, which would otherwise fire for a caller who
/// asked for precisely that. Rejecting `[]` was the earlier behavior and it was
/// wrong: it encoded a client-side opinion about a shape the contract
/// explicitly blesses, the same mistake as pre-refusing
/// `filters` + `folders_scope`.
pub(crate) fn parse_json_object_array_arg(
    raw: Option<&str>,
    label: &str,
) -> anyhow::Result<Option<String>> {
    let Some(text) = resolve_json_arg_text(raw, label)? else {
        return Ok(None);
    };
    validate_json_object_array(&text, label)
}

/// Shape-validate an ALREADY-RESOLVED JSON-array-of-objects value, returning the
/// original text verbatim (or `None` for the empty array).
///
/// Split out of [`parse_json_object_array_arg`] so the built-in MCP server can
/// apply the IDENTICAL shape rules to its encoded-string `filters` spelling
/// without also inheriting [`resolve_json_arg_text`]'s **`@path` file form**.
/// That distinction is the whole point of the split: `@path` is a deliberate
/// convenience for a human typing a shell command, but wiring it into an MCP
/// parameter would hand a remote tool caller an arbitrary local-file read
/// through `filters`. The MCP surface gets the validation and **not** the file
/// access.
///
/// Takes `&str` and copies only on the success path, so the caller keeps
/// ownership of its own buffer.
pub(crate) fn validate_json_object_array(
    text: &str,
    label: &str,
) -> anyhow::Result<Option<String>> {
    let value: serde_json::Value =
        serde_json::from_str(text).with_context(|| format!("{label} must be valid JSON"))?;
    let Some(items) = value.as_array() else {
        anyhow::bail!(
            "{label} must be a JSON array of clause objects \
             (e.g. [{{\"field\":\"status\",\"operator\":\"=\",\"value\":\"open\"}}])"
        );
    };
    if items.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        items.iter().all(serde_json::Value::is_object),
        "every {label} entry must be a JSON object with `field` and `operator` keys"
    );
    Ok(Some(text.to_owned()))
}

/// How a poll loop should react to an error from one poll tick.
///
/// Distinguishes the three cases the previous `Err(_) => {}` collapsed into one
/// (silent loop-to-timeout):
/// - [`PollAction::RateLimited`] — honor the server's `retry_after`;
/// - [`PollAction::RetryTransient`] — a 5xx / network / I/O blip; back off and
///   retry on the next tick;
/// - [`PollAction::Fatal`] — a persistent, non-transient error (404 / 403 /
///   400 / parse / a non-rate-limit 4xx). Surface it instead of looping.
///
/// `pub(crate)` so the Ripley `ask`/`chat` and metadata `extract --wait` poll
/// loops (CLI + MCP) reuse the SAME classification rather than each
/// re-collapsing every error into a silent timeout.
pub(crate) enum PollAction {
    /// Server asked us to wait this many seconds before the next request.
    RateLimited { retry_after_secs: u64 },
    /// A transient failure worth one more poll on the regular cadence.
    RetryTransient,
    /// A persistent error the caller should see now (returned, not swallowed).
    Fatal(fastio_cli::error::CliError),
}

/// Classify a poll-tick [`CliError`] into a [`PollAction`].
///
/// The 401 re-auth short-circuit is handled by the caller before this is
/// reached. Rate limits sleep their advertised interval; all 5xx (`500..=599`),
/// request timeouts, transport, and I/O errors are transient; everything else
/// (4xx other than 408/429, parse, config) is fatal so a 404/403 no longer
/// loops silently to the deadline.
///
/// `pub(crate)` so the Ripley/metadata wait loops share this exact policy.
pub(crate) fn classify_poll_error(err: fastio_cli::error::CliError) -> PollAction {
    use fastio_cli::error::CliError;
    match err {
        CliError::RateLimit { retry_after_secs } => PollAction::RateLimited { retry_after_secs },
        CliError::Api(ref e) => match e.http_status {
            429 | 408 => PollAction::RateLimited {
                retry_after_secs: 0,
            },
            // All server errors are transient — a 500 during a long-running
            // poll is typically a momentary backend blip, not a permanent
            // condition, so it's worth another tick.
            500..=599 => PollAction::RetryTransient,
            _ => PollAction::Fatal(err),
        },
        // Transport/timeout and transient I/O are worth another tick.
        CliError::Http(_) | CliError::Io(_) => PollAction::RetryTransient,
        // Parse / config / auth(other) — and, conservatively, any future
        // non-exhaustive variant — are surfaced rather than looped.
        _ => PollAction::Fatal(err),
    }
}

/// AI chat and prompt commands.
pub mod ai;
/// Connected-app management commands.
pub mod apps;
/// Asset metadata and transformation commands.
pub mod asset;
/// Authentication commands (login, logout, status).
pub mod auth;
/// File and folder comment commands.
pub mod comment;
/// CLI configuration commands (profiles, defaults).
pub mod configure;
/// Per-workspace dashboard (actionable card feed) commands.
pub mod dashboard;
/// File and folder download commands.
pub mod download;
/// Audit and activity event commands.
pub mod event;
/// File and folder management commands.
pub mod files;
/// File Share (durable single-file link) commands.
pub mod fileshare;
/// How-To grounded product-guidance command (`fastio how-to`).
pub mod howto;
/// Offline OpaqueId classification command (`fastio id info`).
pub mod id;
/// External storage import commands.
pub mod import;
/// Agent Intents commands — announce what you are doing.
pub mod intents;
/// Workspace invitation commands.
pub mod invitation;
/// File locking commands.
pub mod lock;
/// Organization and workspace member commands.
pub mod member;
/// Metadata extraction, details, and search commands.
pub mod metadata;
/// Organization management commands.
pub mod org;
/// File preview commands.
pub mod preview;
/// Unified (grouped-bucket) search commands.
pub mod search;
/// Shared one-time-secret output helpers (extract / write 0600 / redact).
pub mod secret_output;
/// Share link management commands.
pub mod share;
/// E-signature (SignEnvelope) commands.
pub mod sign;
/// System health and status commands.
pub mod system;
/// File upload commands.
pub mod upload;
/// User profile commands.
pub mod user;
/// Terminal markdown viewer command (`fastio view`).
pub mod view;
/// Workspace management commands.
pub mod workspace;

#[cfg(test)]
mod tests {
    use super::{PollAction, classify_poll_error, parse_json_object_array_arg};
    use fastio_cli::error::CliError;

    fn api_err(http_status: u16) -> CliError {
        CliError::Api(fastio_cli::error::ApiError::new(
            0,
            None,
            "boom".to_owned(),
            http_status,
        ))
    }

    #[test]
    fn classify_poll_error_rate_limit_uses_retry_after() {
        match classify_poll_error(CliError::RateLimit {
            retry_after_secs: 12,
        }) {
            PollAction::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, 12),
            _ => panic!("rate limit must map to RateLimited"),
        }
        // A 429/408 Api error is also rate-limit-like.
        assert!(matches!(
            classify_poll_error(api_err(429)),
            PollAction::RateLimited { .. }
        ));
        assert!(matches!(
            classify_poll_error(api_err(408)),
            PollAction::RateLimited { .. }
        ));
    }

    #[test]
    fn classify_poll_error_5xx_transient_4xx_fatal() {
        // All 5xx (including 500) are transient — a momentary backend blip.
        for s in [500u16, 502, 503, 504, 599] {
            assert!(
                matches!(classify_poll_error(api_err(s)), PollAction::RetryTransient),
                "{s} should be transient"
            );
        }
        // Persistent client errors must be surfaced, not looped.
        for s in [400u16, 403, 404] {
            assert!(
                matches!(classify_poll_error(api_err(s)), PollAction::Fatal(_)),
                "{s} should be fatal"
            );
        }
        // Parse errors are fatal.
        assert!(matches!(
            classify_poll_error(CliError::Parse("x".to_owned())),
            PollAction::Fatal(_)
        ));
    }

    #[test]
    fn json_array_arg_absent_is_none() {
        assert!(
            parse_json_object_array_arg(None, "--filters")
                .expect("ok")
                .is_none()
        );
    }

    #[test]
    fn json_array_arg_returns_original_text_not_a_reserialization() {
        // The whole point of returning TEXT: a parse/re-serialize round trip
        // would silently change the request.
        //
        // 🔑 The SIGNIFICANT WHITESPACE is what makes this test able to fail.
        // `9007199254740993` fits in `i64`, so `serde_json` round-trips it
        // byte-for-byte — a fixture varying only the number would pass even for
        // a re-serializing implementation, asserting nothing. `to_string()`
        // always emits the compact form, so the spacing below cannot survive
        // one.
        let raw = r#"[ { "field": "n", "operator": "=", "value": 9007199254740993 } ]"#;
        let out = parse_json_object_array_arg(Some(raw), "--filters").expect("ok");
        assert_eq!(out.as_deref(), Some(raw));
    }

    #[test]
    fn json_array_arg_empty_array_is_no_filter_not_an_error() {
        // The `filters` bullet list in the published API docs DEFINES `[]` as "no
        // filter at all: the search runs unfiltered and no
        // `metadata_filter` block is returned". So it must
        // resolve to None — not an error (a client-side opinion about a shape
        // the contract blesses) and not a forwarded `[]` (which would trip the
        // "results are UNFILTERED" warning for a caller who asked for exactly
        // that). Dropping it reproduces the server's own semantics.
        assert!(
            parse_json_object_array_arg(Some("[]"), "--filters")
                .expect("empty array is legal")
                .is_none()
        );
        // Whitespace-padded and nested-whitespace forms behave identically.
        assert!(
            parse_json_object_array_arg(Some("  [ ]  "), "--filters")
                .expect("legal")
                .is_none()
        );
    }

    #[test]
    fn json_array_arg_rejects_object_and_scalar() {
        // A bare clause object is the likely mistake; it must not be silently
        // accepted or wrapped.
        for raw in [r#"{"field":"a","operator":"exists"}"#, "\"x\"", "7"] {
            assert!(
                parse_json_object_array_arg(Some(raw), "--filters").is_err(),
                "{raw} should be rejected"
            );
        }
    }

    #[test]
    fn json_array_arg_rejects_non_object_elements() {
        let err = parse_json_object_array_arg(Some(r#"["status=open"]"#), "--filters")
            .expect_err("rejected");
        assert!(err.to_string().contains("must be a JSON object"));
    }

    #[test]
    fn json_array_arg_rejects_malformed_json() {
        assert!(parse_json_object_array_arg(Some("[{"), "--filters").is_err());
    }

    #[test]
    fn json_array_arg_does_not_validate_server_policy() {
        // Six clauses exceeds the documented ceiling of five, and `nonsense` is
        // not a documented operator — both are SERVER policy that the contract
        // flags as changeable. The client must forward them and let the server
        // rule, or it would refuse requests the platform had begun accepting.
        let raw = r#"[{"field":"a","operator":"nonsense","value":1},
                      {"field":"b","operator":"="},{"field":"c","operator":"="},
                      {"field":"d","operator":"="},{"field":"e","operator":"="},
                      {"field":"f","operator":"="}]"#;
        assert!(parse_json_object_array_arg(Some(raw), "--filters").is_ok());
    }

    #[test]
    fn json_array_arg_reads_at_file_and_unescapes_double_at() {
        let dir = std::env::temp_dir().join(format!("fastio-filters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("f.json");
        let body = r#"[{"field":"category","operator":"=","value":"Legal"}]"#;
        std::fs::write(&path, body).expect("write");
        let at = format!("@{}", path.display());
        assert_eq!(
            parse_json_object_array_arg(Some(&at), "--filters")
                .expect("ok")
                .as_deref(),
            Some(body)
        );
        // `@@` escapes a literal leading `@` rather than reading a file: the
        // remaining text is parsed as inline JSON and returned verbatim.
        // (Deliberately NOT using `[]` as the oracle here — an empty array is
        // legal now and resolves to None, so it could not distinguish "escaped
        // and parsed" from "rejected".)
        assert_eq!(
            parse_json_object_array_arg(Some(&format!("@@{body}")), "--filters")
                .expect("literal @ escape parses as inline JSON")
                .as_deref(),
            Some(body)
        );
        // And a `@@` whose remainder is NOT valid JSON fails as JSON, proving
        // it was never treated as a path.
        assert!(parse_json_object_array_arg(Some("@@not-json"), "--filters").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
