//! How-To command implementation for `fastio how-to`.
//!
//! Thin command surface over [`fastio_cli::api::howto`]: ask a grounded
//! "how do I…" question about Fast.io and render the answer (or the clarifying
//! questions the server returns when the question is too vague).

use anyhow::{Context, Result};

use fastio_cli::api;

use super::CommandContext;

/// Execute the `how-to` command.
///
/// `question` is required (validated client-side for non-blank + length in the
/// API layer); `surface` optionally phrases the answer for an MCP or code-mode
/// client; `context` is optional free-text background. The response is rendered
/// as-is — the server returns either a grounded answer (`status: "answer"`) or a
/// clarification request (`status: "needs_clarification"`), and both are valid
/// HTTP-200 shapes the user reads directly from the output.
pub async fn execute(
    ctx: &CommandContext<'_>,
    question: &str,
    surface: Option<&str>,
    context: Option<&str>,
) -> Result<()> {
    let client = ctx.build_client()?;
    let value = api::howto::ask(&client, question, context, surface)
        .await
        .context("failed to ask how-to question")?;
    ctx.output.render(&value)?;
    Ok(())
}
