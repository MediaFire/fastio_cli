#![allow(clippy::missing_errors_doc)]

/// App-installation endpoints for the Fast.io REST API.
///
/// This is an **installation registry**, not an app catalog. `/user/apps/` is
/// backed by `AppInstallations` server-side and records apps that registered
/// themselves against the calling user — `app_id` (caller-defined, e.g.
/// `com.example.desktop`), version, platform, install/uninstall times, last
/// heartbeat, arbitrary metadata. The documented surface is `GET /user/apps/`
/// plus `POST` `install` / `uninstall` / `heartbeat`
/// (see the published API docs); the CLI currently surfaces only the list.
///
/// Note for anyone adding the writes: those POSTs are **form-encoded**, unlike
/// the cloud-import routes, which take JSON.
///
/// A previous revision built every path under a `/apps/` prefix
/// (`/apps/list/`, `/apps/{id}/details/`, `/apps/{id}/launch/`,
/// `/apps/tool/{name}/`). **That prefix has never existed** — all four returned
/// `9992` on every call, so the whole `fastio apps` surface was non-functional.
/// Those were modelled on app *widgets*, which are an MCP-server concept with
/// **no REST API in the published contract**, so they were removed rather than
/// re-pointed: there is nothing for them to call, and nothing planned.
use serde_json::Value;

use crate::client::ApiClient;
use crate::error::CliError;

/// List the authenticated user's installed apps.
///
/// Requires authentication — this is a per-user list, not a public catalog, and
/// an unauthenticated call fails with `10011`. (The removed `/apps/list/` was
/// called unauthenticated, which is part of why the breakage went unnoticed:
/// it never got far enough to need a token.)
///
/// `GET /user/apps/`
pub async fn list_installed_apps(client: &ApiClient) -> Result<Value, CliError> {
    client.get("/user/apps/").await
}
