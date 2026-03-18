/// Import command implementations for `fastio import *`.
///
/// Manages cloud storage provider integrations: identities, sources,
/// sync jobs, and write-back operations.
///
/// NOTE: The import API may be temporarily disabled on the server.
/// Commands are implemented but may fail at runtime until re-enabled.
use anyhow::{Context, Result};

use super::CommandContext;
use fastio_cli::api;

/// Import subcommand variants.
#[derive(Debug, Clone)]
#[allow(clippy::too_many_lines)]
#[non_exhaustive]
pub enum ImportCommand {
    /// List available cloud import providers.
    ListProviders {
        /// Workspace ID.
        workspace_id: String,
    },
    /// List provider identities.
    ListIdentities {
        /// Workspace ID.
        workspace_id: String,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Provision a new provider identity.
    ProvisionIdentity {
        /// Workspace ID.
        workspace_id: String,
        /// Cloud provider: `google_drive`, `box`, `onedrive_business`, `dropbox`.
        provider: String,
    },
    /// Get identity details.
    IdentityDetails {
        /// Workspace ID.
        workspace_id: String,
        /// Identity ID.
        identity_id: String,
    },
    /// Revoke a provider identity.
    RevokeIdentity {
        /// Workspace ID.
        workspace_id: String,
        /// Identity ID.
        identity_id: String,
    },
    /// List import sources.
    ListSources {
        /// Workspace ID.
        workspace_id: String,
        /// Filter by status.
        status: Option<String>,
        /// Max results per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Discover shared folders.
    Discover {
        /// Workspace ID.
        workspace_id: String,
        /// Identity ID.
        identity_id: String,
    },
    /// Create an import source.
    CreateSource {
        /// Workspace ID.
        workspace_id: String,
        /// Identity ID.
        identity_id: String,
        /// Remote folder path.
        remote_path: String,
        /// Display name.
        remote_name: Option<String>,
        /// Sync interval in seconds (300-86400).
        sync_interval: Option<u32>,
        /// Access mode: `read_only` or `read_write`.
        access_mode: Option<String>,
    },
    /// Get source details.
    SourceDetails {
        /// Source ID.
        source_id: String,
    },
    /// Update source settings.
    UpdateSource {
        /// Source ID.
        source_id: String,
        /// Sync interval in seconds.
        sync_interval: Option<u32>,
        /// Status action: paused or synced.
        status: Option<String>,
        /// Display name.
        remote_name: Option<String>,
        /// Access mode.
        access_mode: Option<String>,
    },
    /// Delete a source.
    DeleteSource {
        /// Source ID.
        source_id: String,
    },
    /// Disconnect source with keep/delete.
    Disconnect {
        /// Source ID.
        source_id: String,
        /// Action: keep or delete.
        action: String,
    },
    /// Trigger immediate refresh.
    Refresh {
        /// Source ID.
        source_id: String,
    },
    /// List jobs for a source.
    ListJobs {
        /// Source ID.
        source_id: String,
        /// Max results.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Get job details.
    JobDetails {
        /// Source ID.
        source_id: String,
        /// Job ID.
        job_id: String,
    },
    /// Cancel a running job.
    CancelJob {
        /// Source ID.
        source_id: String,
        /// Job ID.
        job_id: String,
    },
    /// List write-back jobs.
    ListWritebacks {
        /// Source ID.
        source_id: String,
        /// Filter by status.
        status: Option<String>,
        /// Max results.
        limit: Option<u32>,
        /// Offset.
        offset: Option<u32>,
    },
    /// Get write-back details.
    WritebackDetails {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        writeback_id: String,
    },
    /// Push a file to remote.
    PushWriteback {
        /// Source ID.
        source_id: String,
        /// Node ID.
        node_id: String,
    },
    /// Retry a failed write-back.
    RetryWriteback {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        writeback_id: String,
    },
    /// Resolve a write-back conflict.
    ResolveConflict {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        writeback_id: String,
        /// Resolution: `keep_local` or `keep_remote`.
        resolution: String,
    },
    /// Cancel a pending write-back.
    CancelWriteback {
        /// Source ID.
        source_id: String,
        /// Write-back ID.
        writeback_id: String,
    },
}

/// Execute an import subcommand.
#[allow(clippy::too_many_lines)]
pub async fn execute(command: &ImportCommand, ctx: &CommandContext<'_>) -> Result<()> {
    let client = ctx.build_client()?;
    match command {
        ImportCommand::ListProviders { workspace_id } => {
            let v = api::import::list_providers(&client, workspace_id)
                .await
                .context("failed to list providers")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::ListIdentities {
            workspace_id,
            limit,
            offset,
        } => {
            let v = api::import::list_identities(&client, workspace_id, *limit, *offset)
                .await
                .context("failed to list identities")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::ProvisionIdentity {
            workspace_id,
            provider,
        } => {
            let v = api::import::provision_identity(&client, workspace_id, provider)
                .await
                .context("failed to provision identity")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::IdentityDetails {
            workspace_id,
            identity_id,
        } => {
            let v = api::import::identity_details(&client, workspace_id, identity_id)
                .await
                .context("failed to get identity details")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::RevokeIdentity {
            workspace_id,
            identity_id,
        } => {
            let v = api::import::revoke_identity(&client, workspace_id, identity_id)
                .await
                .context("failed to revoke identity")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::ListSources {
            workspace_id,
            status,
            limit,
            offset,
        } => {
            let v = api::import::list_sources(
                &client,
                workspace_id,
                status.as_deref(),
                *limit,
                *offset,
            )
            .await
            .context("failed to list sources")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::Discover {
            workspace_id,
            identity_id,
        } => {
            let v = api::import::discover(&client, workspace_id, identity_id)
                .await
                .context("failed to discover folders")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::CreateSource {
            workspace_id,
            identity_id,
            remote_path,
            remote_name,
            sync_interval,
            access_mode,
        } => {
            let v = api::import::create_source(
                &client,
                &api::import::CreateSourceParams {
                    workspace_id,
                    identity_id,
                    remote_path,
                    remote_name: remote_name.as_deref(),
                    sync_interval: *sync_interval,
                    access_mode: access_mode.as_deref(),
                },
            )
            .await
            .context("failed to create source")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::SourceDetails { source_id } => {
            let v = api::import::source_details(&client, source_id)
                .await
                .context("failed to get source details")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::UpdateSource {
            source_id,
            sync_interval,
            status,
            remote_name,
            access_mode,
        } => {
            let v = api::import::update_source(
                &client,
                source_id,
                *sync_interval,
                status.as_deref(),
                remote_name.as_deref(),
                access_mode.as_deref(),
            )
            .await
            .context("failed to update source")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::DeleteSource { source_id } => {
            let v = api::import::delete_source(&client, source_id)
                .await
                .context("failed to delete source")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::Disconnect { source_id, action } => {
            let v = api::import::disconnect_source(&client, source_id, action)
                .await
                .context("failed to disconnect source")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::Refresh { source_id } => {
            let v = api::import::refresh_source(&client, source_id)
                .await
                .context("failed to refresh source")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::ListJobs {
            source_id,
            limit,
            offset,
        } => {
            let v = api::import::list_jobs(&client, source_id, *limit, *offset)
                .await
                .context("failed to list jobs")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::JobDetails { source_id, job_id } => {
            let v = api::import::job_details(&client, source_id, job_id)
                .await
                .context("failed to get job details")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::CancelJob { source_id, job_id } => {
            let v = api::import::cancel_job(&client, source_id, job_id)
                .await
                .context("failed to cancel job")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::ListWritebacks {
            source_id,
            status,
            limit,
            offset,
        } => {
            let v = api::import::list_writebacks(
                &client,
                source_id,
                status.as_deref(),
                *limit,
                *offset,
            )
            .await
            .context("failed to list writebacks")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::WritebackDetails {
            source_id,
            writeback_id,
        } => {
            let v = api::import::writeback_details(&client, source_id, writeback_id)
                .await
                .context("failed to get writeback details")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::PushWriteback { source_id, node_id } => {
            let v = api::import::push_writeback(&client, source_id, node_id)
                .await
                .context("failed to push writeback")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::RetryWriteback {
            source_id,
            writeback_id,
        } => {
            let v = api::import::retry_writeback(&client, source_id, writeback_id)
                .await
                .context("failed to retry writeback")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::ResolveConflict {
            source_id,
            writeback_id,
            resolution,
        } => {
            let v = api::import::resolve_conflict(&client, source_id, writeback_id, resolution)
                .await
                .context("failed to resolve conflict")?;
            ctx.output.render(&v)?;
        }
        ImportCommand::CancelWriteback {
            source_id,
            writeback_id,
        } => {
            let v = api::import::cancel_writeback(&client, source_id, writeback_id)
                .await
                .context("failed to cancel writeback")?;
            ctx.output.render(&v)?;
        }
    }
    Ok(())
}
