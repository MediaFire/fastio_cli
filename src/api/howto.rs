#![allow(clippy::missing_errors_doc)]

//! How-To API endpoint for the Fast.io REST API.
//!
//! Maps to the org-less, user-authenticated How-To surface documented at
//! `~/vividengine/llms/howto.txt`: ask a natural-language "how do I…" question
//! about Fastio and get a grounded, product-aware answer back in a single call.
//! The answer is generated over Fastio's own how-to knowledge, so an agent gets
//! usage guidance without scraping the docs.
//!
//! This is a **top-level** endpoint — `POST /current/how-to/`, NOT
//! workspace/org-scoped — and is **open access** (any authenticated user; no
//! org-membership, plan-feature, or subscription gate; never billed). The only
//! access bound is the per-user rate limit.
//!
//! The response is one of two HTTP-200 shapes: a grounded answer
//! (`status: "answer"`) or a request for clarification
//! (`status: "needs_clarification"`). The command/MCP layers branch on `status`
//! and surface the clarifying questions when present.

use std::collections::HashMap;

use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// Maximum length of a how-to `question`, in characters (howto.txt:50).
pub const MAX_QUESTION_LEN: usize = 2000;

/// Maximum length of the optional free-text `context`, in characters
/// (howto.txt:51).
pub const MAX_CONTEXT_LEN: usize = 8000;

/// Validate a how-to `question` against the server contract: non-blank after
/// trimming and at most [`MAX_QUESTION_LEN`] characters (howto.txt:50). Catches
/// the two common client-side failures before the network so the caller gets a
/// clear message rather than a `147185` round-trip.
fn validate_question(question: &str) -> Result<(), CliError> {
    if question.trim().is_empty() {
        return Err(CliError::Parse(
            "how-to question must not be blank".to_owned(),
        ));
    }
    // Measure the TRIMMED length to match the server (`HowToService` trims then
    // `mb_strlen`s) — otherwise trailing whitespace could trip a client-side
    // false-negative on a question the server would accept.
    if question.trim().chars().count() > MAX_QUESTION_LEN {
        return Err(CliError::Parse(format!(
            "how-to question must be at most {MAX_QUESTION_LEN} characters"
        )));
    }
    Ok(())
}

/// Validate the optional `context` length cap (howto.txt:51).
fn validate_context(context: &str) -> Result<(), CliError> {
    if context.chars().count() > MAX_CONTEXT_LEN {
        return Err(CliError::Parse(format!(
            "how-to context must be at most {MAX_CONTEXT_LEN} characters"
        )));
    }
    Ok(())
}

/// Build the form body for [`ask`]. Extracted as a pure function so the
/// parameter wiring is testable without a network round-trip. Only non-empty
/// optional fields are inserted (an omitted `surface`/`context` falls back to
/// the server defaults: REST-API phrasing and no background context).
fn build_form(
    question: &str,
    context: Option<&str>,
    surface: Option<&str>,
) -> HashMap<String, String> {
    let mut form = HashMap::new();
    form.insert("question".to_owned(), question.to_owned());
    if let Some(c) = context {
        form.insert("context".to_owned(), c.to_owned());
    }
    if let Some(s) = surface {
        form.insert("surface".to_owned(), s.to_owned());
    }
    form
}

/// Ask a single how-to question.
///
/// `POST /current/how-to/` (form-encoded, `application/x-www-form-urlencoded`).
///
/// `question` is REQUIRED (1–[`MAX_QUESTION_LEN`] characters, non-blank;
/// validated client-side). `context` is optional free-text background about the
/// caller's situation (treated strictly as data, never as instructions; capped
/// at [`MAX_CONTEXT_LEN`]). `surface` optionally phrases the answer for a
/// specific client: `"mcp"` → in terms of the Fastio MCP consolidated tools,
/// `"code"` → as execute-proxy calls for a code-mode agent; omit for the
/// default REST-API phrasing.
///
/// The returned [`Value`] is one of the two documented 200 shapes — an answer
/// (`status: "answer"`, read `answer`) or a clarification request
/// (`status: "needs_clarification"`, surface `questions`). The caller branches
/// on `status`; a clarification is a normal outcome, not an error.
pub async fn ask(
    client: &ApiClient,
    question: &str,
    context: Option<&str>,
    surface: Option<&str>,
) -> Result<Value, CliError> {
    validate_question(question)?;
    if let Some(c) = context {
        validate_context(c)?;
    }
    let form = build_form(question, context, surface);
    client.post("/how-to/", &form).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_question_rejects_blank() {
        assert!(validate_question("").is_err());
        assert!(validate_question("   ").is_err());
        assert!(validate_question("How do I create a share?").is_ok());
    }

    #[test]
    fn validate_question_rejects_oversized() {
        let too_long: String = "x".repeat(MAX_QUESTION_LEN + 1);
        assert!(validate_question(&too_long).is_err());
        let at_cap: String = "x".repeat(MAX_QUESTION_LEN);
        assert!(validate_question(&at_cap).is_ok());
    }

    #[test]
    fn validate_context_enforces_cap() {
        let too_long: String = "y".repeat(MAX_CONTEXT_LEN + 1);
        assert!(validate_context(&too_long).is_err());
        let at_cap: String = "y".repeat(MAX_CONTEXT_LEN);
        assert!(validate_context(&at_cap).is_ok());
    }

    #[test]
    fn build_form_includes_question_only_by_default() {
        let form = build_form("How do I sign a doc?", None, None);
        assert_eq!(
            form.get("question").map(String::as_str),
            Some("How do I sign a doc?")
        );
        assert!(!form.contains_key("context"));
        assert!(!form.contains_key("surface"));
    }

    #[test]
    fn build_form_includes_context_and_surface_when_set() {
        let form = build_form("q", Some("trying to deliver a report"), Some("mcp"));
        assert_eq!(
            form.get("context").map(String::as_str),
            Some("trying to deliver a report")
        );
        assert_eq!(form.get("surface").map(String::as_str), Some("mcp"));
    }
}
