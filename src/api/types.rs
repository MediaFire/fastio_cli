/// Shared API types for the Fast.io REST API.
///
/// Defines the standard response envelope and common data structures
/// used across multiple API domains.
use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

/// Sign-in response from `GET /user/auth/`.
#[derive(Deserialize)]
pub struct SignInResponse {
    /// Token lifetime in seconds.
    pub expires_in: i64,
    /// JWT access token.
    pub auth_token: String,
    /// Whether 2FA verification is required.
    #[serde(rename = "2factor", default)]
    pub two_factor: bool,
}

impl fmt::Debug for SignInResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInResponse")
            .field("expires_in", &self.expires_in)
            .field("auth_token", &"[REDACTED]")
            .field("two_factor", &self.two_factor)
            .finish()
    }
}

/// Token check response from `GET /user/auth/check/`.
#[derive(Debug, Deserialize)]
pub struct AuthCheckResponse {
    /// The user ID associated with the token.
    ///
    /// The API may return this as either a string or an integer,
    /// so we accept both and normalise to `String`.
    #[serde(deserialize_with = "deserialize_string_or_number")]
    pub id: String,
}

/// Accept a JSON string **or** a JSON number and always produce a `String`.
fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected string or number for id, got {other}"
        ))),
    }
}

/// Sign-up response (the account creation itself returns minimal data).
#[derive(Debug, Deserialize)]
pub struct SignUpResponse {
    /// Placeholder for any fields returned on account creation.
    #[allow(dead_code)]
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// 2FA verification response.
#[derive(Deserialize)]
pub struct TwoFactorVerifyResponse {
    /// Full-scope token after successful 2FA.
    pub auth_token: String,
    /// Token lifetime in seconds.
    pub expires_in: i64,
}

impl fmt::Debug for TwoFactorVerifyResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TwoFactorVerifyResponse")
            .field("auth_token", &"[REDACTED]")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// 2FA status response.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct TwoFactorStatusResponse {
    /// Current 2FA state (e.g., "enabled", "disabled").
    pub state: String,
    /// Whether TOTP is configured.
    #[serde(default)]
    pub totp: bool,
}

/// 2FA enable response.
#[derive(Debug, Deserialize)]
pub struct TwoFactorEnableResponse {
    /// TOTP binding URI (for QR code generation).
    pub binding_uri: Option<String>,
    /// Any extra fields.
    #[allow(dead_code)]
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// PKCE authorize response from `GET /oauth/authorize/`.
#[derive(Debug, Deserialize)]
pub struct PkceAuthorizeResponse {
    /// The authorization request ID.
    pub auth_request_id: String,
}

/// PKCE token exchange response from `POST /oauth/token/`.
#[derive(Deserialize)]
pub struct PkceTokenResponse {
    /// JWT access token.
    pub access_token: String,
    /// Token type (always "Bearer").
    #[allow(dead_code)]
    pub token_type: String,
    /// Token lifetime in seconds.
    pub expires_in: i64,
    /// Refresh token for obtaining new access tokens.
    pub refresh_token: Option<String>,
    /// Granted scope.
    #[allow(dead_code)]
    pub scope: Option<String>,
}

impl fmt::Debug for PkceTokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PkceTokenResponse")
            .field("access_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("scope", &self.scope)
            .finish()
    }
}

/// API key creation response.
#[derive(Deserialize)]
pub struct ApiKeyCreateResponse {
    /// The newly created API key.
    pub api_key: String,
}

impl fmt::Debug for ApiKeyCreateResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyCreateResponse")
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

/// API key listing response.
#[derive(Debug, Deserialize)]
pub struct ApiKeyListResponse {
    /// Number of keys returned.
    #[allow(dead_code)]
    pub results: u32,
    /// List of API key objects (nullable).
    pub api_keys: Option<Vec<serde_json::Value>>,
}

/// Generic empty/success response.
#[derive(Debug, Serialize, Deserialize)]
pub struct EmptyResponse {
    /// Any extra fields returned by the API.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ─── Search modes ────────────────────────────────────────────────────────────

/// Accepted `search_in` values (the search-mode selector).
pub const SEARCH_IN_VALUES: &[&str] = &["filename", "content", "both"];

/// Accepted `name_match` values (how the filename is matched).
pub const NAME_MATCH_VALUES: &[&str] = &["auto", "exact", "prefix", "contains", "glob"];

/// The `name_match` values that engage **precise** matching, as opposed to
/// `auto`'s layered relevance. These are the modes the server guards with a
/// pattern-length cap and an empty-pattern refusal, and the modes that require
/// the Elasticsearch alias swap to have happened in the target environment.
pub const NAME_MATCH_PRECISE: &[&str] = &["exact", "prefix", "contains", "glob"];

/// Maximum pattern length accepted by the precise `name_match` modes, in
/// **Unicode code points** (not bytes, not UTF-16 units).
///
/// The server applies this cap (and an empty/whitespace-only refusal) to **all
/// four** precise modes — not just `glob`. It does **not** apply to `auto` or to
/// a search with no `name_match`, so this must never be used to bound a plain
/// query: `/storage/search/` is deliberately uncapped and the CLI has always
/// forwarded queries longer than the unified endpoint's 1024-char limit.
///
/// **Over-limit behavior is a hard rejection, not truncation** — the server
/// returns error `144711` ("The search pattern may not exceed 256 characters
/// when `name_match` is not auto"). Mirroring it client-side therefore only turns
/// a round-trip 4xx into an immediate local error; it can never mask a result.
/// Verified against the API: 256 code points accepted and 257 rejected on all
/// four precise modes, with `auto` uncapped, and a 256-code-point CJK pattern
/// (766 bytes) accepted — which is what pins the unit to code points.
pub const MAX_PATTERN_LEN: usize = 256;

/// The three optional search-mode parameters shared by the `/storage/search/`
/// and unified `/search/` endpoint families.
///
/// Every field is optional and omitting all three reproduces today's behavior
/// byte-for-byte (`search_in=both`, `name_match=auto`) — the server enforces
/// that equivalence with a regression test against a literal historical query
/// body, so it is a guarantee rather than a convention.
///
/// **Escaping is the server's job.** `*` and `?` are metacharacters only under
/// `name_match=glob`; under `exact`, `prefix`, and `contains` the server escapes
/// them into literal filename characters. Callers must send the user's raw query
/// unmodified — pre-escaping client-side would corrupt the pattern, and three
/// clients hand-rolling it would produce three different behaviors.
///
/// Likewise, **do not lower-case the query** for `case_sensitive=false`: the
/// server normalizes it via a dedicated sub-field that folds non-ASCII
/// (accented, umlauted) filenames correctly, which a client-side
/// `to_lowercase()` would not reproduce for every locale.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SearchModeParams {
    /// `filename` | `content` | `both` (default `both` when omitted).
    pub search_in: Option<String>,
    /// `auto` | `exact` | `prefix` | `contains` | `glob` (default `auto`).
    pub name_match: Option<String>,
    /// Case-sensitive matching for the precise modes (default `false`).
    pub case_sensitive: Option<bool>,
}

impl SearchModeParams {
    /// An empty parameter set (all fields unset). Equivalent to
    /// [`Default::default`]; provided so callers in other crates can build the
    /// `#[non_exhaustive]` struct without struct-literal syntax.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `search_in`. The value is lower-cased (the server accepts the enum
    /// case-insensitively; normalizing keeps the wire form predictable).
    #[must_use]
    pub fn search_in(mut self, v: Option<&str>) -> Self {
        self.search_in = v.map(str::to_lowercase);
        self
    }

    /// Set `name_match` (lower-cased, as for [`Self::search_in`]).
    #[must_use]
    pub fn name_match(mut self, v: Option<&str>) -> Self {
        self.name_match = v.map(str::to_lowercase);
        self
    }

    /// Set `case_sensitive`. Only sent when explicitly supplied.
    #[must_use]
    pub fn case_sensitive(mut self, v: Option<bool>) -> Self {
        self.case_sensitive = v;
        self
    }

    /// Whether any search-mode parameter is set.
    ///
    /// Note that the response's `search_metadata` block is gated on `search_in`
    /// **alone** — supplying only `name_match` and/or `case_sensitive` leaves
    /// the response shape unchanged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.search_in.is_none() && self.name_match.is_none() && self.case_sensitive.is_none()
    }

    /// Whether a **precise** (non-`auto`) `name_match` mode is selected.
    ///
    /// These modes are inert until the target environment's Elasticsearch alias
    /// swap has run: before it, they return `200` with zero hits and **no
    /// error**, which is indistinguishable from a genuine no-match.
    #[must_use]
    pub fn is_precise(&self) -> bool {
        self.name_match
            .as_deref()
            .is_some_and(|m| NAME_MATCH_PRECISE.contains(&m))
    }

    /// Validate the mode values, and the query against the precise-mode pattern
    /// rules, before a request is sent.
    ///
    /// The pattern rules (256-char cap, no empty/whitespace-only pattern) apply
    /// **only** when a precise `name_match` is selected — a plain query is never
    /// length-checked here, which preserves the uncapped `/storage/search/`
    /// path.
    ///
    /// # Errors
    /// [`CliError::Parse`] when a mode value is outside its enum, or when a
    /// precise-mode pattern is empty or exceeds [`MAX_PATTERN_LEN`].
    pub fn validate(&self, query: &str) -> Result<(), CliError> {
        if let Some(v) = self.search_in.as_deref()
            && !SEARCH_IN_VALUES.contains(&v)
        {
            return Err(CliError::Parse(format!(
                "--search-in must be one of {} (got `{v}`)",
                SEARCH_IN_VALUES.join(", "),
            )));
        }
        if let Some(v) = self.name_match.as_deref()
            && !NAME_MATCH_VALUES.contains(&v)
        {
            return Err(CliError::Parse(format!(
                "--name-match must be one of {} (got `{v}`)",
                NAME_MATCH_VALUES.join(", "),
            )));
        }
        if self.is_precise() {
            let mode = self.name_match.as_deref().unwrap_or("auto");
            if query.trim().is_empty() {
                return Err(CliError::Parse(format!(
                    "--name-match {mode} requires a non-empty pattern"
                )));
            }
            let len = query.chars().count();
            if len > MAX_PATTERN_LEN {
                return Err(CliError::Parse(format!(
                    "--name-match {mode} pattern must be at most {MAX_PATTERN_LEN} characters \
                     (got {len}); omit --name-match to search without a length limit"
                )));
            }
        }
        Ok(())
    }

    /// Insert the set parameters into an outgoing query map.
    pub fn apply(&self, params: &mut HashMap<String, String>) {
        if let Some(v) = &self.search_in {
            params.insert("search_in".to_owned(), v.clone());
        }
        if let Some(v) = &self.name_match {
            params.insert("name_match".to_owned(), v.clone());
        }
        if let Some(v) = self.case_sensitive {
            params.insert("case_sensitive".to_owned(), v.to_string());
        }
    }
}

#[cfg(test)]
mod search_mode_tests {
    use super::*;

    #[test]
    fn apply_emits_only_set_params() {
        let mut q = HashMap::new();
        SearchModeParams::new().apply(&mut q);
        assert!(q.is_empty(), "an empty mode set must not alter the request");

        let mut q = HashMap::new();
        SearchModeParams::new()
            .search_in(Some("filename"))
            .name_match(Some("glob"))
            .case_sensitive(Some(true))
            .apply(&mut q);
        assert_eq!(q.get("search_in").map(String::as_str), Some("filename"));
        assert_eq!(q.get("name_match").map(String::as_str), Some("glob"));
        assert_eq!(q.get("case_sensitive").map(String::as_str), Some("true"));
    }

    #[test]
    fn enum_values_are_normalized_to_lowercase() {
        let p = SearchModeParams::new()
            .search_in(Some("FileName"))
            .name_match(Some("GLOB"));
        assert_eq!(p.search_in.as_deref(), Some("filename"));
        assert_eq!(p.name_match.as_deref(), Some("glob"));
        assert!(p.validate("*.pdf").is_ok());
    }

    #[test]
    fn invalid_enum_values_are_rejected() {
        assert!(
            SearchModeParams::new()
                .search_in(Some("contents"))
                .validate("q")
                .is_err()
        );
        assert!(
            SearchModeParams::new()
                .name_match(Some("regex"))
                .validate("q")
                .is_err()
        );
    }

    #[test]
    fn precise_modes_reject_empty_and_overlong_patterns() {
        for mode in NAME_MATCH_PRECISE {
            let p = SearchModeParams::new().name_match(Some(mode));
            assert!(p.validate("   ").is_err(), "{mode} must reject blank");
            let long: String = "x".repeat(MAX_PATTERN_LEN + 1);
            assert!(p.validate(&long).is_err(), "{mode} must reject >256");
            let ok: String = "x".repeat(MAX_PATTERN_LEN);
            assert!(p.validate(&ok).is_ok(), "{mode} must accept 256");
        }
    }

    #[test]
    fn non_precise_modes_do_not_length_check_the_query() {
        // `/storage/search/` is deliberately uncapped: a >1024-char query has
        // always worked and must keep working. Only precise modes are bounded.
        let long: String = "x".repeat(2000);
        assert!(SearchModeParams::new().validate(&long).is_ok());
        assert!(
            SearchModeParams::new()
                .name_match(Some("auto"))
                .validate(&long)
                .is_ok()
        );
        assert!(
            SearchModeParams::new()
                .search_in(Some("content"))
                .validate(&long)
                .is_ok()
        );
    }

    #[test]
    fn is_precise_detects_migration_gated_modes() {
        assert!(!SearchModeParams::new().is_precise());
        assert!(
            !SearchModeParams::new()
                .name_match(Some("auto"))
                .is_precise()
        );
        for mode in NAME_MATCH_PRECISE {
            assert!(SearchModeParams::new().name_match(Some(mode)).is_precise());
        }
    }
}
