/// Agent Intents command implementations for `fastio intents *`.
///
/// Allocate, browse, fill, expand, and release short-lived workspace-scoped
/// slots that announce what an agent is doing.
use anyhow::{Context, Result, ensure};

use super::CommandContext;
use fastio_cli::api;
use fastio_cli::api::intents::{MESSAGE_MAX_CHARS, TOPIC_MAX_CHARS};

/// Intents subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IntentsCommand {
    /// Allocate a slot, content-free.
    Allocate {
        /// Workspace ID.
        workspace: String,
        /// Optional node this intent is about.
        node_id: Option<String>,
        /// Optional intent verb (server-validated closed enum).
        intent: Option<String>,
    },
    /// Browse slots — topics only, no message bodies.
    List {
        /// Workspace ID.
        workspace: String,
        /// Keyset cursor from a previous page. The endpoint accepts no
        /// page-size parameter, so there is deliberately none here.
        cursor: Option<String>,
    },
    /// Fill or refine a slot; also pushes its expiry forward.
    Fill {
        /// Workspace ID.
        workspace: String,
        /// Intent ID returned by allocate.
        intent_id: String,
        /// Version read from the intent; a stale value is refused.
        version: u64,
        /// One-line label, the browse surface.
        topic: Option<String>,
        /// Long-form body, never returned by browse.
        message: Option<String>,
        /// Intent verb (server-validated closed enum). Scope is fixed at
        /// allocation and cannot be re-set by a fill, so no `node_id` here.
        intent: Option<String>,
    },
    /// Expand one or more intents, including message bodies.
    Get {
        /// Workspace ID.
        workspace: String,
        /// One or more intent IDs, expanded in a single call.
        intent_ids: Vec<String>,
    },
    /// Release a slot.
    Release {
        /// Workspace ID.
        workspace: String,
        /// Intent ID.
        intent_id: String,
    },
}

/// Reject text the server would refuse, with a local message that names the
/// bound — counted in CHARACTERS, because that is how the server counts.
///
/// The server is authoritative and rejects (never strips) control characters
/// at intake; this only turns the common length mistake into an immediate,
/// legible error instead of a round trip.
fn check_len(field: &str, value: &str, max: usize) -> Result<()> {
    let chars = value.chars().count();
    ensure!(
        chars <= max,
        "{field} must be at most {max} characters (got {chars}) — counted in \
         characters, not bytes"
    );
    Ok(())
}

/// Execute an intents subcommand.
pub async fn execute(command: &IntentsCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        IntentsCommand::Allocate {
            workspace,
            node_id,
            intent,
        } => {
            let v =
                api::intents::allocate(&client, workspace, node_id.as_deref(), intent.as_deref())
                    .await
                    .context("failed to allocate intent")?;
            ctx.output.render(&v)?;
        }
        IntentsCommand::List { workspace, cursor } => {
            let v = api::intents::browse(&client, workspace, cursor.as_deref())
                .await
                .context("failed to browse intents")?;
            ctx.output.render(&v)?;
        }
        IntentsCommand::Fill {
            workspace,
            intent_id,
            version,
            topic,
            message,
            intent,
        } => {
            if let Some(topic) = topic {
                check_len("topic", topic, TOPIC_MAX_CHARS)?;
                // `topic` is a ONE-LINE label and the server rejects tabs and
                // newlines in it. Caught locally so the failure names the
                // reason rather than arriving as a generic intake refusal.
                ensure!(
                    !topic.contains(['\n', '\r', '\t']),
                    "topic must be a single line — it is a label, so tabs and \
                     newlines are rejected. Put detail in --message instead."
                );
            }
            if let Some(message) = message {
                check_len("message", message, MESSAGE_MAX_CHARS)?;
            }
            let params = api::intents::FillParams::new()
                .topic(topic.as_deref())
                .message(message.as_deref())
                .intent(intent.as_deref());
            // A fill carrying only `--version` is NOT an error: the contract
            // defines it as a pure KEEPALIVE that still advances `version` and
            // `expires_at` while leaving `state` untouched, so an unfilled slot
            // is not flipped to `filled` with a null topic. This previously
            // bailed, which refused a documented operation — and the refusal
            // text asserted "a no-op write is not a renewal", the opposite of
            // what the contract says it does.
            let v = api::intents::fill(&client, workspace, intent_id, *version, &params)
                .await
                .context("failed to fill intent")?;
            ctx.output.render(&v)?;
        }
        IntentsCommand::Get {
            workspace,
            intent_ids,
        } => {
            let v = api::intents::expand(&client, workspace, intent_ids)
                .await
                .context("failed to expand intents")?;
            ctx.output.render(&v)?;
        }
        IntentsCommand::Release {
            workspace,
            intent_id,
        } => {
            let v = api::intents::release(&client, workspace, intent_id)
                .await
                .context("failed to release intent")?;
            ctx.output.render(&v)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_len;

    /// The bound is in CHARACTERS, not bytes — a multi-byte string at the
    /// character limit must pass, or a caller writing non-ASCII gets refused
    /// well below the documented cap.
    #[test]
    fn length_is_counted_in_characters_not_bytes() {
        let four_byte_each = "𝄞".repeat(256);
        assert!(four_byte_each.len() > 256, "test string must be multi-byte");
        assert!(check_len("topic", &four_byte_each, 256).is_ok());
        assert!(check_len("topic", &"a".repeat(257), 256).is_err());
    }

    #[test]
    fn length_error_names_the_field_and_the_bound() {
        let err = check_len("message", &"x".repeat(9000), 8192)
            .expect_err("over-long message must be refused");
        let text = format!("{err}");
        assert!(
            text.contains("message"),
            "error must name the field: {text}"
        );
        assert!(text.contains("8192"), "error must name the bound: {text}");
    }
}
