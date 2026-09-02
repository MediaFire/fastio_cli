/// Lock command implementations for `fastio lock *`.
///
/// Acquire, check, and release exclusive file locks
/// in workspaces or shares.
use anyhow::{Context, Result};

use super::CommandContext;
use super::secret_output::capture_or_redact_lock_token;
use fastio_cli::api;

/// Lock subcommand variants.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LockCommand {
    /// Acquire an exclusive lock on a file.
    Acquire {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
        /// File node ID.
        node_id: String,
        /// Lock duration in seconds (60-3600).
        duration: Option<u32>,
        /// Client metadata as a JSON object (e.g.
        /// `{"device_name":"…","client_version":"…"}`).
        client_info: Option<String>,
        /// Optional path to write the returned lock token to (0600). When
        /// absent the token is redacted from output.
        lock_token_file: Option<std::path::PathBuf>,
    },
    /// Check lock status for a file.
    Status {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
        /// File node ID.
        node_id: String,
    },
    /// Release a lock on a file.
    Release {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
        /// File node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        lock_token: String,
    },
    /// Renew (heartbeat) an existing lock on a file.
    Heartbeat {
        /// Context type: workspace or share.
        context_type: String,
        /// Context ID.
        context_id: String,
        /// File node ID.
        node_id: String,
        /// Lock token returned by the acquire command.
        lock_token: String,
    },
}

/// Execute a lock subcommand.
pub async fn execute(command: &LockCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        LockCommand::Acquire {
            context_type,
            context_id,
            node_id,
            duration,
            client_info,
            lock_token_file,
        } => {
            let mut v = api::locking::lock_acquire(
                &client,
                context_type,
                context_id,
                node_id,
                *duration,
                client_info.as_deref(),
            )
            .await
            .context("failed to acquire lock")?;
            // `lock_token` is a bearer capability — capture it to a 0600 file or
            // redact it, never render it. Shared with `files lock acquire` so the
            // two surfaces cannot diverge.
            capture_or_redact_lock_token(&mut v, lock_token_file.as_deref(), ctx.output.quiet)?;
            ctx.output.render(&v)?;
        }
        LockCommand::Status {
            context_type,
            context_id,
            node_id,
        } => {
            let v = api::locking::lock_status(&client, context_type, context_id, node_id)
                .await
                .context("failed to get lock status")?;
            ctx.output.render(&v)?;
        }
        LockCommand::Release {
            context_type,
            context_id,
            node_id,
            lock_token,
        } => {
            let v =
                api::locking::lock_release(&client, context_type, context_id, node_id, lock_token)
                    .await
                    .context("failed to release lock")?;
            ctx.output.render(&v)?;
        }
        LockCommand::Heartbeat {
            context_type,
            context_id,
            node_id,
            lock_token,
        } => {
            let v = api::locking::lock_heartbeat(
                &client,
                context_type,
                context_id,
                node_id,
                lock_token,
            )
            .await
            .context("failed to renew lock")?;
            ctx.output.render(&v)?;
        }
    }
    Ok(())
}
