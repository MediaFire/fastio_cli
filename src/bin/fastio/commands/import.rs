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

// ─── Cloud-import kill-switch ───────────────────────────────────────────────

/// Cloud-import kill-switch: the import surface is disabled by default because
/// cloud import has not launched yet; `FASTIO_ENABLE_CLOUD_IMPORT=1` re-enables
/// it for operators and dev testing.
///
/// The platform enforces its own gate server-side (`CLOUD_IMPORT_DEPLOY_MODE`
/// resolves to dev environments only), so this local switch is belt-and-braces
/// and controls only surface visibility/dispatch. Mirrors the E-Sign switch in
/// [`crate::commands::sign::esign_enabled`].
pub(crate) fn cloud_import_enabled() -> bool {
    cloud_import_enabled_from(std::env::var("FASTIO_ENABLE_CLOUD_IMPORT").ok().as_deref())
}

/// Testable core: enabled iff the value is exactly "1".
pub(crate) fn cloud_import_enabled_from(value: Option<&str>) -> bool {
    value == Some("1")
}

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
        /// Microsoft account family (`work` | `personal`); `onedrive_business` only.
        account_type: Option<String>,
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
    /// List stored document libraries (drives) an identity can reach.
    ListDrives {
        /// Workspace ID.
        workspace_id: String,
        /// Identity ID.
        identity_id: String,
        /// Filter stored rows by discovered site path.
        site_path: Option<String>,
        /// Max drives per page.
        limit: Option<u32>,
        /// Offset for pagination.
        offset: Option<u32>,
    },
    /// Rebuild an identity's drive catalog from the provider (async).
    RefreshDrives {
        /// Workspace ID.
        workspace_id: String,
        /// Identity ID.
        identity_id: String,
        /// Site path to enumerate.
        site_path: Option<String>,
    },
    /// Discover shared folders.
    Discover {
        /// Workspace ID.
        workspace_id: String,
        /// Identity ID.
        identity_id: String,
        /// Drive ID to enumerate (`onedrive_business`).
        drive_id: Option<String>,
        /// Enumerate inside this folder instead of the provider root.
        remote_path: Option<String>,
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
        /// Drive ID to bind the source to (`onedrive_business`; create-time only).
        drive_id: Option<String>,
        /// Storage folder to graft the import under (create-time only).
        destination_node_id: Option<String>,
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
            account_type,
        } => {
            let v = api::import::provision_identity(
                &client,
                workspace_id,
                provider,
                account_type.as_deref(),
            )
            .await
            .context("failed to provision identity")?;
            ctx.output.render(&v)?;
            // Being handed an existing connection is a 200 that looks like
            // progress, so an ignored `--account-type` leaves no trace in the
            // rendered output. Say so on stderr; stdout stays a data stream.
            if !ctx.output.quiet
                && let Some(hint) = account_type_ignored_hint(account_type.as_deref(), &v)
            {
                eprintln!("{hint}");
            }
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
        ImportCommand::RefreshDrives {
            workspace_id,
            identity_id,
            site_path,
        } => {
            let v = api::import::refresh_drives(
                &client,
                workspace_id,
                identity_id,
                site_path.as_deref(),
            )
            .await
            .context("failed to start drive refresh")?;
            ctx.output.render(&v)?;
            if !ctx.output.quiet {
                eprintln!(
                    "Refresh started. It runs in the background and returns no job id — poll \
                     `import list-drives` until drives_state leaves `refreshing`."
                );
            }
        }
        ImportCommand::ListDrives {
            workspace_id,
            identity_id,
            site_path,
            limit,
            offset,
        } => {
            let v = api::import::list_drives(
                &client,
                workspace_id,
                identity_id,
                site_path.as_deref(),
                *limit,
                *offset,
            )
            .await
            .context("failed to list drives")?;
            ctx.output.render(&v)?;
            // `requires_site_path` is a scalar sibling of the `drives` array,
            // and the table/CSV flattener returns only the first array it
            // finds — so it is dropped in exactly the formats a person reads
            // interactively, leaving an empty table with no explanation. Emit
            // the reason on stderr, which keeps stdout a clean data stream.
            if !ctx.output.quiet
                && let Some(hint) = drives_state_hint(&v)
            {
                eprintln!("{hint}");
            }
        }
        ImportCommand::Discover {
            workspace_id,
            identity_id,
            drive_id,
            remote_path,
        } => {
            let v = api::import::discover(
                &client,
                workspace_id,
                identity_id,
                drive_id.as_deref(),
                remote_path.as_deref(),
            )
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
            drive_id,
            destination_node_id,
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
                    drive_id: drive_id.as_deref(),
                    destination_node_id: destination_node_id.as_deref(),
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

/// Explain an `--account-type` the server did not apply.
///
/// One connection exists per user per workspace per provider, and `work` and
/// `personal` share the single `onedrive_business` provider value — so
/// provisioning when one already exists hands back that existing row. That
/// response is a success, and the account type is recorded only where a connect
/// actually begins, so the requested value is dropped with nothing in the
/// rendered output to show it. Returns the line to print on stderr, or `None`
/// when nothing was ignored.
fn account_type_ignored_hint(requested: Option<&str>, value: &serde_json::Value) -> Option<String> {
    // Nothing to ignore if the caller never asked for one.
    let requested = requested?;
    // Only an EXISTING row drops it; where a connect begins, it is stored.
    if value
        .get("already_exists")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let identity = value.get("identity")?;
    // The field is meaningful only for OneDrive. Other providers accept the
    // flag and ignore it always, so naming a OneDrive remedy under one of them
    // would describe a connection the caller does not have.
    if identity.get("provider").and_then(serde_json::Value::as_str) != Some("onedrive_business") {
        return None;
    }
    // Absent means the connection predates the field or took the default;
    // either way it is not `personal`.
    //
    // Reading absence as a DEFAULT rather than as "hidden from you" is only
    // sound because the provision response serializes the full properties bag
    // to every caller. The identity list and details endpoints strip
    // provider-internal properties for a non-owner, non-admin member, where
    // absent would mean "redacted" and this would state a guess as fact. Do
    // not point this helper at those responses.
    let stored = identity
        .get("properties")
        .and_then(|p| p.get("account_type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("work");
    // Asking for what it already is changes nothing, so there is nothing to
    // report — the caller got the connection they described.
    if stored == requested {
        return None;
    }
    Some(format!(
        "Note: --account-type {requested} was not applied. This returned the connection that \
         already exists, and it stays {stored} — a connection keeps the account type it was \
         connected with. One connection exists per user per workspace, and work and personal \
         share it, so connecting as {requested} means revoking that connection first and \
         provisioning again."
    ))
}

/// Explain an empty `list-drives` result.
///
/// The drives response pairs a `drives` array with a `requires_site_path`
/// flag, but table/CSV rendering keeps only the first array it finds, so the
/// flag never reaches the screen. Returns the line to print on stderr, or
/// `None` when the result speaks for itself.
fn drives_state_hint(value: &serde_json::Value) -> Option<String> {
    // Only comment on a well-formed drives response. Anything else — an error
    // envelope, an unexpected shape — already speaks for itself, and guessing
    // at it would put a misleading explanation under a real error.
    value.get("drives").and_then(serde_json::Value::as_array)?;
    let error = value
        .get("drives_error")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());

    // Branch on `drives_state`, never on list length: an empty list carries
    // five distinct meanings and a populated one can still be incomplete.
    match value.get("drives_state").and_then(serde_json::Value::as_str) {
        Some("never_refreshed") => Some(
            "The drive catalog has not been built yet — this is normal on a new connection. \
             Run `import refresh-drives` to build it."
                .to_owned(),
        ),
        Some("refreshing") => Some(
            "A drive refresh is in progress. Re-run `import list-drives` to poll; the catalog \
             appears once drives_state leaves `refreshing`."
                .to_owned(),
        ),
        Some("requires_site_path") => Some(
            "Nothing was refused — there is simply no listing to ask for, so the site has to be \
             named. This is not a permission problem and checking the grant will not resolve it. \
             Run `import refresh-drives --site-path <site path>`; passing --site-path to \
             `list-drives` only filters rows already stored."
                .to_owned(),
        ),
        Some("permission_denied") => Some(format!(
            "The grant does not permit enumerating libraries. On a WORK/school account an \
             administrator approves the app for the site(s); a PERSONAL Microsoft account has no \
             SharePoint sites to enumerate at all, and there is no administrator to ask — \
             reconnect and approve the storage scope. Switching this connection to a DIFFERENT \
             account requires revoking it first: a live connection stays bound to the account it \
             was connected with, and re-consenting as somebody else is refused. A revoke releases \
             that binding.{}",
            error.map_or_else(String::new, |e| format!(" Detail: {e}")),
        )),
        Some("failed") => Some(format!(
            "The last drive refresh failed{}. Run `import refresh-drives` to retry.",
            error.map_or_else(String::new, |e| format!(": {e}")),
        )),
        Some("empty") => Some(
            "A refresh completed and this connection reaches no libraries — every leg the grant \
             permits was walked and each one answered, so there is nothing further to retry. \
             Widening it needs a different grant (a work/school account with site access). Re-\
             consenting THIS connection as somebody else is refused while it is live — revoke it \
             first, or create a separate connection."
                .to_owned(),
        ),
        // A `ready` catalog carrying an error is TRUNCATED: usable, but not the
        // whole picture, so it must read as a warning beside a working list —
        // never as a failure.
        Some("ready") => error.map(|e| {
            format!("The library list is usable but incomplete: {e}. Run `import refresh-drives` to rebuild it.")
        }),
        // An unknown state is a server the CLI is older than. Say so plainly
        // rather than guessing a cause.
        Some(other) => Some(format!(
            "Unrecognized drives_state `{other}` — this CLI may be older than the server. \
             Use --format json to read the raw response."
        )),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::account_type_ignored_hint;
    use super::drives_state_hint;

    /// The shape the endpoint returns when it hands back an existing row:
    /// a success carrying `already_exists`, with the stored account type in the
    /// identity's properties bag.
    fn existing(provider: &str, stored: Option<&str>) -> serde_json::Value {
        let mut identity = json!({ "provider": provider, "properties": {} });
        if let Some(s) = stored {
            identity["properties"]["account_type"] = json!(s);
        }
        json!({ "already_exists": true, "identity": identity })
    }

    #[test]
    fn account_type_ignored_hint_fires_when_stored_type_differs() {
        let v = existing("onedrive_business", Some("work"));
        let h = account_type_ignored_hint(Some("personal"), &v).expect("hint expected");
        // Both types must appear: the one asked for and the one that stands.
        // A message naming only one leaves the reader unable to tell which
        // connection they now have.
        assert!(h.contains("personal"), "must name what was requested: {h}");
        assert!(
            h.contains("stays work"),
            "must name what the row keeps: {h}"
        );
        assert!(
            h.contains("revoking"),
            "must name the action that actually changes it: {h}"
        );
    }

    #[test]
    fn account_type_ignored_hint_treats_absent_stored_type_as_work() {
        // A connection made before the field existed, or one that took the
        // server's default, carries no `account_type` at all. Reading that as
        // "unknown, say nothing" would silence the exact case this exists for.
        let h = account_type_ignored_hint(Some("personal"), &existing("onedrive_business", None))
            .expect("absent stored type must still be a mismatch against personal");
        assert!(h.contains("stays work"), "absent must read as work: {h}");
    }

    /// POSITIVE CONTROL IN THE SAME TEST for every silent case below: a single
    /// mutation of the input under test must produce a hint. Without it, a
    /// helper that returned `None` unconditionally would pass all four.
    #[test]
    fn account_type_ignored_hint_is_silent_when_nothing_was_ignored() {
        let control = existing("onedrive_business", Some("work"));
        assert!(
            account_type_ignored_hint(Some("personal"), &control).is_some(),
            "control: the mismatch case must produce a hint, or the silences below prove nothing"
        );

        // Asked for what it already is — nothing was dropped.
        assert!(account_type_ignored_hint(Some("work"), &control).is_none());
        // Never asked for one.
        assert!(account_type_ignored_hint(None, &control).is_none());
        // A connect actually began, so the value was recorded.
        assert!(
            account_type_ignored_hint(
                Some("personal"),
                &json!({ "identity": { "provider": "onedrive_business", "properties": {} } })
            )
            .is_none()
        );
        // Another provider ignores the flag in every case; a OneDrive remedy
        // here would describe a connection the caller does not have.
        assert!(
            account_type_ignored_hint(Some("personal"), &existing("box", Some("work"))).is_none()
        );
    }
    use serde_json::json;

    /// A populated, clean catalog needs no commentary.
    ///
    /// POSITIVE CONTROL IN THE SAME TEST. `drives_state_hint` returns `None`
    /// early for any payload without a `drives` array, so a fixture that lost
    /// that key would satisfy the `is_none()` assertion for a reason that has
    /// nothing to do with `ready`-and-clean — a pass indistinguishable from the
    /// real one. The control mutates ONLY `drives_error` and requires a hint,
    /// proving this fixture actually reaches the state match before the silence
    /// is credited to it.
    #[test]
    fn ready_without_error_is_silent() {
        let v = json!({ "drives": [{ "drive_id": "b!a" }], "drives_state": "ready",
                        "drives_error": null });
        assert!(drives_state_hint(&v).is_none());

        let reaches_the_branch = json!({ "drives": [{ "drive_id": "b!a" }], "drives_state": "ready",
                                         "drives_error": "truncated" });
        assert!(
            drives_state_hint(&reaches_the_branch).is_some(),
            "control failed: this fixture never reaches the state match, so the \
             silence above proves nothing"
        );
    }

    /// A `ready` catalog carrying an error is TRUNCATED — usable but incomplete.
    /// It must read as a warning beside a working list, never as a failure, and
    /// length-based logic would miss it entirely.
    #[test]
    fn ready_with_error_warns_but_does_not_condemn() {
        let v = json!({ "drives": [{ "drive_id": "b!a" }], "drives_state": "ready",
                        "drives_error": "enumeration truncated at 500 sites" });
        let h = drives_state_hint(&v).expect("hint expected");
        assert!(h.contains("usable but incomplete"), "got: {h}");
        assert!(h.contains("truncated at 500 sites"), "got: {h}");
    }

    /// The five empty states must be told apart. Each gets a DIFFERENT next
    /// step; conflating them is the defect this function exists to prevent.
    #[test]
    fn each_empty_state_gets_its_own_next_step() {
        let hint = |state: &str| {
            drives_state_hint(&json!({ "drives": [], "drives_state": state }))
                .unwrap_or_else(|| panic!("hint expected for {state}"))
        };

        let never = hint("never_refreshed");
        assert!(never.contains("refresh-drives"), "got: {never}");
        assert!(
            never.contains("normal"),
            "a new connection is not an error: {never}"
        );

        let refreshing = hint("refreshing");
        assert!(refreshing.contains("poll"), "got: {refreshing}");
        assert!(
            !refreshing.contains("administrator"),
            "in-flight is not a grant problem: {refreshing}"
        );

        let requires = hint("requires_site_path");
        assert!(
            requires.contains("refresh-drives --site-path"),
            "got: {requires}"
        );

        // `permission_denied` may name an administrator, but must NOT name one
        // unconditionally: a PERSONAL Microsoft account has no SharePoint and
        // no administrator to ask, and `--account-type personal` made that
        // reachable. Naming an actor who cannot exist is the same defect as
        // naming an action the user cannot complete.
        let denied = hint("permission_denied");
        assert!(denied.contains("administrator"), "got: {denied}");
        assert!(
            denied.to_lowercase().contains("personal"),
            "must state the personal-account case, where no administrator exists: {denied}"
        );

        // `empty` is defined server-side as "every leg the grant permits was
        // walked and each one answered" — nothing further to try. It must NOT
        // promise an administrator remedy the server says does not exist, and
        // must not send the caller round the refresh loop again.
        let empty = hint("empty");
        assert!(
            !empty.contains("administrator must"),
            "empty is terminal for this grant; do not promise an admin fix: {empty}"
        );
        assert!(
            empty.contains("nothing further")
                || empty.to_lowercase().contains("not another refresh"),
            "empty must say retrying cannot help: {empty}"
        );
        assert!(
            !empty.contains("--site-path"),
            "a granted-but-empty tenant needs no site path: {empty}"
        );
    }

    /// `never_refreshed` and `refreshing` must NOT be reported as a missing
    /// admin grant — that was the pre-contract behaviour and it would send a
    /// user to their administrator over a catalog that simply is not built yet.
    #[test]
    fn unbuilt_catalog_is_never_blamed_on_the_administrator() {
        for state in ["never_refreshed", "refreshing"] {
            let h = drives_state_hint(&json!({ "drives": [], "drives_state": state }))
                .unwrap_or_else(|| panic!("hint expected for {state}"));
            assert!(
                !h.contains("administrator"),
                "{state} must not blame an admin: {h}"
            );
        }
    }

    #[test]
    fn failed_state_surfaces_the_server_error() {
        let v = json!({ "drives": [], "drives_state": "failed",
                        "drives_error": "Graph returned 503" });
        let h = drives_state_hint(&v).expect("hint expected");
        assert!(h.contains("Graph returned 503"), "got: {h}");
        assert!(h.contains("retry"), "got: {h}");
    }

    /// The retired service-account model must never reappear in remedy copy.
    ///
    /// Under it, `google_drive` and `box` were background-provisioned as
    /// service accounts and the user shared a folder with an address. Delegated
    /// OAuth deleted that — the connected account IS the access. The wording
    /// outlived the model in two peer codebases on a single day, once on an
    /// ERROR path, which is read at the exact moment somebody is trying to fix
    /// something.
    ///
    /// So this asserts the MODEL rather than any particular phrasing, and
    /// sweeps EVERY branch this module can emit rather than a sample — the
    /// branch nobody thought to check is the one free to rot.
    ///
    /// This is a BLOCKLIST and is not sufficient alone: gutting a hint to
    /// "Nothing to report." passes it (verified 2026-08-17 by mutation). Copy
    /// that says nothing satisfies every negative assertion, which is one way
    /// a retired model survives unnoticed. The positive half lives in
    /// [`each_empty_state_gets_its_own_next_step`] and its siblings, which
    /// require each state to name its own next step — that pairing is what
    /// makes this meaningful, so do not weaken those on the grounds that this
    /// test covers the wording.
    ///
    /// SCOPE, stated because a gate that quietly scans the wrong corpus
    /// reports a confident zero about text it never read: this covers the
    /// runtime HINTS — what a user acts on, and the worse place to be wrong.
    /// It does NOT cover `--help` or the MCP tool descriptions. Those were
    /// checked by hand instead (2026-08-17): the only matches there are
    /// `cli.rs` deliberately describing the retired model AS retired, and two
    /// correct uses of the live `identity_email` field. Gating them
    /// automatically would need to tell a description of the old model apart
    /// from an instruction under it, which these patterns cannot do.
    #[test]
    fn no_hint_resurrects_the_retired_service_account_model() {
        // Each pattern must be something that can ONLY be true under the
        // retired model. `identity_email` was here and was WRONG: it is a live
        // field naming the connected account, so legitimate copy referring to
        // it would have failed this gate. The retired construct is sharing
        // *with* an address, which the verb patterns already catch.
        const RETIRED: &[&str] = &[
            "share the folder",
            "share your folder",
            "address to share",
            "re-share",
            "import agent",
            "service account",
            "app user",
            "grant edit",
        ];

        let mut hints: Vec<(String, String)> = Vec::new();
        // Every `drives_state` the function branches on, plus the unknown
        // fallback — enumerated, not sampled.
        for state in [
            "never_refreshed",
            "refreshing",
            "requires_site_path",
            "permission_denied",
            "empty",
            "failed",
            "some_future_state",
        ] {
            // WITHOUT a server error: purely text this CLI authored, which is
            // the only text this blocklist is entitled to police. Three hints
            // append a server-supplied `drives_error`, so a fixture that
            // always supplies one tests my sentence and the platform's
            // together and can no longer tell which of them said something.
            let bare = json!({ "drives": [], "drives_state": state });
            if let Some(h) = drives_state_hint(&bare) {
                hints.push((format!("{state} (authored)"), h));
            }
            let v = json!({ "drives": [], "drives_state": state, "drives_error": "boom" });
            if let Some(h) = drives_state_hint(&v) {
                hints.push((state.to_owned(), h));
            }
        }
        let ready = json!({ "drives": [{ "drive_id": "b!a" }], "drives_state": "ready",
                            "drives_error": "truncated" });
        if let Some(h) = drives_state_hint(&ready) {
            hints.push(("ready+error".to_owned(), h));
        }
        if let Some(h) = account_type_ignored_hint(
            Some("personal"),
            &existing("onedrive_business", Some("work")),
        ) {
            hints.push(("account_type_ignored".to_owned(), h));
        }

        // CONTROL: a clean sweep over an empty or truncated set proves nothing.
        assert!(
            hints.len() >= 16,
            "control: only {} hints exercised — the sweep must reach every branch \
             in BOTH the authored and server-error forms, or passing means nothing",
            hints.len()
        );

        for (what, h) in &hints {
            let lower = h.to_lowercase();
            for term in RETIRED {
                assert!(
                    !lower.contains(term),
                    "`{what}` resurrects the retired service-account model (matched {term:?}): {h}"
                );
            }
        }
    }

    /// A server-supplied `drives_error` is relayed VERBATIM, and the blocklist
    /// above deliberately does not police it.
    ///
    /// Three hints append the platform's own error text. That text is not this
    /// CLI's to rewrite — hiding or editing a platform error is how a caller
    /// ends up debugging our paraphrase instead of their problem — so if the
    /// platform ever emits retired-model wording, it reaches the user and the
    /// fix belongs on that side. What this CLI owes is that ITS OWN half stays
    /// clean, which is why the sweep exercises every state with no error
    /// attached.
    ///
    /// Stated as a test rather than a comment because the alternative reading —
    /// "the gate covers these messages" — is the one someone will have when
    /// the platform's wording is the thing that rots.
    #[test]
    fn server_error_fragment_is_relayed_verbatim_and_not_policed() {
        let hostile = "Re-share the folder with the import agent";
        let with_error = drives_state_hint(
            &json!({ "drives": [], "drives_state": "failed", "drives_error": hostile }),
        )
        .expect("hint expected");
        assert!(
            with_error.contains(hostile),
            "the platform's own error must reach the user unaltered: {with_error}"
        );

        // ...and the half this CLI wrote stays clean regardless.
        let authored = drives_state_hint(&json!({ "drives": [], "drives_state": "failed" }))
            .expect("hint expected");
        assert!(
            !authored.to_lowercase().contains("import agent")
                && !authored.to_lowercase().contains("share the folder"),
            "authored text must be clean even where a server fragment is appended: {authored}"
        );
    }

    /// A state this CLI does not know about must be reported as such, not
    /// silently mapped onto a neighbouring meaning.
    #[test]
    fn unknown_state_is_admitted_not_guessed() {
        let v = json!({ "drives": [], "drives_state": "some_future_state" });
        let h = drives_state_hint(&v).expect("hint expected");
        assert!(h.contains("Unrecognized"), "got: {h}");
        assert!(h.contains("some_future_state"), "got: {h}");
    }

    /// Not a drives payload (error envelope, unexpected shape) => stay quiet
    /// rather than print a confident explanation under an unrelated error.
    #[test]
    fn non_drives_payload_yields_no_hint() {
        assert!(drives_state_hint(&json!({ "drives_state": "empty" })).is_none());
        assert!(drives_state_hint(&json!({})).is_none());
    }

    /// Absent `drives_state` (an older server) => no guess.
    #[test]
    fn missing_state_yields_no_hint() {
        assert!(drives_state_hint(&json!({ "drives": [] })).is_none());
    }
}
