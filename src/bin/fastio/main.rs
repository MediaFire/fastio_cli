//! Binary crate for the `fastio` CLI tool and MCP server.
//!
//! Detects whether to run in interactive CLI mode or as a JSON-RPC MCP server,
//! then dispatches accordingly.
mod cli;
mod commands;
mod mcp;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use colored::Colorize;

use cli::{AuthCommands, Cli, Commands};
use commands::ai::AiCommand;
use commands::apps::AppsCommand;
use commands::asset::AssetCommand;
use commands::auth::{ApiKeyCommand, AuthCommand, OauthCommand, TwoFaCommand};
use commands::comment::CommentCommand;
use commands::download::DownloadCommand;
use commands::event::EventCommand;
use commands::files::{FileLockCommand, FilesCommand};
use commands::import::ImportCommand;
use commands::intents::IntentsCommand;
use commands::invitation::InvitationCommand;
use commands::lock::LockCommand;
use commands::member::MemberCommand;
use commands::metadata::MetadataCommand;
use commands::org::{
    BillingCommand, DiscoverCommand, OrgAssetCommand, OrgCommand, OrgInvitationsCommand,
    OrgMembersCommand, OrgTransferTokenCommand,
};
use commands::preview::PreviewCommand;
use commands::search::SearchCommand;
use commands::share::{
    ShareCommand, ShareCreateArgs, ShareFilesCommand, ShareInvitationCommand, ShareMembersCommand,
    ShareUpdateArgs,
};
use commands::system::SystemCommand;
use commands::upload::UploadCommand;
use commands::user::{
    AvatarCommand, SettingsCommand, UserAssetCommand, UserCommand, UserEmailChangeCommand,
    UserInvitationsCommand,
};
use commands::workspace::WorkspaceCommand;
use fastio_cli::config::Config;
use fastio_cli::output::OutputConfig;

/// Generic failure — any error without a more specific classification.
///
/// Note that exit code `2` is not ours to assign: `clap` uses it for usage
/// errors (an unknown flag, a missing required argument), and those are raised
/// before any API call happens.
const EXIT_FAILURE: i32 = 1;

/// A write lost a compare-and-swap: the base revision sent was not current.
///
/// Retrying after re-reading the current revision is the appropriate response.
const EXIT_CONFLICT: i32 = 3;

// Exit code 4 is intentionally UNASSIGNED. It briefly denoted "contested" —
// stop retrying, this node is under contention — read from a server-declared
// flag. That signal was cut 2026-08-24: the field would have shipped as a
// constant, and a value that is always the same has no failing case, so a
// client branch on it is untestable by construction. The reader is removed
// rather than left dormant: a branch awaiting a CANCELLED feature is not
// dormant, it is dead. If contention signalling returns, this is three lines
// to restore — cheaper than carrying an exit code nothing can produce.

#[tokio::main]
async fn main() -> Result<()> {
    let result = run().await;
    if let Err(ref err) = result {
        // If the underlying error is a CliError, use rich display.
        if let Some(cli_err) = err.downcast_ref::<fastio_cli::error::CliError>() {
            let (headline, hint) = cli_error_render(err, cli_err);
            eprintln!("{} {headline}", "error:".red().bold());
            if let Some(hint) = hint {
                eprintln!("{} {hint}", "hint:".yellow().bold());
            }
            std::process::exit(exit_code_for(cli_err));
        }
    }
    result
}

/// Classify a `CliError` into the process exit code it should produce.
///
/// Factored out as a pure function, like [`cli_error_render`], so the mapping
/// is unit-testable without spawning a process.
///
/// Every API failure used to exit `1`, which made a lost compare-and-swap
/// indistinguishable from a bad id or a server fault. A scripted agent could
/// therefore only distinguish "worked" from "did not work", and the CAS retry
/// loop is precisely where that distinction matters.
///
/// `contested` is read from the server's structured `details`, never computed
/// here: whether a node is contested depends on attempts by *other* processes,
/// which a single stateless invocation cannot observe.
///
/// Classification requires a POSITIVE compare-and-swap signal — never the HTTP
/// status alone. The API reuses `409` for unrelated conflicts (email already in
/// use, 2FA already enabled, app already uninstalled) and `412` almost entirely
/// for plan/quota limits (`1685 Feature Limit`). Mapping those to
/// [`EXIT_CONFLICT`] would tell an agent to re-read and retry a **billing
/// quota** — advice that is both wrong and unbounded. Anything without a
/// positive signal keeps the historical [`EXIT_FAILURE`], so this can only ever
/// narrow what exits `1`, never broaden it.
fn exit_code_for(cli_err: &fastio_cli::error::CliError) -> i32 {
    // The FileShare write-back path already parses a CAS rejection into its own
    // variant (`CONFLICT_VERSION_MISMATCH` → `VersionConflict`), so it never
    // reaches the `Api` arm below. It is the only conflict this CLI can
    // currently produce — omitting it would leave the conflict exit code unable
    // to fire on the one path that exercises it.
    // `MappedApi` is an `ApiError` that a command re-hinted (the FileShare and
    // AI paths wrap one so the render layer prints THEIR hint). It carries the
    // same code and details, so it must be classified identically — matching
    // only `Api` would send a re-hinted compare-and-swap conflict to the
    // generic failure code.
    let api_err = match cli_err {
        fastio_cli::error::CliError::Api(api_err)
        | fastio_cli::error::CliError::MappedApi { api: api_err, .. } => api_err,
        fastio_cli::error::CliError::VersionConflict { .. } => return EXIT_CONFLICT,
        _ => return EXIT_FAILURE,
    };
    // ONE TOKEN, TWO ENVELOPES. A version mismatch arrives on two channels and
    // the client must read both or it only ever works on one path:
    //   * synchronous 409 (note CAS today, attach CAS after K2) — the server's
    //     structured `error.params.reason`, which `field_reason()` reads. The
    //     only structured slot on this envelope; there is NO top-level `reason`.
    //   * async (FileShare write-back) — `status_message` prefixed
    //     `CONFLICT_VERSION_MISMATCH:`, on an HTTP 200 poll of a FAILED session.
    //     That never reaches this function as an `Api`; the FileShare command
    //     parses it into `CliError::VersionConflict`, handled above.
    // The `error_code` check below is a DEFENSIVE THIRD: the server does not
    // currently emit `error.error_code`, so it cannot fire today. Kept because
    // it costs nothing and would catch the token if that ever changes — but it
    // is NOT the working path, and must not be mistaken for one.
    let marker_matches = |m: &str| m.eq_ignore_ascii_case(VERSION_MISMATCH_MARKER);
    let by_error_code = api_err.error_code.as_deref().is_some_and(marker_matches);
    let by_reason = api_err.field_reason().is_some_and(marker_matches);
    if by_error_code || by_reason {
        EXIT_CONFLICT
    } else {
        EXIT_FAILURE
    }
}

/// Machine-readable marker the API uses for a compare-and-swap version mismatch.
const VERSION_MISMATCH_MARKER: &str = "CONFLICT_VERSION_MISMATCH";

/// Decide what a `CliError`-rooted failure should print on stderr.
///
/// `main` intercepts an error chain whose root is a [`CliError`] and prints a
/// red `error:` headline plus an optional yellow `hint:` line. This helper is
/// the pure decision behind that rendering, factored out so it can be
/// unit-tested without capturing stderr.
///
/// The subtlety it handles: command handlers attach signing / upload / etc.
/// framing via `anyhow::Error::context(...)` ON TOP OF a `CliError`. anyhow's
/// `.context()` preserves downcastability, so `downcast_ref::<CliError>()` still
/// succeeds — but [`CliError::render_stderr`] only prints the bare `CliError`
/// `Display`, silently dropping every added context layer. To avoid that:
///
/// - **Bare `CliError`** (no context added): the anyhow top-level message equals
///   the `CliError`'s own `Display`, so the headline is that `Display`
///   verbatim — byte-identical to the legacy `render_stderr` output.
/// - **Context added**: the anyhow top-level message differs from the `CliError`
///   `Display`, so the headline is anyhow's alternate (`{err:#}`) rendering —
///   `outer context: …: root cause` — surfacing the added framing to the user.
///
/// In both cases the hint is the `CliError`'s own `suggestion()` (rendered once,
/// on its own `hint:` line), so an added-context message is strictly more
/// informative than the bare one without ever doubling the suggestion text.
fn cli_error_render(
    err: &anyhow::Error,
    cli_err: &fastio_cli::error::CliError,
) -> (String, Option<&'static str>) {
    let headline = if err.to_string() == cli_err.to_string() {
        // Bare CliError — preserve the legacy headline exactly.
        cli_err.to_string()
    } else {
        // Context was layered on; render the full chain so it isn't dropped,
        // de-doubling the consecutive identical links that `thiserror`'s
        // `#[from]`-generated `source()` produces (see `render_chain_dedup`).
        render_chain_dedup(err)
    };
    (headline, cli_err.suggestion())
}

/// Render an `anyhow` error chain as `outer: …: root cause`, collapsing ONLY the
/// duplicate-link artifact that `thiserror`'s `#[from]` introduces on a
/// [`CliError::Api`].
///
/// This is `format!("{err:#}")` MINUS that one artifact. [`CliError::Api`] is
/// `#[error("{0}")]` with `#[from] ApiError`: the `#[from]` ALSO generates a
/// `source()` returning the inner [`ApiError`], so the chain carries the
/// `ApiError`'s `Display` TWICE in a row — once as `CliError::Api`'s own
/// `Display` (which just forwards to it) and once as the `ApiError` source link
/// directly behind it. anyhow's `{:#}` joins every link with `": "`, so the full
/// `[HTTP …] … see: … resource: …` block printed twice in a row (observed on
/// the signing not-ready path, but inherent to ANY command that layers
/// `.context()` onto a [`CliError::Api`]).
///
/// The dedup is **type-aware**, not text-only: a link is skipped ONLY when the
/// immediately-preceding link downcasts to [`CliError::Api`] AND the current
/// link's `Display` is byte-identical to it (the forwarded `ApiError` source).
/// This targets exactly the `#[from]` forward-to-source adjacency and nothing
/// else — a hand-written same-text context/cause pair (e.g. an anyhow `.context("x")`
/// over a plain error whose `Display` is `"x"`) renders in full as `"x: x"`,
/// because the preceding link is not a `CliError::Api`.
fn render_chain_dedup(err: &anyhow::Error) -> String {
    use fastio_cli::error::CliError;

    let mut out = String::new();
    // Track the previously-appended link's rendered text and whether it was a
    // `CliError::Api` — the only pairing whose forwarded source we collapse.
    let mut prev: Option<(String, bool)> = None;
    for link in err.chain() {
        let rendered = link.to_string();
        if let Some((prev_text, prev_is_api)) = &prev
            && *prev_is_api
            && *prev_text == rendered
        {
            // The `#[from]` forward-to-source duplicate of a `CliError::Api`
            // link. Skip it (do not advance `prev`: the appended link is
            // still the `CliError::Api` we are collapsing onto).
            continue;
        }
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(&rendered);
        let is_api = matches!(link.downcast_ref::<CliError>(), Some(CliError::Api(_)));
        prev = Some((rendered, is_api));
    }
    out
}

/// Whether the E-Sign kill-switch should block this command.
///
/// Factored out of `run()` so the gate predicate is unit-testable WITHOUT
/// mutating the process environment (`FASTIO_ENABLE_ESIGN` is process-global and
/// unsafe to set under Rust 2024): `esign_enabled` is passed in. Blocks iff the
/// command is a `sign` subcommand AND E-Sign is not enabled; every non-sign
/// command passes regardless of the flag.
fn sign_gate_blocks(command: &Commands, esign_enabled: bool) -> bool {
    matches!(command, Commands::Sign(_)) && !esign_enabled
}

/// Whether the cloud-import kill-switch should block this command.
///
/// Same shape as [`sign_gate_blocks`], and factored out for the same reason:
/// `FASTIO_ENABLE_CLOUD_IMPORT` is process-global and unsafe to set under Rust
/// 2024, so the predicate takes the flag as a parameter and stays unit-testable.
fn import_gate_blocks(command: &Commands, cloud_import_enabled: bool) -> bool {
    matches!(command, Commands::Import(_)) && !cloud_import_enabled
}

/// Core logic extracted so we can intercept errors in `main()`.
async fn run() -> Result<()> {
    let cli = Cli::parse();

    // MCP mode: start the MCP server over stdio without tracing (would corrupt stdio).
    if let Commands::Mcp { tools } = &cli.command {
        let tools_filter = tools.as_ref().map(|t| {
            t.split(',')
                .map(|s| s.trim().to_owned())
                .collect::<Vec<_>>()
        });
        // Thread the global --api-base / --token / --profile overrides into the
        // MCP server; this branch returns before the non-MCP path resolves them.
        return mcp::serve(
            tools_filter,
            cli.api_base.as_deref(),
            cli.token.as_deref(),
            cli.profile.as_deref(),
        )
        .await;
    }

    // Initialize tracing (only for non-MCP modes)
    let filter = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    // Disable colors if requested
    if cli.no_color {
        colored::control::set_override(false);
    }

    // E-Sign kill-switch (feature sunset 2026-07): gate the sign surface here,
    // BEFORE config/profile resolution, so the disabled error always wins over
    // any config/auth error. Enabled only via `FASTIO_ENABLE_ESIGN=1`; the
    // platform enforces its own org-level flag server-side as well. Returning a
    // `CliError::FeatureDisabled` (rather than a bare `anyhow::bail!`) routes the
    // failure through `main`'s red `error:` + yellow `hint:` render.
    if sign_gate_blocks(&cli.command, commands::sign::esign_enabled()) {
        return Err(fastio_cli::error::CliError::FeatureDisabled {
            message: "E-Sign is currently disabled.",
            hint: "Set FASTIO_ENABLE_ESIGN=1 to use sign commands (signing must also be enabled for your organization).",
        }
        .into());
    }

    // Cloud-import kill-switch: cloud import has not launched, so the surface is
    // off by default. Gated here alongside E-Sign — BEFORE config/profile
    // resolution — so the disabled error always wins over any config/auth error.
    // The platform gates it server-side too (dev environments only); this is
    // belt-and-braces so the commands are not merely broken-on-prod but
    // explicitly unavailable.
    if import_gate_blocks(&cli.command, commands::import::cloud_import_enabled()) {
        return Err(fastio_cli::error::CliError::FeatureDisabled {
            message: "Cloud import is not yet available.",
            hint: "Set FASTIO_ENABLE_CLOUD_IMPORT=1 to use import commands (cloud import must also be enabled for your workspace).",
        }
        .into());
    }

    // Load configuration
    let config = Config::load().context("failed to load configuration")?;

    // Determine active profile name
    let profile_name = cli.profile.as_deref().unwrap_or(&config.default_profile);

    // Resolve API base URL
    let api_base = config.api_base(cli.api_base.as_deref(), Some(profile_name));

    // Build output configuration
    let output = OutputConfig::from_flags_detail(
        cli.format.as_deref(),
        cli.fields.as_deref(),
        cli.no_color,
        cli.quiet,
        cli.detail.as_deref(),
    );

    let ctx = commands::CommandContext {
        output: &output,
        profile_name,
        api_base: &api_base,
        flag_token: cli.token.as_deref(),
        config_dir: &config.config_dir,
    };

    // Renew an expired/expiring PKCE access token BEFORE the command runs.
    //
    // This is the one async seam where it can happen: `resolve_token` is sync
    // and reached from 200+ `build_client()` call sites, so refreshing there
    // would mean making the whole client-construction path async. Doing it once
    // here — after the profile and API base are known, before any handler runs —
    // means every downstream `resolve_token` simply reads the refreshed
    // credential and nothing else changes.
    //
    // Skipped only for auth commands where renewing is meaningless or actively
    // wrong — NOT for the whole `auth` group. The authenticated ones (`status`,
    // `check`, `session`, `scopes`, `api-key *`, `oauth *`, `2fa *`) need a live
    // bearer like any other command, and `status` / `check` are precisely the
    // commands a user runs to ask "am I logged in?" — reporting a stale expiry
    // there, while a valid refresh token sits on disk, is the exact complaint
    // this change exists to fix.
    //
    // The skips: `login` / `signup` have no credential yet; `verify`,
    // `email-check` and the password-reset trio are unauthenticated by design;
    // `logout` / `signout` / `invalidate-all` tear a session down, so renewing
    // one to immediately discard it would be absurd and `signout` would revoke a
    // token minted seconds earlier.
    //
    // Best-effort by design: a failure leaves stored credentials untouched and
    // the pre-existing "expired — run `fastio auth login`" error still surfaces.
    //
    // Only `Failed` is worth a word to the user. `Refreshed` and `NotNeeded`
    // stay silent — a renewal nobody asked for should not add noise to every
    // command — and a credentials-read `Err` is left to the command's own auth
    // resolution, which reports it with far better context than this step could.
    let skip_refresh = matches!(
        cli.command,
        Commands::Auth(
            cli::AuthCommands::Login { .. }
                | cli::AuthCommands::Signup { .. }
                | cli::AuthCommands::Verify { .. }
                | cli::AuthCommands::EmailCheck { .. }
                | cli::AuthCommands::PasswordResetRequest { .. }
                | cli::AuthCommands::PasswordReset { .. }
                | cli::AuthCommands::PasswordResetCheck { .. }
                | cli::AuthCommands::Logout
                | cli::AuthCommands::Signout
                | cli::AuthCommands::InvalidateAll
        )
    );
    if !skip_refresh
        && let Ok(fastio_cli::auth::token::RefreshOutcome::Failed) =
            fastio_cli::auth::token::refresh_if_needed(
                &api_base,
                profile_name,
                &config.config_dir,
                cli.token.as_deref(),
            )
            .await
        && !output.quiet
    {
        eprintln!(
            "warning: could not renew the stored session — it may have been revoked; \
             run `fastio auth login` if the command below fails to authenticate"
        );
    }

    dispatch(cli.command, &ctx, &config).await
}

/// Dispatch a parsed CLI command to the appropriate handler.
async fn dispatch(
    cli_command: Commands,
    ctx: &commands::CommandContext<'_>,
    config: &Config,
) -> Result<()> {
    match cli_command {
        Commands::Auth(c) => commands::auth::execute(&map_auth_command(c), config, ctx).await,
        Commands::User(c) => commands::user::execute(&map_user_command(c), ctx).await,
        Commands::Org(c) => commands::org::execute(&map_org_command(c), ctx).await,
        Commands::Workspace(c) => {
            commands::workspace::execute(&map_workspace_command(c), ctx).await
        }
        Commands::Member(c) => commands::member::execute(&map_member_command(c), ctx).await,
        Commands::Invitation(c) => {
            commands::invitation::execute(&map_invitation_command(c), ctx).await
        }
        Commands::Files(c) => commands::files::execute(&map_files_command(c), ctx).await,
        Commands::Upload(c) => commands::upload::execute(&map_upload_command(c), ctx).await,
        Commands::Download(c) => commands::download::execute(&map_download_command(c), ctx).await,
        Commands::Share(c) => commands::share::execute(&map_share_command(c), ctx).await,
        Commands::Comment(c) => commands::comment::execute(&map_comment_command(c), ctx).await,
        Commands::Event(c) => commands::event::execute(&map_event_command(c), ctx).await,
        Commands::Dashboard(c) => commands::dashboard::execute(c, ctx).await,
        Commands::Preview(c) => commands::preview::execute(&map_preview_command(c), ctx).await,
        Commands::Asset(c) => commands::asset::execute(&map_asset_command(c), ctx).await,
        Commands::Ripley(c) => commands::ai::execute(&map_ripley_command(c), ctx).await,
        Commands::Apps(c) => commands::apps::execute(&map_apps_command(&c), ctx).await,
        Commands::Import(c) => commands::import::execute(&map_import_command(&c), ctx).await,
        Commands::Intents(c) => commands::intents::execute(&map_intents_command(&c), ctx).await,
        Commands::Lock(c) => commands::lock::execute(&map_lock_command(&c), ctx).await,
        Commands::Search(c) => commands::search::execute(map_search_command(c), ctx).await,
        Commands::View {
            workspace_id,
            node_id,
            raw,
            version,
            no_pager,
        } => {
            commands::view::execute(
                &commands::view::ViewCommand {
                    workspace_id,
                    node_id,
                    raw,
                    version,
                    no_pager,
                },
                ctx,
            )
            .await
        }
        Commands::HowTo {
            question,
            surface,
            context,
        } => commands::howto::execute(ctx, &question, surface.as_deref(), context.as_deref()).await,
        Commands::Metadata(c) => commands::metadata::execute(&map_metadata_command(c), ctx).await,
        Commands::Sign(c) => commands::sign::execute(c, ctx).await,
        Commands::Fileshare(c) => commands::fileshare::execute(c, ctx).await,
        Commands::System(c) => commands::system::execute(&map_system_command(&c), ctx).await,
        // Offline: pure local classification, no client/auth (like `Configure`).
        Commands::Id(c) => commands::id::execute(&c, ctx.output),
        Commands::Completions { shell } => {
            generate_completions(shell);
            Ok(())
        }
        Commands::Configure(configure_cmd) => {
            commands::configure::execute(&configure_cmd, ctx.output)
        }
        Commands::Skill => {
            print!("{}", include_str!("../../../AGENTS.md"));
            Ok(())
        }
        Commands::Mcp { .. } => {
            // MCP mode is dispatched earlier (before this match) and never
            // reaches here; return a graceful error rather than panicking so
            // no `unreachable!` remains in the production dispatch path.
            anyhow::bail!("mcp command should be handled before this point")
        }
    }
}

/// Generate shell completion scripts and write them to stdout.
fn generate_completions(shell: cli::ShellType) {
    use clap_complete::{Shell, generate};
    let mut cmd = Cli::command();
    let shell = match shell {
        cli::ShellType::Bash => Shell::Bash,
        cli::ShellType::Zsh => Shell::Zsh,
        cli::ShellType::Fish => Shell::Fish,
        cli::ShellType::Powershell => Shell::PowerShell,
    };
    generate(shell, &mut cmd, "fastio", &mut std::io::stdout());
}

/// Convert clap-parsed auth commands to the internal enum.
fn map_auth_command(cmd: AuthCommands) -> AuthCommand {
    match cmd {
        AuthCommands::Login {
            email,
            password,
            agent_name,
        } => AuthCommand::Login {
            email,
            password,
            agent_name,
        },
        AuthCommands::Logout => AuthCommand::Logout,
        AuthCommands::Signout => AuthCommand::Signout,
        AuthCommands::InvalidateAll => AuthCommand::InvalidateAll,
        AuthCommands::Status => AuthCommand::Status,
        AuthCommands::Signup {
            email,
            password,
            first_name,
            last_name,
            agent,
        } => AuthCommand::Signup {
            email,
            password,
            first_name,
            last_name,
            agent,
        },
        AuthCommands::Verify { email, code } => AuthCommand::Verify { email, code },
        AuthCommands::TwoFa(tfa) => AuthCommand::TwoFa(match tfa {
            cli::TwoFaCommands::Setup { channel } => TwoFaCommand::Setup { channel },
            cli::TwoFaCommands::Verify { code } => TwoFaCommand::Verify { code },
            cli::TwoFaCommands::Disable { token } => TwoFaCommand::Disable { token },
            cli::TwoFaCommands::Status => TwoFaCommand::Status,
            cli::TwoFaCommands::Send { channel } => TwoFaCommand::Send { channel },
            cli::TwoFaCommands::VerifySetup { token } => TwoFaCommand::VerifySetup { token },
        }),
        AuthCommands::ApiKey(ak) => AuthCommand::ApiKey(match ak {
            cli::ApiKeyCommands::Create {
                name,
                scopes,
                agent_name,
                expires,
            } => ApiKeyCommand::Create {
                name,
                scopes,
                agent_name,
                expires,
            },
            cli::ApiKeyCommands::List => ApiKeyCommand::List,
            cli::ApiKeyCommands::Delete { key_id } => ApiKeyCommand::Delete { key_id },
            cli::ApiKeyCommands::Get { key_id } => ApiKeyCommand::Get { key_id },
            cli::ApiKeyCommands::Update {
                key_id,
                name,
                scopes,
                agent_name,
                expires,
            } => ApiKeyCommand::Update {
                key_id,
                name,
                scopes,
                agent_name,
                expires,
            },
        }),
        AuthCommands::Check => AuthCommand::Check,
        AuthCommands::Session => AuthCommand::Session,
        AuthCommands::EmailCheck { email } => AuthCommand::EmailCheck { email },
        AuthCommands::PasswordResetRequest { email } => AuthCommand::PasswordResetRequest { email },
        AuthCommands::PasswordReset {
            code,
            password1,
            password2,
        } => AuthCommand::PasswordReset {
            code,
            password1,
            password2,
        },
        AuthCommands::Oauth(o) => AuthCommand::Oauth(match o {
            cli::OauthCommands::List => OauthCommand::List,
            cli::OauthCommands::Details { session_id } => OauthCommand::Details { session_id },
            cli::OauthCommands::Rename {
                session_id,
                device_name,
                agent_name,
            } => OauthCommand::Rename {
                session_id,
                device_name,
                agent_name,
            },
            cli::OauthCommands::Revoke { session_id } => OauthCommand::Revoke { session_id },
            cli::OauthCommands::RevokeAll { exclude_current } => {
                OauthCommand::RevokeAll { exclude_current }
            }
        }),
        AuthCommands::Scopes => AuthCommand::Scopes,
        AuthCommands::PasswordResetCheck { code } => AuthCommand::PasswordResetCheck { code },
    }
}

/// Convert clap-parsed user commands to the internal enum.
fn map_user_command(cmd: cli::UserCommands) -> UserCommand {
    match cmd {
        cli::UserCommands::Info => UserCommand::Info,
        cli::UserCommands::Update {
            first_name,
            last_name,
            display_name,
            phone_country,
            phone_number,
            password,
            current_password,
        } => UserCommand::Update {
            first_name,
            last_name,
            display_name,
            phone_country,
            phone_number,
            password,
            current_password,
        },
        cli::UserCommands::EmailChange(ec) => UserCommand::EmailChange(match ec {
            cli::UserEmailChangeCommands::Request {
                new_email,
                current_password,
            } => UserEmailChangeCommand::Request {
                new_email,
                current_password,
            },
            cli::UserEmailChangeCommands::Confirm { token } => {
                UserEmailChangeCommand::Confirm { token }
            }
        }),
        cli::UserCommands::Avatar(av) => UserCommand::Avatar(match av {
            cli::UserAvatarCommands::Upload { file } => AvatarCommand::Upload { file },
            cli::UserAvatarCommands::Remove => AvatarCommand::Remove,
        }),
        cli::UserCommands::Settings(s) => UserCommand::Settings(match s {
            cli::UserSettingsCommands::Get => SettingsCommand::Get,
            cli::UserSettingsCommands::Update {
                first_name,
                last_name,
            } => SettingsCommand::Update {
                first_name,
                last_name,
            },
        }),
        cli::UserCommands::Search { query } => UserCommand::Search { query },
        cli::UserCommands::Close {
            email_address,
            dryrun,
        } => UserCommand::Close {
            email_address,
            dryrun,
        },
        cli::UserCommands::Details { user_id } => UserCommand::Details { user_id },
        cli::UserCommands::Profiles => UserCommand::Profiles,
        cli::UserCommands::Allowed => UserCommand::Allowed,
        cli::UserCommands::OrgLimits => UserCommand::OrgLimits,
        cli::UserCommands::Shares => UserCommand::Shares,
        cli::UserCommands::Invitations(inv) => UserCommand::Invitations(match inv {
            cli::UserInvitationsCommands::List => UserInvitationsCommand::List,
            cli::UserInvitationsCommands::Details { invitation_id } => {
                UserInvitationsCommand::Details { invitation_id }
            }
            cli::UserInvitationsCommands::AcceptAll => UserInvitationsCommand::AcceptAll,
        }),
        cli::UserCommands::Asset(a) => UserCommand::Asset(match a {
            cli::UserAssetCommands::Types => UserAssetCommand::Types,
            cli::UserAssetCommands::List { user_id } => UserAssetCommand::List { user_id },
            cli::UserAssetCommands::Delete { asset_type } => {
                UserAssetCommand::Delete { asset_type }
            }
            cli::UserAssetCommands::Upload { asset_type, file } => {
                UserAssetCommand::Upload { asset_type, file }
            }
            cli::UserAssetCommands::Read {
                user_id,
                asset_type,
                output,
            } => UserAssetCommand::Read {
                user_id,
                asset_type,
                output,
            },
        }),
        cli::UserCommands::Autosync { state } => UserCommand::Autosync { state },
        cli::UserCommands::Pin => UserCommand::Pin,
        cli::UserCommands::Phone {
            country_code,
            phone_number,
        } => UserCommand::Phone {
            country_code,
            phone_number,
        },
    }
}

/// Convert clap-parsed org commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_org_command(cmd: cli::OrgCommands) -> OrgCommand {
    match cmd {
        cli::OrgCommands::List { limit, offset } => OrgCommand::List { limit, offset },
        cli::OrgCommands::Create {
            name,
            domain,
            description,
            industry,
            billing_email,
        } => OrgCommand::Create {
            name,
            domain,
            description,
            industry,
            billing_email,
        },
        cli::OrgCommands::Info { org_id } => OrgCommand::Info { org_id },
        cli::OrgCommands::Update {
            org_id,
            name,
            domain,
            description,
            industry,
            billing_email,
            homepage_url,
            accent_color,
            background_color,
            background_mode,
            use_background,
            facebook_url,
            twitter_url,
            instagram_url,
            youtube_url,
            perm_member_manage,
            perm_authorized_domains,
            owner_defined,
        } => OrgCommand::Update {
            org_id,
            name,
            domain,
            description,
            industry,
            billing_email,
            homepage_url,
            accent_color,
            background_color,
            background_mode,
            use_background,
            facebook_url,
            twitter_url,
            instagram_url,
            youtube_url,
            perm_member_manage,
            perm_authorized_domains,
            owner_defined,
        },
        cli::OrgCommands::Delete { org_id, confirm } => OrgCommand::Delete { org_id, confirm },
        cli::OrgCommands::Members(m) => OrgCommand::Members(map_org_members_command(m)),
        cli::OrgCommands::Transfer {
            org_id,
            new_owner_id,
        } => OrgCommand::Transfer {
            org_id,
            new_owner_id,
        },
        cli::OrgCommands::Discover { limit, offset } => {
            OrgCommand::Discover(DiscoverCommand::List { limit, offset })
        }
        cli::OrgCommands::Billing(b) => OrgCommand::Billing(map_org_billing_command(b)),
        cli::OrgCommands::PublicDetails { org_id } => OrgCommand::PublicDetails { org_id },
        cli::OrgCommands::Limits { org_id } => OrgCommand::Limits { org_id },
        cli::OrgCommands::Invitations(inv) => {
            OrgCommand::Invitations(map_org_invitations_command(inv))
        }
        cli::OrgCommands::TransferToken(tt) => {
            OrgCommand::TransferToken(map_org_transfer_token_command(tt))
        }
        cli::OrgCommands::TransferClaim { token } => OrgCommand::TransferClaim { token },
        cli::OrgCommands::DiscoverAll { limit, offset } => {
            OrgCommand::Discover(DiscoverCommand::All { limit, offset })
        }
        cli::OrgCommands::DiscoverAvailable { limit, offset } => {
            OrgCommand::Discover(DiscoverCommand::Available { limit, offset })
        }
        cli::OrgCommands::DiscoverCheckDomain { domain } => {
            OrgCommand::Discover(DiscoverCommand::CheckDomain { domain })
        }
        cli::OrgCommands::DiscoverExternal { limit, offset } => {
            OrgCommand::Discover(DiscoverCommand::External { limit, offset })
        }
        cli::OrgCommands::Workspaces {
            org_id,
            limit,
            offset,
            archived,
        } => OrgCommand::Workspaces {
            org_id,
            limit,
            offset,
            archived,
        },
        cli::OrgCommands::Shares {
            org_id,
            limit,
            offset,
        } => OrgCommand::Shares {
            org_id,
            limit,
            offset,
        },
        cli::OrgCommands::OrgAsset(a) => OrgCommand::Asset(match a {
            cli::OrgAssetCommands::Types => OrgAssetCommand::Types,
            cli::OrgAssetCommands::List { org_id } => OrgAssetCommand::List { org_id },
            cli::OrgAssetCommands::Delete { org_id, asset_type } => {
                OrgAssetCommand::Delete { org_id, asset_type }
            }
        }),
        cli::OrgCommands::CreateWorkspace {
            org_id,
            name,
            folder_name,
            description,
            perm_join,
            perm_member_manage,
            intelligence,
            metadata_extraction,
        } => OrgCommand::CreateWorkspace {
            org_id,
            name,
            folder_name,
            description,
            perm_join,
            perm_member_manage,
            intelligence,
            metadata_extraction,
        },
    }
}

/// Convert clap-parsed org members subcommands to the internal enum.
fn map_org_members_command(m: cli::OrgMembersCommands) -> OrgMembersCommand {
    match m {
        cli::OrgMembersCommands::List {
            org_id,
            limit,
            offset,
        } => OrgMembersCommand::List {
            org_id,
            limit,
            offset,
        },
        cli::OrgMembersCommands::Invite {
            org_id,
            email,
            role,
        } => OrgMembersCommand::Invite {
            org_id,
            email,
            role,
        },
        cli::OrgMembersCommands::Remove { org_id, member_id } => {
            OrgMembersCommand::Remove { org_id, member_id }
        }
        cli::OrgMembersCommands::UpdateRole {
            org_id,
            member_id,
            role,
        } => OrgMembersCommand::UpdateRole {
            org_id,
            member_id,
            role,
        },
        cli::OrgMembersCommands::Details { org_id, member_id } => {
            OrgMembersCommand::Details { org_id, member_id }
        }
        cli::OrgMembersCommands::Leave { org_id } => OrgMembersCommand::Leave { org_id },
        cli::OrgMembersCommands::Join { org_id } => OrgMembersCommand::Join { org_id },
    }
}

/// Convert clap-parsed org billing subcommands to the internal enum.
fn map_org_billing_command(b: cli::OrgBillingCommands) -> BillingCommand {
    match b {
        cli::OrgBillingCommands::Details { org_id } => BillingCommand::Details { org_id },
        cli::OrgBillingCommands::Plans => BillingCommand::Plans,
        cli::OrgBillingCommands::Usage { org_id } => BillingCommand::Usage { org_id },
        cli::OrgBillingCommands::Meters {
            org_id,
            meter,
            start_time,
            end_time,
            workspace_id,
            share_id,
        } => BillingCommand::Meters {
            org_id,
            meter,
            start_time,
            end_time,
            workspace_id,
            share_id,
        },
        cli::OrgBillingCommands::Cancel { org_id, yes } => BillingCommand::Cancel { org_id, yes },
        cli::OrgBillingCommands::Reactivate { org_id } => BillingCommand::Reactivate { org_id },
        // The deprecated shims never call the server, so the parsed `org_id`
        // positional is intentionally discarded here.
        cli::OrgBillingCommands::Activate { .. } => BillingCommand::Activate,
        cli::OrgBillingCommands::Reset { .. } => BillingCommand::Reset,
        cli::OrgBillingCommands::Members {
            org_id,
            limit,
            offset,
        } => BillingCommand::Members {
            org_id,
            limit,
            offset,
        },
        cli::OrgBillingCommands::Subscribe { org_id, plan } => BillingCommand::Subscribe {
            org_id,
            plan_id: Some(plan),
        },
        cli::OrgBillingCommands::Invoices {
            org_id,
            limit,
            starting_after,
        } => BillingCommand::Invoices {
            org_id,
            limit,
            starting_after,
        },
    }
}

/// Convert clap-parsed org invitations subcommands to the internal enum.
fn map_org_invitations_command(inv: cli::OrgInvitationsCommands) -> OrgInvitationsCommand {
    match inv {
        cli::OrgInvitationsCommands::List {
            org_id,
            state,
            limit,
            offset,
        } => OrgInvitationsCommand::List {
            org_id,
            state,
            limit,
            offset,
        },
        cli::OrgInvitationsCommands::Update {
            org_id,
            invitation_id,
            state,
            role,
        } => OrgInvitationsCommand::Update {
            org_id,
            invitation_id,
            state,
            role,
        },
        cli::OrgInvitationsCommands::Delete {
            org_id,
            invitation_id,
        } => OrgInvitationsCommand::Delete {
            org_id,
            invitation_id,
        },
    }
}

/// Convert clap-parsed org transfer token subcommands to the internal enum.
fn map_org_transfer_token_command(tt: cli::OrgTransferTokenCommands) -> OrgTransferTokenCommand {
    match tt {
        cli::OrgTransferTokenCommands::Create { org_id } => {
            OrgTransferTokenCommand::Create { org_id }
        }
        cli::OrgTransferTokenCommands::List {
            org_id,
            limit,
            offset,
        } => OrgTransferTokenCommand::List {
            org_id,
            limit,
            offset,
        },
        cli::OrgTransferTokenCommands::Delete { org_id, token_id } => {
            OrgTransferTokenCommand::Delete { org_id, token_id }
        }
    }
}

/// Convert clap-parsed workspace commands to the internal enum.
fn map_workspace_command(cmd: cli::WorkspaceCommands) -> WorkspaceCommand {
    match cmd {
        cli::WorkspaceCommands::List {
            org,
            limit,
            offset,
            archived,
        } => WorkspaceCommand::List {
            org_id: org,
            limit,
            offset,
            archived,
        },
        cli::WorkspaceCommands::Create {
            name,
            org,
            folder_name,
            description,
            intelligence,
            metadata_extraction,
        } => WorkspaceCommand::Create {
            org_id: org,
            name,
            folder_name,
            description,
            intelligence,
            metadata_extraction,
        },
        cli::WorkspaceCommands::Info { workspace_id } => WorkspaceCommand::Info { workspace_id },
        cli::WorkspaceCommands::Update {
            workspace_id,
            name,
            description,
            folder_name,
            intelligence,
            metadata_extraction,
            perm_join,
            perm_member_manage,
            accent_color,
            background_color1,
            background_color2,
            owner_defined,
        } => WorkspaceCommand::Update {
            workspace_id,
            name,
            description,
            folder_name,
            intelligence,
            metadata_extraction,
            perm_join,
            perm_member_manage,
            accent_color,
            background_color1,
            background_color2,
            owner_defined,
        },
        cli::WorkspaceCommands::Delete {
            workspace_id,
            confirm,
        } => WorkspaceCommand::Delete {
            workspace_id,
            confirm,
        },
        cli::WorkspaceCommands::JobsStatus { workspace_id } => {
            WorkspaceCommand::JobsStatus { workspace_id }
        }
        cli::WorkspaceCommands::Search {
            workspace_id,
            query,
            limit,
            offset,
            modes,
        } => WorkspaceCommand::Search {
            workspace_id,
            query,
            limit,
            offset,
            modes: modes.to_params(),
        },
        cli::WorkspaceCommands::Limits { workspace_id } => {
            WorkspaceCommand::Limits { workspace_id }
        }
    }
}

/// Convert clap-parsed member commands to the internal enum.
fn map_member_command(cmd: cli::MemberCommands) -> MemberCommand {
    match cmd {
        cli::MemberCommands::List {
            workspace,
            limit,
            offset,
        } => MemberCommand::List {
            workspace,
            limit,
            offset,
        },
        cli::MemberCommands::Add {
            email,
            workspace,
            role,
        } => MemberCommand::Add {
            workspace,
            email,
            role,
        },
        cli::MemberCommands::Remove {
            member_id,
            workspace,
        } => MemberCommand::Remove {
            workspace,
            member_id,
        },
        cli::MemberCommands::Update {
            member_id,
            workspace,
            role,
        } => MemberCommand::Update {
            workspace,
            member_id,
            role,
        },
        cli::MemberCommands::Info {
            member_id,
            workspace,
        } => MemberCommand::Info {
            workspace,
            member_id,
        },
    }
}

/// Convert clap-parsed invitation commands to the internal enum.
fn map_invitation_command(cmd: cli::InvitationCommands) -> InvitationCommand {
    match cmd {
        cli::InvitationCommands::List { limit, offset } => {
            InvitationCommand::List { limit, offset }
        }
        cli::InvitationCommands::Accept {
            invitation_id,
            entity_type,
            entity_id,
        } => InvitationCommand::Accept {
            invitation_id,
            entity_type,
            entity_id,
        },
        cli::InvitationCommands::Decline {
            invitation_id,
            entity_type,
            entity_id,
        } => InvitationCommand::Decline {
            invitation_id,
            entity_type,
            entity_id,
        },
        cli::InvitationCommands::Join {
            entity_type,
            entity_id,
            invitation_key,
            action,
        } => InvitationCommand::Join {
            entity_type,
            entity_id,
            invitation_key,
            action,
        },
        cli::InvitationCommands::Delete {
            invitation_id,
            entity_type,
            entity_id,
        } => InvitationCommand::Delete {
            invitation_id,
            entity_type,
            entity_id,
        },
    }
}

/// Convert clap-parsed files commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_files_command(cmd: cli::FilesCommands) -> FilesCommand {
    match cmd {
        cli::FilesCommands::List {
            workspace,
            share,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => FilesCommand::List {
            workspace,
            share,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        },
        cli::FilesCommands::Info {
            workspace,
            share,
            node_ids,
        } => FilesCommand::Info {
            workspace,
            share,
            node_ids,
        },
        cli::FilesCommands::CreateFolder {
            workspace,
            share,
            name,
            parent,
            force,
        } => FilesCommand::CreateFolder {
            workspace,
            share,
            name,
            parent,
            force,
        },
        cli::FilesCommands::Move {
            workspace,
            share,
            node_id,
            to,
        } => FilesCommand::Move {
            workspace,
            share,
            node_id,
            to,
        },
        cli::FilesCommands::Copy {
            workspace,
            share,
            node_id,
            to,
        } => FilesCommand::Copy {
            workspace,
            share,
            node_id,
            to,
        },
        cli::FilesCommands::Rename {
            workspace,
            share,
            node_id,
            new_name,
        } => FilesCommand::Rename {
            workspace,
            share,
            node_id,
            new_name,
        },
        cli::FilesCommands::Update {
            workspace,
            share,
            node_id,
            name,
            from,
            metadata_title,
            metadata_short,
            if_version,
        } => FilesCommand::Update {
            workspace,
            share,
            node_id,
            name,
            from,
            metadata_title,
            metadata_short,
            if_version,
        },
        cli::FilesCommands::AddFile {
            workspace,
            share,
            name,
            parent,
            upload_id,
            hash,
            hash_type,
        } => FilesCommand::AddFile {
            workspace,
            share,
            parent,
            name,
            upload_id,
            hash,
            hash_type,
        },
        cli::FilesCommands::Delete {
            workspace,
            share,
            node_id,
        } => FilesCommand::Delete {
            workspace,
            share,
            node_id,
        },
        cli::FilesCommands::Restore {
            workspace,
            share,
            node_id,
        } => FilesCommand::Restore {
            workspace,
            share,
            node_id,
        },
        cli::FilesCommands::Purge {
            workspace,
            share,
            node_id,
        } => FilesCommand::Purge {
            workspace,
            share,
            node_id,
        },
        cli::FilesCommands::Trash {
            workspace,
            share,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => FilesCommand::Trash {
            workspace,
            share,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        },
        cli::FilesCommands::Versions {
            workspace,
            share,
            node_id,
        } => FilesCommand::Versions {
            workspace,
            share,
            node_id,
        },
        cli::FilesCommands::Search {
            workspace,
            share,
            query,
            limit,
            offset,
            scope,
            folder_scope,
            filters,
            details,
            modes,
            page_size: _,
            cursor: _,
        } => FilesCommand::Search {
            // clap guarantees exactly one of --workspace / --share
            // (`conflicts_with` + `required_unless_present`). Written as four
            // explicit arms so it FAILS CLOSED if that guarantee ever regresses:
            // "both" and "neither" resolve to an empty id, which the validator
            // rejects with the normal "must not be empty" error. Preferring one
            // silently would search a profile the caller did not name — the
            // defect the clap rule exists to prevent. No `unwrap()`: the
            // unreachable arms yield `String::new()`, never a panic.
            target: match (share, workspace) {
                (Some(id), None) => commands::files::SearchTarget::Share(id),
                (None, Some(id)) => commands::files::SearchTarget::Workspace(id),
                (Some(_), Some(_)) | (None, None) => {
                    commands::files::SearchTarget::Workspace(String::new())
                }
            },
            query,
            limit,
            offset,
            scope,
            folder_scope,
            // Carried through as the RAW argument: this mapper is infallible,
            // and `--filters` needs a fallible parse (shape validation plus
            // `@file` resolution). Both happen at the top of the `search`
            // command, still before any request is made.
            filters,
            details,
            modes: modes.to_params(),
        },
        cli::FilesCommands::Recent {
            workspace,
            share,
            page_size,
            cursor,
            node_type,
        } => FilesCommand::Recent {
            workspace,
            share,
            page_size,
            cursor,
            node_type,
        },
        cli::FilesCommands::AddLink {
            workspace,
            parent,
            share_id,
        } => FilesCommand::AddLink {
            workspace,
            parent,
            share_id,
        },
        cli::FilesCommands::Transfer {
            workspace,
            node_id,
            to_workspace,
        } => FilesCommand::Transfer {
            workspace,
            node_id,
            to_workspace,
        },
        cli::FilesCommands::VersionRestore {
            workspace,
            share,
            node_id,
            version_id,
        } => FilesCommand::VersionRestore {
            workspace,
            share,
            node_id,
            version_id,
        },
        cli::FilesCommands::Lock(lk) => FilesCommand::Lock(map_file_lock_command(lk)),
        cli::FilesCommands::Read {
            workspace,
            share,
            node_id,
        } => FilesCommand::Read {
            workspace,
            share,
            node_id,
        },
        cli::FilesCommands::Content {
            workspace,
            share,
            node_id,
            nodes,
            query,
            page,
            chunk_from,
            chunk_to,
            cursor,
            limit,
            max_bytes,
        } => FilesCommand::Content {
            workspace,
            share,
            node_id,
            nodes,
            query,
            page,
            chunk_from,
            chunk_to,
            cursor,
            limit,
            max_bytes,
        },
    }
}

/// Convert clap-parsed file lock subcommands to the internal enum.
fn map_file_lock_command(lk: cli::FileLockCommands) -> FileLockCommand {
    match lk {
        cli::FileLockCommands::Acquire {
            workspace,
            node_id,
            duration,
            client_info,
            lock_token_file,
        } => FileLockCommand::Acquire {
            workspace,
            node_id,
            duration,
            client_info,
            lock_token_file,
        },
        cli::FileLockCommands::Status { workspace, node_id } => {
            FileLockCommand::Status { workspace, node_id }
        }
        cli::FileLockCommands::Release {
            workspace,
            node_id,
            lock_token,
        } => FileLockCommand::Release {
            workspace,
            node_id,
            lock_token,
        },
    }
}

/// Convert clap-parsed upload commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_upload_command(cmd: cli::UploadCommands) -> UploadCommand {
    match cmd {
        cli::UploadCommands::File {
            workspace,
            file_paths,
            folder,
            preserve_tree,
            allow_partial,
            creator,
        } => UploadCommand::File {
            workspace,
            file_paths,
            folder,
            preserve_tree,
            allow_partial,
            creator,
        },
        cli::UploadCommands::Text {
            workspace,
            share,
            name,
            content,
            folder,
        } => UploadCommand::Text {
            workspace,
            share,
            name,
            content,
            folder,
        },
        cli::UploadCommands::Url {
            workspace,
            url,
            folder,
            name,
        } => UploadCommand::Url {
            workspace,
            url,
            folder,
            name,
        },
        cli::UploadCommands::CreateSession {
            workspace,
            share,
            filename,
            filesize,
            folder,
        } => UploadCommand::CreateSession {
            workspace,
            share,
            filename,
            filesize,
            folder,
        },
        cli::UploadCommands::Chunk {
            upload_key,
            chunk_num,
            file,
        } => UploadCommand::Chunk {
            upload_key,
            chunk_num,
            file,
        },
        cli::UploadCommands::Finalize { upload_key } => UploadCommand::Finalize { upload_key },
        cli::UploadCommands::Status { upload_key } => UploadCommand::Status { upload_key },
        cli::UploadCommands::Cancel { upload_key } => UploadCommand::Cancel { upload_key },
        cli::UploadCommands::ListSessions => UploadCommand::ListSessions,
        cli::UploadCommands::CancelAll => UploadCommand::CancelAll,
        cli::UploadCommands::ChunkStatus { upload_key } => {
            UploadCommand::ChunkStatus { upload_key }
        }
        cli::UploadCommands::ChunkDelete {
            upload_key,
            chunk_num,
        } => UploadCommand::ChunkDelete {
            upload_key,
            chunk_num,
        },
        cli::UploadCommands::WebList {
            limit,
            offset,
            status,
        } => UploadCommand::WebList {
            limit,
            offset,
            status,
        },
        cli::UploadCommands::WebCancel { upload_id } => UploadCommand::WebCancel { upload_id },
        cli::UploadCommands::WebStatus { upload_id } => UploadCommand::WebStatus { upload_id },
        cli::UploadCommands::Limits {
            action,
            org,
            instance_id,
            folder_id,
            file_id,
        } => UploadCommand::Limits {
            action,
            org,
            instance_id,
            folder_id,
            file_id,
        },
        cli::UploadCommands::Algos => UploadCommand::Algos,
        cli::UploadCommands::Extensions { plan } => UploadCommand::Extensions { plan },
        cli::UploadCommands::Stream {
            workspace,
            share,
            file_path,
            folder,
            max_size,
            name,
            hash,
            hash_algo,
        } => UploadCommand::Stream {
            workspace,
            share,
            file_path,
            folder,
            max_size,
            name,
            hash,
            hash_algo,
        },
        cli::UploadCommands::CreateStreamSession {
            workspace,
            share,
            filename,
            folder,
            max_size,
        } => UploadCommand::CreateStreamSession {
            workspace,
            share,
            filename,
            folder,
            max_size,
        },
        cli::UploadCommands::StreamSend {
            upload_key,
            file,
            max_size,
            hash,
            hash_algo,
        } => UploadCommand::StreamSend {
            upload_key,
            file,
            max_size,
            hash,
            hash_algo,
        },
    }
}

/// Convert clap-parsed download commands to the internal enum.
fn map_download_command(cmd: cli::DownloadCommands) -> DownloadCommand {
    match cmd {
        cli::DownloadCommands::File {
            workspace,
            share,
            node_id,
            output,
            version,
        } => DownloadCommand::File {
            workspace,
            share,
            node_id,
            output_path: output,
            version,
        },
        cli::DownloadCommands::Folder {
            workspace,
            share,
            node_id,
            output,
        } => DownloadCommand::Folder {
            workspace,
            share,
            node_id,
            output_path: output,
        },
        cli::DownloadCommands::Batch {
            workspace,
            node_ids,
            output_dir,
        } => DownloadCommand::Batch {
            workspace,
            node_ids,
            output_dir,
        },
    }
}

/// Convert clap-parsed share commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_share_command(cmd: cli::ShareCommands) -> ShareCommand {
    match cmd {
        cli::ShareCommands::List { limit, offset } => ShareCommand::List { limit, offset },
        cli::ShareCommands::Create {
            name,
            workspace,
            share_type,
            description,
            access_options,
            invite,
            storage_mode,
            folder_node_id,
            create_folder,
            folder_name,
            custom_name,
            password,
            expires,
            notify,
            comments_enabled,
            guest_chat_enabled,
            display_type,
            workspace_style,
            anonymous_uploads,
            intelligence,
            download_security,
            accent_color,
            background_color1,
            background_color2,
            background_image,
            link_1,
            link_2,
            link_3,
            owner_defined,
        } => ShareCommand::Create(Box::new(ShareCreateArgs {
            workspace_id: workspace,
            name,
            share_type,
            description,
            access_options,
            invite,
            storage_mode,
            folder_node_id,
            create_folder,
            folder_name,
            custom_name,
            password,
            expires,
            notify,
            comments_enabled,
            guest_chat_enabled,
            display_type,
            workspace_style,
            anonymous_uploads,
            intelligence,
            download_security,
            accent_color,
            background_color1,
            background_color2,
            background_image,
            link_1,
            link_2,
            link_3,
            owner_defined,
        })),
        cli::ShareCommands::Info { share_id } => ShareCommand::Info { share_id },
        cli::ShareCommands::Update {
            share_id,
            name,
            title,
            custom_name,
            description,
            share_type,
            access_options,
            invite,
            password,
            expires,
            notify,
            download_enabled,
            comments_enabled,
            download_security,
            display_type,
            workspace_style,
            guest_chat_enabled,
            intelligence,
            anonymous_uploads,
            accent_color,
            background_color1,
            background_color2,
            background_image,
            link_1,
            link_2,
            link_3,
            owner_defined,
            share_link_node_id,
        } => ShareCommand::Update(Box::new(ShareUpdateArgs {
            share_id,
            name,
            title,
            custom_name,
            description,
            share_type,
            access_options,
            invite,
            password,
            expires,
            notify,
            download_enabled,
            comments_enabled,
            download_security,
            display_type,
            workspace_style,
            guest_chat_enabled,
            intelligence,
            anonymous_uploads,
            accent_color,
            background_color1,
            background_color2,
            background_image,
            link_1,
            link_2,
            link_3,
            owner_defined,
            share_link_node_id,
        })),
        cli::ShareCommands::Delete { share_id, confirm } => {
            ShareCommand::Delete { share_id, confirm }
        }
        cli::ShareCommands::Archive { share_id } => ShareCommand::Archive { share_id },
        cli::ShareCommands::Unarchive { share_id } => ShareCommand::Unarchive { share_id },
        cli::ShareCommands::PasswordAuth { share_id, password } => {
            ShareCommand::PasswordAuth { share_id, password }
        }
        cli::ShareCommands::GuestAuth { share_id } => ShareCommand::GuestAuth { share_id },
        cli::ShareCommands::PublicInfo { share_id } => ShareCommand::PublicInfo { share_id },
        cli::ShareCommands::Available => ShareCommand::Available,
        cli::ShareCommands::CheckName { name } => ShareCommand::CheckName { name },
        cli::ShareCommands::Files(f) => ShareCommand::Files(map_share_files_command(f)),
        cli::ShareCommands::Members(m) => ShareCommand::Members(map_share_members_command(m)),
        cli::ShareCommands::Invitation(i) => {
            ShareCommand::Invitation(map_share_invitation_command(i))
        }
    }
}

/// Convert clap-parsed share files subcommands to the internal enum.
fn map_share_files_command(f: cli::ShareFilesCommands) -> ShareFilesCommand {
    match f {
        cli::ShareFilesCommands::List {
            share_id,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => ShareFilesCommand::List {
            share_id,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        },
    }
}

/// Convert clap-parsed share members subcommands to the internal enum.
fn map_share_members_command(m: cli::ShareMembersCommands) -> ShareMembersCommand {
    match m {
        cli::ShareMembersCommands::List {
            share_id,
            limit,
            offset,
        } => ShareMembersCommand::List {
            share_id,
            limit,
            offset,
        },
        cli::ShareMembersCommands::Add {
            share_id,
            email,
            role,
            notify_options,
            expires,
            force_notification,
            message,
            invitation_expires,
        } => ShareMembersCommand::Add {
            share_id,
            email,
            role,
            notify_options,
            expires,
            force_notification,
            message,
            invitation_expires,
        },
        cli::ShareMembersCommands::Update {
            share_id,
            member_id,
            role,
            notify_options,
            expires,
        } => ShareMembersCommand::Update {
            share_id,
            member_id,
            role,
            notify_options,
            expires,
        },
        cli::ShareMembersCommands::Info {
            share_id,
            member_id,
        } => ShareMembersCommand::Info {
            share_id,
            member_id,
        },
        cli::ShareMembersCommands::Transfer {
            share_id,
            member_id,
        } => ShareMembersCommand::Transfer {
            share_id,
            member_id,
        },
        cli::ShareMembersCommands::Leave { share_id } => ShareMembersCommand::Leave { share_id },
        cli::ShareMembersCommands::Join { share_id } => ShareMembersCommand::Join { share_id },
        cli::ShareMembersCommands::Remove {
            share_id,
            member_id,
        } => ShareMembersCommand::Remove {
            share_id,
            member_id,
        },
    }
}

/// Convert clap-parsed share invitation subcommands to the internal enum.
fn map_share_invitation_command(i: cli::ShareInvitationCommands) -> ShareInvitationCommand {
    match i {
        cli::ShareInvitationCommands::List { share_id, state } => {
            ShareInvitationCommand::List { share_id, state }
        }
        cli::ShareInvitationCommands::Update {
            share_id,
            invitation_id,
            state,
            role,
            notify_options,
            expires,
        } => ShareInvitationCommand::Update {
            share_id,
            invitation_id,
            state,
            role,
            notify_options,
            expires,
        },
        cli::ShareInvitationCommands::Delete {
            share_id,
            invitation_id,
        } => ShareInvitationCommand::Delete {
            share_id,
            invitation_id,
        },
    }
}

/// Convert clap-parsed comment commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_comment_command(cmd: cli::CommentCommands) -> CommentCommand {
    match cmd {
        cli::CommentCommands::List {
            node_id,
            entity_type,
            entity_id,
            sort,
            limit,
            offset,
        } => CommentCommand::List {
            entity_type,
            entity_id,
            node_id,
            sort,
            limit,
            offset,
        },
        cli::CommentCommands::Create {
            node_id,
            text,
            entity_type,
            entity_id,
            reference,
            properties,
            target_id,
            target_ids,
        } => CommentCommand::Create {
            entity_type,
            entity_id,
            node_id,
            text,
            reference,
            properties,
            target_id,
            target_ids,
        },
        cli::CommentCommands::Reply {
            comment_id,
            text,
            node_id,
            entity_type,
            entity_id,
        } => CommentCommand::Reply {
            entity_type,
            entity_id,
            node_id,
            comment_id,
            text,
        },
        cli::CommentCommands::Edit { comment_id, text } => {
            CommentCommand::Edit { comment_id, text }
        }
        cli::CommentCommands::Delete { comment_id } => CommentCommand::Delete { comment_id },
        cli::CommentCommands::ListAll {
            entity_type,
            entity_id,
            sort,
            limit,
            offset,
        } => CommentCommand::ListAll {
            entity_type,
            entity_id,
            sort,
            limit,
            offset,
        },
        cli::CommentCommands::Info { comment_id } => CommentCommand::Info { comment_id },
        cli::CommentCommands::React { comment_id, emoji } => {
            CommentCommand::React { comment_id, emoji }
        }
        cli::CommentCommands::Unreact { comment_id } => CommentCommand::Unreact { comment_id },
        cli::CommentCommands::BulkDelete { comment_ids } => {
            CommentCommand::BulkDelete { comment_ids }
        }
        cli::CommentCommands::Attachments { comment_id } => {
            CommentCommand::Attachments { comment_id }
        }
        cli::CommentCommands::Attach {
            comment_id,
            target_id,
            target_ids,
        } => CommentCommand::Attach {
            comment_id,
            target_id,
            target_ids,
        },
        cli::CommentCommands::Detach {
            comment_id,
            target_id,
        } => CommentCommand::Detach {
            comment_id,
            target_id,
        },
    }
}

/// Convert clap-parsed event commands to the internal enum.
fn map_event_command(cmd: cli::EventCommands) -> EventCommand {
    match cmd {
        cli::EventCommands::List {
            workspace,
            share,
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            limit,
            offset,
        } => EventCommand::List {
            workspace,
            share,
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            limit,
            offset,
        },
        cli::EventCommands::Info { event_id } => EventCommand::Info { event_id },
        cli::EventCommands::Poll {
            entity_id,
            lastactivity,
            wait,
        } => EventCommand::Poll {
            entity_id,
            lastactivity,
            wait,
        },
        cli::EventCommands::Ack { event_id } => EventCommand::Ack { event_id },
        cli::EventCommands::Summarize {
            workspace,
            share,
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            user_context,
            limit,
            offset,
        } => EventCommand::Summarize {
            workspace,
            share,
            user_id,
            org_id,
            event,
            category,
            subcategory,
            parent_event_id,
            calling_user_id,
            object_id,
            visibility,
            acknowledged,
            created_min,
            created_max,
            user_context,
            limit,
            offset,
        },
    }
}

/// Convert clap-parsed preview commands to the internal enum.
fn map_preview_command(cmd: cli::PreviewCommands) -> PreviewCommand {
    match cmd {
        cli::PreviewCommands::Get {
            node_id,
            preview_type,
            context_type,
            context_id,
        } => PreviewCommand::Get {
            context_type,
            context_id,
            node_id,
            preview_type,
        },
        cli::PreviewCommands::Thumbnail {
            node_id,
            context_type,
            context_id,
        } => PreviewCommand::Thumbnail {
            context_type,
            context_id,
            node_id,
        },
        cli::PreviewCommands::Transform {
            node_id,
            transform_name,
            context_type,
            context_id,
            width,
            height,
            output_format,
            size,
            crop_width,
            crop_height,
            crop_x,
            crop_y,
            rotate,
        } => PreviewCommand::Transform {
            context_type,
            context_id,
            node_id,
            transform_name,
            width,
            height,
            output_format,
            size,
            crop_width,
            crop_height,
            crop_x,
            crop_y,
            rotate,
        },
    }
}

/// Convert clap-parsed asset commands to the internal enum.
fn map_asset_command(cmd: cli::AssetCommands) -> AssetCommand {
    match cmd {
        cli::AssetCommands::Upload {
            asset_type,
            file,
            entity_type,
            entity_id,
        } => AssetCommand::Upload {
            entity_type,
            entity_id,
            asset_type,
            file,
        },
        cli::AssetCommands::Remove {
            asset_type,
            entity_type,
            entity_id,
        } => AssetCommand::Remove {
            entity_type,
            entity_id,
            asset_type,
        },
        cli::AssetCommands::List {
            entity_type,
            entity_id,
        } => AssetCommand::List {
            entity_type,
            entity_id,
        },
        cli::AssetCommands::Types { entity_type } => AssetCommand::Types { entity_type },
    }
}

/// Convert clap-parsed Ripley (AI agent) commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_ripley_command(cmd: cli::RipleyCommands) -> AiCommand {
    use commands::ai::AskScopeFlags;
    match cmd {
        cli::RipleyCommands::Ask {
            workspace,
            share,
            question,
            files_scope,
            folders_scope,
            files_attach,
            personality,
            kind,
            no_wait,
            idempotency_key,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Ask {
                profile_type,
                profile_id,
                question,
                scope: AskScopeFlags {
                    files_scope,
                    folders_scope,
                    files_attach,
                },
                personality,
                kind,
                no_wait,
                idempotency_key,
            }
        }
        cli::RipleyCommands::List {
            workspace,
            share,
            kind,
            deleted,
            limit,
            offset,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::List {
                profile_type,
                profile_id,
                kind,
                deleted,
                limit,
                offset,
            }
        }
        cli::RipleyCommands::Details {
            workspace,
            share,
            chat_id,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Details {
                profile_type,
                profile_id,
                chat_id,
            }
        }
        cli::RipleyCommands::Messages {
            workspace,
            share,
            chat_id,
            limit,
            offset,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Messages {
                profile_type,
                profile_id,
                chat_id,
                limit,
                offset,
            }
        }
        cli::RipleyCommands::Message {
            workspace,
            share,
            chat_id,
            message_id,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Message {
                profile_type,
                profile_id,
                chat_id,
                message_id,
            }
        }
        cli::RipleyCommands::Update {
            workspace,
            share,
            chat_id,
            name,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Update {
                profile_type,
                profile_id,
                chat_id,
                name,
            }
        }
        cli::RipleyCommands::Publish {
            workspace,
            share,
            chat_id,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Publish {
                profile_type,
                profile_id,
                chat_id,
            }
        }
        cli::RipleyCommands::Delete {
            workspace,
            share,
            chat_id,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Delete {
                profile_type,
                profile_id,
                chat_id,
            }
        }
        cli::RipleyCommands::Transactions { workspace } => AiCommand::Transactions { workspace },
        cli::RipleyCommands::Autotitle {
            share,
            user_context,
        } => AiCommand::Autotitle {
            share,
            user_context,
        },
        cli::RipleyCommands::Delegate { instruction, .. } => AiCommand::Delegate { instruction },
        cli::RipleyCommands::Status { id } => AiCommand::JobStatus { id },
        cli::RipleyCommands::Logs { id } => AiCommand::JobLogs { id },
        cli::RipleyCommands::CancelJob { id } => AiCommand::JobCancel { id },
        cli::RipleyCommands::Chat {
            workspace,
            message,
            chat_id,
            files_scope,
            folders_scope,
            files_attach,
            node_ids,
            folder_id,
            intelligence,
            idempotency_key,
        } => AiCommand::Chat {
            workspace,
            message,
            chat_id,
            files_scope,
            folders_scope,
            files_attach,
            node_ids,
            folder_id,
            intelligence,
            idempotency_key,
        },
        cli::RipleyCommands::History {
            workspace,
            chat_id,
            limit,
            offset,
        } => AiCommand::History {
            workspace,
            chat_id,
            limit,
            offset,
        },
        cli::RipleyCommands::Summary {
            workspace,
            node_ids,
        } => AiCommand::Summary {
            workspace,
            node_ids,
        },
        cli::RipleyCommands::Cancel {
            workspace,
            share,
            chat_id,
        } => {
            let (profile_type, profile_id) = resolve_workspace_or_share_profile(workspace, share);
            AiCommand::Cancel {
                profile_type,
                profile_id,
                chat_id,
            }
        }
    }
}

/// Resolve workspace/share options into a `(profile_type, profile_id)` pair.
///
/// Clap ensures at least one of `workspace` or `share` is present via
/// `required_unless_present`. If both are `None` (should not happen), returns
/// an empty workspace profile ID which will fail server-side.
fn resolve_workspace_or_share_profile(
    workspace: Option<String>,
    share: Option<String>,
) -> (String, String) {
    if let Some(sid) = share {
        ("share".to_owned(), sid)
    } else if let Some(wid) = workspace {
        ("workspace".to_owned(), wid)
    } else {
        // Defensive: clap should prevent reaching here.
        ("workspace".to_owned(), String::new())
    }
}

/// Map CLI apps subcommand to command variant.
fn map_apps_command(cmd: &cli::AppsCommands) -> AppsCommand {
    match cmd {
        cli::AppsCommands::List => AppsCommand::List,
    }
}

/// Map CLI import subcommand to command variant.
#[allow(clippy::too_many_lines)]
fn map_import_command(cmd: &cli::ImportCommands) -> ImportCommand {
    match cmd {
        cli::ImportCommands::ListProviders { workspace } => ImportCommand::ListProviders {
            workspace_id: workspace.clone(),
        },
        cli::ImportCommands::ListIdentities {
            workspace,
            limit,
            offset,
        } => ImportCommand::ListIdentities {
            workspace_id: workspace.clone(),
            limit: *limit,
            offset: *offset,
        },
        cli::ImportCommands::ProvisionIdentity {
            workspace,
            provider,
            account_type,
        } => ImportCommand::ProvisionIdentity {
            workspace_id: workspace.clone(),
            provider: provider.clone(),
            account_type: account_type.clone(),
        },
        cli::ImportCommands::IdentityDetails {
            workspace,
            identity_id,
        } => ImportCommand::IdentityDetails {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
        },
        cli::ImportCommands::RevokeIdentity {
            workspace,
            identity_id,
        } => ImportCommand::RevokeIdentity {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
        },
        cli::ImportCommands::ListSources {
            workspace,
            status,
            limit,
            offset,
        } => ImportCommand::ListSources {
            workspace_id: workspace.clone(),
            status: status.clone(),
            limit: *limit,
            offset: *offset,
        },
        cli::ImportCommands::ListDrives {
            workspace,
            identity_id,
            site_path,
            limit,
            offset,
        } => ImportCommand::ListDrives {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
            site_path: site_path.clone(),
            limit: *limit,
            offset: *offset,
        },
        cli::ImportCommands::RefreshDrives {
            workspace,
            identity_id,
            site_path,
        } => ImportCommand::RefreshDrives {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
            site_path: site_path.clone(),
        },
        cli::ImportCommands::Discover {
            workspace,
            identity_id,
            drive_id,
            remote_path,
        } => ImportCommand::Discover {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
            drive_id: drive_id.clone(),
            remote_path: remote_path.clone(),
        },
        cli::ImportCommands::CreateSource {
            workspace,
            identity_id,
            remote_path,
            remote_name,
            sync_interval,
            access_mode,
            drive_id,
            destination_node_id,
        } => ImportCommand::CreateSource {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
            remote_path: remote_path.clone(),
            remote_name: remote_name.clone(),
            sync_interval: *sync_interval,
            access_mode: access_mode.clone(),
            drive_id: drive_id.clone(),
            destination_node_id: destination_node_id.clone(),
        },
        cli::ImportCommands::SourceDetails { source_id } => ImportCommand::SourceDetails {
            source_id: source_id.clone(),
        },
        cli::ImportCommands::UpdateSource {
            source_id,
            sync_interval,
            status,
            remote_name,
            access_mode,
        } => ImportCommand::UpdateSource {
            source_id: source_id.clone(),
            sync_interval: *sync_interval,
            status: status.clone(),
            remote_name: remote_name.clone(),
            access_mode: access_mode.clone(),
        },
        cli::ImportCommands::DeleteSource { source_id } => ImportCommand::DeleteSource {
            source_id: source_id.clone(),
        },
        cli::ImportCommands::Disconnect { source_id, action } => ImportCommand::Disconnect {
            source_id: source_id.clone(),
            action: action.clone(),
        },
        cli::ImportCommands::Refresh { source_id } => ImportCommand::Refresh {
            source_id: source_id.clone(),
        },
        cli::ImportCommands::ListJobs {
            source_id,
            limit,
            offset,
        } => ImportCommand::ListJobs {
            source_id: source_id.clone(),
            limit: *limit,
            offset: *offset,
        },
        cli::ImportCommands::JobDetails { source_id, job_id } => ImportCommand::JobDetails {
            source_id: source_id.clone(),
            job_id: job_id.clone(),
        },
        cli::ImportCommands::CancelJob { source_id, job_id } => ImportCommand::CancelJob {
            source_id: source_id.clone(),
            job_id: job_id.clone(),
        },
        cli::ImportCommands::ListWritebacks {
            source_id,
            status,
            limit,
            offset,
        } => ImportCommand::ListWritebacks {
            source_id: source_id.clone(),
            status: status.clone(),
            limit: *limit,
            offset: *offset,
        },
        cli::ImportCommands::WritebackDetails {
            source_id,
            writeback_id,
        } => ImportCommand::WritebackDetails {
            source_id: source_id.clone(),
            writeback_id: writeback_id.clone(),
        },
        cli::ImportCommands::PushWriteback { source_id, node_id } => ImportCommand::PushWriteback {
            source_id: source_id.clone(),
            node_id: node_id.clone(),
        },
        cli::ImportCommands::RetryWriteback {
            source_id,
            writeback_id,
        } => ImportCommand::RetryWriteback {
            source_id: source_id.clone(),
            writeback_id: writeback_id.clone(),
        },
        cli::ImportCommands::ResolveConflict {
            source_id,
            writeback_id,
            resolution,
        } => ImportCommand::ResolveConflict {
            source_id: source_id.clone(),
            writeback_id: writeback_id.clone(),
            resolution: resolution.clone(),
        },
        cli::ImportCommands::CancelWriteback {
            source_id,
            writeback_id,
        } => ImportCommand::CancelWriteback {
            source_id: source_id.clone(),
            writeback_id: writeback_id.clone(),
        },
    }
}

/// Map CLI lock subcommand to command variant.
/// Map the clap `intents` subcommand onto the domain command enum.
fn map_intents_command(cmd: &cli::IntentsCommands) -> IntentsCommand {
    match cmd {
        cli::IntentsCommands::Allocate {
            workspace,
            node_id,
            intent,
        } => IntentsCommand::Allocate {
            workspace: workspace.clone(),
            node_id: node_id.clone(),
            intent: intent.clone(),
        },
        cli::IntentsCommands::List { workspace, cursor } => IntentsCommand::List {
            workspace: workspace.clone(),
            cursor: cursor.clone(),
        },
        cli::IntentsCommands::Fill {
            workspace,
            intent_id,
            version,
            topic,
            message,
            intent,
        } => IntentsCommand::Fill {
            workspace: workspace.clone(),
            intent_id: intent_id.clone(),
            version: *version,
            topic: topic.clone(),
            message: message.clone(),
            intent: intent.clone(),
        },
        cli::IntentsCommands::Get {
            workspace,
            intent_ids,
        } => IntentsCommand::Get {
            workspace: workspace.clone(),
            intent_ids: intent_ids.clone(),
        },
        cli::IntentsCommands::Release {
            workspace,
            intent_id,
        } => IntentsCommand::Release {
            workspace: workspace.clone(),
            intent_id: intent_id.clone(),
        },
    }
}

fn map_lock_command(cmd: &cli::LockCommands) -> LockCommand {
    match cmd {
        cli::LockCommands::Acquire {
            context_type,
            context_id,
            node_id,
            duration,
            client_info,
            lock_token_file,
        } => LockCommand::Acquire {
            context_type: context_type.clone(),
            context_id: context_id.clone(),
            node_id: node_id.clone(),
            duration: *duration,
            client_info: client_info.clone(),
            lock_token_file: lock_token_file.clone(),
        },
        cli::LockCommands::Status {
            context_type,
            context_id,
            node_id,
        } => LockCommand::Status {
            context_type: context_type.clone(),
            context_id: context_id.clone(),
            node_id: node_id.clone(),
        },
        cli::LockCommands::Release {
            context_type,
            context_id,
            node_id,
            lock_token,
        } => LockCommand::Release {
            context_type: context_type.clone(),
            context_id: context_id.clone(),
            node_id: node_id.clone(),
            lock_token: lock_token.clone(),
        },
        cli::LockCommands::Heartbeat {
            context_type,
            context_id,
            node_id,
            lock_token,
        } => LockCommand::Heartbeat {
            context_type: context_type.clone(),
            context_id: context_id.clone(),
            node_id: node_id.clone(),
            lock_token: lock_token.clone(),
        },
    }
}

/// Convert clap-parsed unified-search commands to the internal enum.
fn map_search_command(cmd: cli::SearchCommands) -> SearchCommand {
    use fastio_cli::api::search::UnifiedSearchParams;
    match cmd {
        cli::SearchCommands::Workspace {
            workspace_id,
            query,
            files_limit,
            files_offset,
            metadata_limit,
            metadata_offset,
            comments_limit,
            comments_offset,
            only,
            details,
            modes,
        } => SearchCommand::Workspace {
            workspace_id,
            query,
            params: UnifiedSearchParams::new()
                .files(files_offset, files_limit)
                .metadata(metadata_offset, metadata_limit)
                .comments(comments_offset, comments_limit)
                .details(details)
                .modes(modes.to_params()),
            only,
        },
        cli::SearchCommands::Share {
            share_id,
            query,
            files_limit,
            files_offset,
            comments_limit,
            comments_offset,
            only,
            details,
            modes,
        } => SearchCommand::Share {
            share_id,
            query,
            // Sent on the share leg too: the parameter is accepted there and
            // simply never yields facts. Suppressing it client-side would be a
            // second, divergent rule for a route the server already handles.
            params: UnifiedSearchParams::new()
                .files(files_offset, files_limit)
                .comments(comments_offset, comments_limit)
                .details(details)
                .modes(modes.to_params()),
            only,
        },
    }
}

/// Convert clap-parsed metadata commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_metadata_command(cmd: cli::MetadataCommands) -> MetadataCommand {
    match cmd {
        cli::MetadataCommands::Eligible {
            workspace,
            page_size,
            cursor,
            mimetype,
            extension,
            limit,
            offset,
        } => MetadataCommand::Eligible {
            workspace,
            page_size,
            cursor,
            mimetype,
            extension,
            // The retired offset-pagination flags are not forwarded — the
            // server ignores them — but their PRESENCE is, so the command can
            // say they were ignored instead of dropping them silently.
            used_offset_pagination: limit.is_some() || offset.is_some(),
        },
        cli::MetadataCommands::Details {
            workspace,
            node_ids,
        } => MetadataCommand::Details {
            workspace,
            node_ids,
        },
        cli::MetadataCommands::Extract {
            workspace,
            node_id,
            fields,
            wait,
            poll_interval,
            confirm_ai_spend,
        } => MetadataCommand::Extract {
            workspace,
            node_id,
            fields,
            wait,
            poll_interval,
            confirm_ai_spend,
        },
        cli::MetadataCommands::Search {
            workspace,
            query,
            limit,
            offset,
        } => MetadataCommand::Search {
            workspace,
            query,
            limit,
            offset,
        },
    }
}

/// Convert clap-parsed system commands to the internal enum.
fn map_system_command(cmd: &cli::SystemCommands) -> SystemCommand {
    match cmd {
        cli::SystemCommands::Ping => SystemCommand::Ping,
        cli::SystemCommands::Status => SystemCommand::Status,
    }
}

#[cfg(test)]
mod tests {
    use super::{EXIT_CONFLICT, EXIT_FAILURE, cli_error_render, exit_code_for};
    use fastio_cli::error::{ApiError, CliError};

    /// An ordinary API failure keeps the historical exit code, so nothing that
    /// already checks for non-zero (or for `1` specifically) changes meaning.
    #[test]
    fn exit_code_ordinary_api_failure_is_failure() {
        let err = CliError::Api(ApiError::new(262_889, None, "bad id".to_owned(), 406));
        assert_eq!(exit_code_for(&err), EXIT_FAILURE);
        let err = CliError::Api(ApiError::new(0, None, "boom".to_owned(), 500));
        assert_eq!(exit_code_for(&err), EXIT_FAILURE);
    }

    /// A re-hinted API error (`MappedApi`) classifies exactly like the `Api` it
    /// wraps — the `FileShare` and AI paths re-hint API errors into one, and
    /// matching only `Api` would send a re-hinted CAS conflict to generic
    /// failure.
    #[test]
    fn exit_code_mapped_api_classifies_like_the_api_it_wraps() {
        let conflict = CliError::MappedApi {
            api: ApiError::new(
                182_596,
                Some("CONFLICT_VERSION_MISMATCH".to_owned()),
                "status conflict".to_owned(),
                409,
            ),
            hint: None,
        };
        assert_eq!(exit_code_for(&conflict), EXIT_CONFLICT);

        // …and an unrelated re-hinted error still exits generic failure.
        let unrelated = CliError::MappedApi {
            api: ApiError::new(1685, None, "quota".to_owned(), 412),
            hint: None,
        };
        assert_eq!(exit_code_for(&unrelated), EXIT_FAILURE);
    }

    /// A non-API error is never a conflict.
    #[test]
    fn exit_code_non_api_error_is_failure() {
        assert_eq!(
            exit_code_for(&CliError::Auth("nope".to_owned())),
            EXIT_FAILURE
        );
    }

    /// The shipped `FileShare` write-back conflict — parsed into its own variant,
    /// so it never reaches the `Api` arm. This is the only conflict the CLI can
    /// currently produce; without this the conflict exit code cannot fire at all.
    #[test]
    fn exit_code_version_conflict_variant_is_conflict() {
        let err = CliError::VersionConflict {
            current_version: "v7".to_owned(),
        };
        assert_eq!(exit_code_for(&err), EXIT_CONFLICT);
    }

    /// A CAS rejection is distinguishable from a generic failure — but only on a
    /// POSITIVE marker, delivered either as `error_code` or as `reason`.
    #[test]
    fn exit_code_marked_version_mismatch_is_conflict() {
        let by_code = CliError::Api(ApiError::new(
            1660,
            Some("CONFLICT_VERSION_MISMATCH".to_owned()),
            "stale base".to_owned(),
            409,
        ));
        assert_eq!(exit_code_for(&by_code), EXIT_CONFLICT);

        // The synchronous path carries it in the server's ONLY structured slot,
        // `error.params` — there is no top-level `reason` on this envelope.
        let by_reason = CliError::Api(
            ApiError::new(113_958, None, "stale base".to_owned(), 409).with_details(
                serde_json::json!({"params": {
                    "reason": "CONFLICT_VERSION_MISMATCH",
                    "current_version_id": "v3"
                }}),
            ),
        );
        assert_eq!(exit_code_for(&by_reason), EXIT_CONFLICT);
    }

    /// The API reuses 409 and 412 for things that are NOT compare-and-swap —
    /// email-already-in-use, 2FA-already-enabled, and (on 412) plan/quota limits.
    /// Classifying those as a conflict would tell an agent to re-read and retry a
    /// billing quota, so they must keep the historical generic failure code.
    #[test]
    fn exit_code_unrelated_409_and_412_are_not_conflicts() {
        // 409: "There email you specified is not available."
        let email_taken = CliError::Api(ApiError::new(
            10_025,
            None,
            "email not available".to_owned(),
            409,
        ));
        assert_eq!(exit_code_for(&email_taken), EXIT_FAILURE);

        // 412: `1685 (Feature Limit)` — a plan/quota refusal, never retryable.
        let quota = CliError::Api(ApiError::new(
            1685,
            None,
            "share creation limit reached".to_owned(),
            412,
        ));
        assert_eq!(exit_code_for(&quota), EXIT_FAILURE);
    }

    /// A conflict whose payload omits the marker — every conflict today, since
    /// the server does not send it yet — degrades to a plain conflict rather
    /// than being reported as contested.
    #[test]
    fn exit_code_conflict_without_marker_degrades() {
        let err = CliError::Api(
            ApiError::new(0, None, "stale base".to_owned(), 409)
                .with_details(serde_json::json!({"current_version": "v3"})),
        );
        assert_eq!(exit_code_for(&err), EXIT_FAILURE);
        // A falsey marker must not be read as contested either; without a
        // version-mismatch marker it stays a generic failure.
        let err = CliError::Api(
            ApiError::new(0, None, "stale base".to_owned(), 409)
                .with_details(serde_json::json!({"contested": false})),
        );
        assert_eq!(exit_code_for(&err), EXIT_FAILURE);
        // …but a falsey `contested` alongside a real mismatch marker is still a
        // conflict, just not a contested one.
        let err = CliError::Api(
            ApiError::new(0, None, "stale base".to_owned(), 409)
                .with_details(serde_json::json!({"contested": false,
                    "params": {"reason": "CONFLICT_VERSION_MISMATCH"}})),
        );
        assert_eq!(exit_code_for(&err), EXIT_CONFLICT);
    }

    /// A bare `CliError` (no `.context()` layered on) must render byte-identically
    /// to the legacy `render_stderr`: headline == the `CliError` `Display`, hint
    /// == its own `suggestion()`.
    #[test]
    fn cli_error_render_bare_uses_display_and_suggestion() {
        let cli_err = CliError::Api(ApiError::new(1670, None, "restricted".to_owned(), 403));
        let display = cli_err.to_string();
        let hint = cli_err.suggestion();
        // A bare CliError carries no added anyhow context.
        let err = anyhow::Error::from(CliError::Api(ApiError::new(
            1670,
            None,
            "restricted".to_owned(),
            403,
        )));
        let (headline, got_hint) = cli_error_render(&err, &cli_err);
        assert_eq!(
            headline, display,
            "bare headline must equal CliError Display"
        );
        assert_eq!(got_hint, hint, "bare hint must be the CliError suggestion");
        // The chosen hint is exactly the shared restricted hint for code 1670.
        assert_eq!(got_hint, Some(fastio_cli::error::HINT_RESTRICTED));
    }

    /// When `.context(...)` is layered on a `CliError`, the headline must surface
    /// the full anyhow chain (the added context AND the underlying error via the
    /// `{:#}` rendering), while the hint stays the `CliError`'s own suggestion —
    /// shown once, not doubled into the headline.
    #[test]
    fn cli_error_render_with_context_surfaces_chain() {
        let cli_err = CliError::Api(ApiError::new(0, None, "boom".to_owned(), 404));
        let underlying = cli_err.to_string();
        let hint = cli_err.suggestion();
        let context_msg =
            "envelope X: not ready yet — poll the envelope and retry once it completes";
        // Build the same chain a command handler would: context ON TOP of the CliError.
        let err = anyhow::Error::from(CliError::Api(ApiError::new(
            0,
            None,
            "boom".to_owned(),
            404,
        )))
        .context(context_msg);

        let (headline, got_hint) = cli_error_render(&err, &cli_err);
        // The added context reaches the user…
        assert!(
            headline.contains(context_msg),
            "headline must contain the added context: {headline}"
        );
        // …AND so does the underlying CliError (the {:#} chain), not just the top.
        assert!(
            headline.contains(&underlying),
            "headline must contain the underlying error: {headline}"
        );
        // The hint is the CliError suggestion, unchanged and not folded into the headline.
        assert_eq!(got_hint, hint, "hint must be the CliError suggestion");
        assert!(got_hint.is_some(), "a 404 CliError has a suggestion");
    }

    /// LV-1 regression: when `.context()` is layered onto a `CliError::Api`,
    /// the underlying `ApiError` `Display` block must appear EXACTLY ONCE in the
    /// headline — not twice. `thiserror`'s `#[from]` makes `CliError::Api`'s
    /// own `Display` AND its `ApiError` source link render identically, so a
    /// naive `{:#}` printed the `[HTTP …] … see: … resource: …` block twice in
    /// a row. `render_chain_dedup` collapses the consecutive duplicate.
    #[test]
    fn cli_error_render_dedups_doubled_api_block() {
        use fastio_cli::error::ApiError;
        // Same shape a command handler builds: `.context(...)` layered on a
        // `CliError::Api`. `thiserror`'s `#[from]` makes the chain carry the
        // `ApiError` Display twice in a row (once as `CliError::Api`'s own
        // forwarded Display, once as the source link), which the naive `{:#}`
        // rendered as a doubled block. The dedup must collapse it.
        let api = ApiError::new(
            146_422,
            None,
            "Signed PDF is not yet available for this document.".to_owned(),
            404,
        );
        let block = api.to_string();
        let cli_err = CliError::Api(api);
        let err = anyhow::Error::from(CliError::Api(ApiError::new(
            146_422,
            None,
            "Signed PDF is not yet available for this document.".to_owned(),
            404,
        )))
        .context("failed to download signed document");
        let (headline, _hint) = cli_error_render(&err, &cli_err);
        // The block (which itself contains "code 146422") must occur ONCE.
        assert_eq!(
            headline.matches("code 146422").count(),
            1,
            "the ApiError block must render exactly once, not doubled: {headline}"
        );
        // And it is still present (the context is also surfaced).
        assert!(
            headline.contains(&block),
            "block must be present: {headline}"
        );
        assert!(
            headline.contains("failed to download signed document"),
            "context must be present: {headline}"
        );
    }

    /// LV-1 + LV-2 regression for the signing not-ready path as the user sees it:
    /// the `ArtifactNotReady` variant carries no `ApiError` source link, so the
    /// `[HTTP 404] … (code …)` block renders exactly once even with context
    /// layered on, AND its hint is the poll-and-retry guidance — NOT the generic
    /// 404 "Verify the ID or path is correct.".
    #[test]
    fn cli_error_render_artifact_not_ready_single_block_and_poll_hint() {
        use fastio_cli::error::ApiError;
        let cli_err = CliError::ArtifactNotReady {
            api: ApiError::new(
                146_422,
                None,
                "Signed PDF is not yet available for this document.".to_owned(),
                404,
            ),
        };
        let err = anyhow::Error::from(CliError::ArtifactNotReady {
            api: ApiError::new(
                146_422,
                None,
                "Signed PDF is not yet available for this document.".to_owned(),
                404,
            ),
        })
        .context(
            "failed to download signed document: not ready yet — Poll the envelope and retry once \
             it completes.",
        );
        let (headline, hint) = cli_error_render(&err, &cli_err);
        // Exact rendered headline: context, then the server block ONCE.
        assert_eq!(
            headline,
            "failed to download signed document: not ready yet — Poll the envelope and retry once \
             it completes.: [HTTP 404] Signed PDF is not yet available for this document. \
             (code 146422)",
            "unexpected headline: {headline}"
        );
        // The code appears exactly once (no doubling — no ApiError source link).
        assert_eq!(
            headline.matches("code 146422").count(),
            1,
            "not-ready block must render exactly once: {headline}"
        );
        // The server code is carried through.
        assert!(
            headline.contains("146422"),
            "server code must surface: {headline}"
        );
        // The poll guidance is in the headline.
        assert!(
            headline.to_lowercase().contains("poll"),
            "poll guidance must surface: {headline}"
        );
        // The hint is the poll-appropriate one, NOT the generic-404 wording.
        let hint = hint.unwrap_or_default();
        assert!(
            !hint.contains("Verify the ID or path is correct"),
            "not-ready hint must NOT be the generic-404 hint: {hint}"
        );
        assert!(
            hint.to_lowercase().contains("poll") || hint.to_lowercase().contains("not ready"),
            "not-ready hint must steer to poll-and-retry: {hint}"
        );
    }

    /// D3: the E-Sign kill-switch failure is a `CliError::FeatureDisabled`, so
    /// it renders through the same red `error:` + yellow `hint:` path as every
    /// other `CliError`. The headline is the message (no
    /// "Configuration/Authentication error:" prefix), and the hint is the
    /// re-enable guidance.
    #[test]
    fn feature_disabled_renders_message_and_hint_without_prefix() {
        let cli_err = CliError::FeatureDisabled {
            message: "E-Sign is currently disabled.",
            hint: "Set FASTIO_ENABLE_ESIGN=1 to use sign commands (signing must also be enabled for your organization).",
        };
        let err = anyhow::Error::from(CliError::FeatureDisabled {
            message: "E-Sign is currently disabled.",
            hint: "Set FASTIO_ENABLE_ESIGN=1 to use sign commands (signing must also be enabled for your organization).",
        });
        let (headline, hint) = cli_error_render(&err, &cli_err);
        // The headline is the message verbatim — NO "Configuration error:" /
        // "Authentication error:" prefix (those come from other variants).
        assert_eq!(headline, "E-Sign is currently disabled.");
        assert!(
            !headline.contains("Configuration error:")
                && !headline.contains("Authentication error:"),
            "FeatureDisabled headline must carry no variant prefix: {headline}"
        );
        // The hint line is the re-enable guidance.
        let hint = hint.unwrap_or_default();
        assert!(
            hint.contains("FASTIO_ENABLE_ESIGN=1"),
            "hint must steer to the enable flag: {hint}"
        );
    }

    /// D1: the E-Sign gate predicate blocks a `sign` command iff E-Sign is
    /// disabled, and never blocks a non-sign command — exercised without
    /// mutating the process environment by passing the flag in and parsing the
    /// command via clap (sign parses even though it is hidden).
    #[test]
    fn import_gate_blocks_only_import_when_disabled() {
        use super::import_gate_blocks;
        use crate::cli::Cli;
        use clap::Parser;

        let import =
            Cli::try_parse_from(["fastio", "import", "list-providers", "--workspace", "1"])
                .expect("import command parses even when gated")
                .command;
        assert!(
            import_gate_blocks(&import, false),
            "import must be blocked when cloud import is disabled"
        );
        assert!(
            !import_gate_blocks(&import, true),
            "import must pass when cloud import is enabled"
        );

        // A non-import command is never blocked, regardless of the flag.
        let org = Cli::try_parse_from(["fastio", "org", "list"])
            .expect("org command parses")
            .command;
        assert!(!import_gate_blocks(&org, false));
        assert!(!import_gate_blocks(&org, true));

        // The two kill-switches are independent: a `sign` command must not be
        // blocked by the import gate, nor `import` by the E-Sign gate.
        let sign = Cli::try_parse_from(["fastio", "sign", "envelope", "list", "--workspace", "1"])
            .expect("sign command parses")
            .command;
        assert!(
            !import_gate_blocks(&sign, false),
            "the import gate must not block sign"
        );
        assert!(
            !super::sign_gate_blocks(&import, false),
            "the E-Sign gate must not block import"
        );
    }

    #[test]
    fn cloud_import_enabled_only_for_exactly_one() {
        use crate::commands::import::cloud_import_enabled_from;
        assert!(cloud_import_enabled_from(Some("1")));
        // Anything else is disabled — "true"/"yes"/empty must NOT enable a
        // not-yet-launched surface by accident.
        for v in ["0", "true", "TRUE", "yes", "", "01", " 1"] {
            assert!(
                !cloud_import_enabled_from(Some(v)),
                "value {v:?} must not enable"
            );
        }
        assert!(!cloud_import_enabled_from(None));
    }

    #[test]
    fn sign_gate_blocks_only_sign_when_disabled() {
        use super::sign_gate_blocks;
        use crate::cli::Cli;
        use clap::Parser;

        let sign = Cli::try_parse_from(["fastio", "sign", "envelope", "list", "--workspace", "1"])
            .expect("sign command parses even though hidden")
            .command;
        // Disabled → the sign command is blocked; enabled → it passes.
        assert!(
            sign_gate_blocks(&sign, false),
            "sign must be blocked when E-Sign is disabled"
        );
        assert!(
            !sign_gate_blocks(&sign, true),
            "sign must pass when E-Sign is enabled"
        );

        // A non-sign command is never blocked, regardless of the flag.
        let non_sign = Cli::try_parse_from(["fastio", "org", "list"])
            .expect("org list parses")
            .command;
        assert!(
            !sign_gate_blocks(&non_sign, false),
            "non-sign command must never be blocked (E-Sign disabled)"
        );
        assert!(
            !sign_gate_blocks(&non_sign, true),
            "non-sign command must never be blocked (E-Sign enabled)"
        );
    }

    /// MEDIUM B regression: the dedup is TYPE-AWARE — it collapses ONLY the
    /// `#[from]` forward-to-source adjacency on a `CliError::Api`, never an
    /// arbitrary same-text context/cause pair. A chain of two distinct,
    /// same-text links NOT involving `CliError::Api` (anyhow `.context("x")`
    /// over a plain error whose `Display` is `"x"`) must render BOTH links:
    /// `"x: x"`.
    #[test]
    fn render_chain_dedup_preserves_non_api_same_text_links() {
        use super::render_chain_dedup;
        use std::fmt;

        // A plain (non-CliError) error whose Display is exactly "x".
        #[derive(Debug)]
        struct PlainX;
        impl fmt::Display for PlainX {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("x")
            }
        }
        impl std::error::Error for PlainX {}

        // `.context("x")` over PlainX → chain is ["x" (context), "x" (PlainX)],
        // two distinct links that happen to render identically. Neither is a
        // `CliError::Api`, so BOTH must survive.
        let err = anyhow::Error::from(PlainX).context("x");
        assert_eq!(
            render_chain_dedup(&err),
            "x: x",
            "a same-text context/cause pair not involving CliError::Api must render in full"
        );
    }
}
