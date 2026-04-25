//! Binary crate for the `fastio` CLI tool and MCP server.
//!
//! Detects whether to run in interactive CLI mode or as a JSON-RPC MCP server,
//! then dispatches accordingly.
mod cli;
mod commands;
mod mcp;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

use cli::{AuthCommands, Cli, Commands};
use commands::ai::AiCommand;
use commands::approval::ApprovalCommand;
use commands::apps::AppsCommand;
use commands::asset::AssetCommand;
use commands::auth::{ApiKeyCommand, AuthCommand, OauthCommand, TwoFaCommand};
use commands::comment::CommentCommand;
use commands::download::DownloadCommand;
use commands::event::EventCommand;
use commands::files::{FileLockCommand, FilesCommand};
use commands::import::ImportCommand;
use commands::invitation::InvitationCommand;
use commands::lock::LockCommand;
use commands::member::MemberCommand;
use commands::metadata::MetadataCommand;
use commands::org::{
    BillingCommand, DiscoverCommand, OrgAssetCommand, OrgCommand, OrgInvitationsCommand,
    OrgMembersCommand, OrgTransferTokenCommand,
};
use commands::preview::PreviewCommand;
use commands::share::{ShareCommand, ShareFilesCommand, ShareMembersCommand};
use commands::system::SystemCommand;
use commands::task::{TaskCommand, TaskListCommand};
use commands::todo::TodoCommand;
use commands::upload::UploadCommand;
use commands::user::{
    AvatarCommand, SettingsCommand, UserAssetCommand, UserCommand, UserInvitationsCommand,
};
use commands::worklog::WorklogCommand;
use commands::workspace::WorkspaceCommand;
use fastio_cli::config::Config;
use fastio_cli::output::OutputConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let result = run().await;
    if let Err(ref err) = result {
        // If the underlying error is a CliError, use rich display.
        if let Some(cli_err) = err.downcast_ref::<fastio_cli::error::CliError>() {
            cli_err.render_stderr();
            std::process::exit(1);
        }
    }
    result
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
        return mcp::serve(tools_filter).await;
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

    // Load configuration
    let config = Config::load().context("failed to load configuration")?;

    // Determine active profile name
    let profile_name = cli.profile.as_deref().unwrap_or(&config.default_profile);

    // Resolve API base URL
    let api_base = config.api_base(cli.api_base.as_deref(), Some(profile_name));

    // Build output configuration
    let output = OutputConfig::from_flags(
        cli.format.as_deref(),
        cli.fields.as_deref(),
        cli.no_color,
        cli.quiet,
    );

    let ctx = commands::CommandContext {
        output: &output,
        profile_name,
        api_base: &api_base,
        flag_token: cli.token.as_deref(),
        config_dir: &config.config_dir,
    };

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
        Commands::Preview(c) => commands::preview::execute(&map_preview_command(c), ctx).await,
        Commands::Asset(c) => commands::asset::execute(&map_asset_command(c), ctx).await,
        Commands::Ai(c) => commands::ai::execute(&map_ai_command(c), ctx).await,
        Commands::Task(c) => commands::task::execute(&map_task_command(c), ctx).await,
        Commands::Worklog(c) => commands::worklog::execute(&map_worklog_command(c), ctx).await,
        Commands::Approval(c) => commands::approval::execute(&map_approval_command(c), ctx).await,
        Commands::Todo(c) => commands::todo::execute(&map_todo_command(c), ctx).await,
        Commands::Apps(c) => commands::apps::execute(&map_apps_command(&c), ctx).await,
        Commands::Import(c) => commands::import::execute(&map_import_command(&c), ctx).await,
        Commands::Lock(c) => commands::lock::execute(&map_lock_command(&c), ctx).await,
        Commands::Metadata(c) => commands::metadata::execute(&map_metadata_command(c), ctx).await,
        Commands::System(c) => commands::system::execute(&map_system_command(&c), ctx).await,
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
            unreachable!("MCP mode is handled before this match")
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
        AuthCommands::Login { email, password } => AuthCommand::Login { email, password },
        AuthCommands::Logout => AuthCommand::Logout,
        AuthCommands::Status => AuthCommand::Status,
        AuthCommands::Signup {
            email,
            password,
            first_name,
            last_name,
        } => AuthCommand::Signup {
            email,
            password,
            first_name,
            last_name,
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
            cli::ApiKeyCommands::Create { name, scopes } => ApiKeyCommand::Create { name, scopes },
            cli::ApiKeyCommands::List => ApiKeyCommand::List,
            cli::ApiKeyCommands::Delete { key_id } => ApiKeyCommand::Delete { key_id },
            cli::ApiKeyCommands::Get { key_id } => ApiKeyCommand::Get { key_id },
            cli::ApiKeyCommands::Update {
                key_id,
                name,
                scopes,
            } => ApiKeyCommand::Update {
                key_id,
                name,
                scopes,
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
            cli::OauthCommands::Revoke { session_id } => OauthCommand::Revoke { session_id },
            cli::OauthCommands::RevokeAll => OauthCommand::RevokeAll,
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
        } => UserCommand::Update {
            first_name,
            last_name,
            display_name,
        },
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
        cli::UserCommands::Close { confirmation } => UserCommand::Close { confirmation },
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
        } => OrgCommand::Update {
            org_id,
            name,
            domain,
            description,
            industry,
            billing_email,
            homepage_url,
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
        } => OrgCommand::Workspaces {
            org_id,
            limit,
            offset,
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
        } => OrgCommand::CreateWorkspace {
            org_id,
            name,
            folder_name,
            description,
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
        cli::OrgBillingCommands::Info { org_id } => BillingCommand::Info { org_id },
        cli::OrgBillingCommands::Plans => BillingCommand::Plans,
        cli::OrgBillingCommands::Meters {
            org_id,
            meter,
            start_time,
            end_time,
        } => BillingCommand::Meters {
            org_id,
            meter,
            start_time,
            end_time,
        },
        cli::OrgBillingCommands::Cancel { org_id } => BillingCommand::Cancel { org_id },
        cli::OrgBillingCommands::Activate { org_id } => BillingCommand::Activate { org_id },
        cli::OrgBillingCommands::Reset { org_id } => BillingCommand::Reset { org_id },
        cli::OrgBillingCommands::Members {
            org_id,
            limit,
            offset,
        } => BillingCommand::Members {
            org_id,
            limit,
            offset,
        },
        cli::OrgBillingCommands::Create { org_id, plan_id } => {
            BillingCommand::Create { org_id, plan_id }
        }
        cli::OrgBillingCommands::Invoices {
            org_id,
            limit,
            offset,
        } => BillingCommand::Invoices {
            org_id,
            limit,
            offset,
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
        cli::WorkspaceCommands::List { org, limit, offset } => WorkspaceCommand::List {
            org_id: org,
            limit,
            offset,
        },
        cli::WorkspaceCommands::Create {
            name,
            org,
            folder_name,
            description,
            intelligence,
        } => WorkspaceCommand::Create {
            org_id: org,
            name,
            folder_name,
            description,
            intelligence,
        },
        cli::WorkspaceCommands::Info { workspace_id } => WorkspaceCommand::Info { workspace_id },
        cli::WorkspaceCommands::Update {
            workspace_id,
            name,
            description,
            folder_name,
        } => WorkspaceCommand::Update {
            workspace_id,
            name,
            description,
            folder_name,
        },
        cli::WorkspaceCommands::Delete {
            workspace_id,
            confirm,
        } => WorkspaceCommand::Delete {
            workspace_id,
            confirm,
        },
        cli::WorkspaceCommands::EnableWorkflow { workspace_id } => {
            WorkspaceCommand::EnableWorkflow { workspace_id }
        }
        cli::WorkspaceCommands::DisableWorkflow { workspace_id } => {
            WorkspaceCommand::DisableWorkflow { workspace_id }
        }
        cli::WorkspaceCommands::JobsStatus { workspace_id } => {
            WorkspaceCommand::JobsStatus { workspace_id }
        }
        cli::WorkspaceCommands::Search {
            workspace_id,
            query,
            limit,
            offset,
        } => WorkspaceCommand::Search {
            workspace_id,
            query,
            limit,
            offset,
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
        cli::InvitationCommands::Accept { invitation_id } => {
            InvitationCommand::Accept { invitation_id }
        }
        cli::InvitationCommands::Decline {
            invitation_id,
            entity_type,
            entity_id,
        } => InvitationCommand::Decline {
            invitation_id,
            entity_type,
            entity_id,
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
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => FilesCommand::List {
            workspace,
            folder,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        },
        cli::FilesCommands::Info { workspace, node_id } => {
            FilesCommand::Info { workspace, node_id }
        }
        cli::FilesCommands::CreateFolder {
            workspace,
            name,
            parent,
        } => FilesCommand::CreateFolder {
            workspace,
            name,
            parent,
        },
        cli::FilesCommands::Move {
            workspace,
            node_id,
            to,
        } => FilesCommand::Move {
            workspace,
            node_id,
            to,
        },
        cli::FilesCommands::Copy {
            workspace,
            node_id,
            to,
        } => FilesCommand::Copy {
            workspace,
            node_id,
            to,
        },
        cli::FilesCommands::Rename {
            workspace,
            node_id,
            new_name,
        } => FilesCommand::Rename {
            workspace,
            node_id,
            new_name,
        },
        cli::FilesCommands::Delete { workspace, node_id } => {
            FilesCommand::Delete { workspace, node_id }
        }
        cli::FilesCommands::Restore { workspace, node_id } => {
            FilesCommand::Restore { workspace, node_id }
        }
        cli::FilesCommands::Purge { workspace, node_id } => {
            FilesCommand::Purge { workspace, node_id }
        }
        cli::FilesCommands::Trash {
            workspace,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        } => FilesCommand::Trash {
            workspace,
            sort_by,
            sort_dir,
            page_size,
            cursor,
        },
        cli::FilesCommands::Versions { workspace, node_id } => {
            FilesCommand::Versions { workspace, node_id }
        }
        cli::FilesCommands::Search {
            workspace,
            query,
            page_size,
            cursor,
        } => FilesCommand::Search {
            workspace,
            query,
            page_size,
            cursor,
        },
        cli::FilesCommands::Recent {
            workspace,
            page_size,
            cursor,
        } => FilesCommand::Recent {
            workspace,
            page_size,
            cursor,
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
            node_id,
            version_id,
        } => FilesCommand::VersionRestore {
            workspace,
            node_id,
            version_id,
        },
        cli::FilesCommands::Lock(lk) => FilesCommand::Lock(map_file_lock_command(lk)),
        cli::FilesCommands::Read { workspace, node_id } => {
            FilesCommand::Read { workspace, node_id }
        }
        cli::FilesCommands::Quickshare { workspace, node_id } => {
            FilesCommand::Quickshare { workspace, node_id }
        }
    }
}

/// Convert clap-parsed file lock subcommands to the internal enum.
fn map_file_lock_command(lk: cli::FileLockCommands) -> FileLockCommand {
    match lk {
        cli::FileLockCommands::Acquire { workspace, node_id } => {
            FileLockCommand::Acquire { workspace, node_id }
        }
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
            name,
            content,
            folder,
        } => UploadCommand::Text {
            workspace,
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
            filename,
            filesize,
            folder,
        } => UploadCommand::CreateSession {
            workspace,
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
        cli::UploadCommands::WebList => UploadCommand::WebList,
        cli::UploadCommands::WebCancel { upload_id } => UploadCommand::WebCancel { upload_id },
        cli::UploadCommands::WebStatus { upload_id } => UploadCommand::WebStatus { upload_id },
        cli::UploadCommands::Limits => UploadCommand::Limits,
        cli::UploadCommands::Extensions => UploadCommand::Extensions,
        cli::UploadCommands::Stream {
            workspace,
            file_path,
            folder,
            max_size,
            name,
            hash,
            hash_algo,
        } => UploadCommand::Stream {
            workspace,
            file_path,
            folder,
            max_size,
            name,
            hash,
            hash_algo,
        },
        cli::UploadCommands::CreateStreamSession {
            workspace,
            filename,
            folder,
            max_size,
        } => UploadCommand::CreateStreamSession {
            workspace,
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
            node_id,
            output,
        } => DownloadCommand::File {
            workspace,
            node_id,
            output_path: output,
        },
        cli::DownloadCommands::Folder {
            workspace,
            node_id,
            output,
        } => DownloadCommand::Folder {
            workspace,
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
fn map_share_command(cmd: cli::ShareCommands) -> ShareCommand {
    match cmd {
        cli::ShareCommands::List { limit, offset } => ShareCommand::List { limit, offset },
        cli::ShareCommands::Create {
            name,
            workspace,
            description,
            access_options,
            password,
            anonymous_uploads,
            intelligence,
            download_security,
        } => ShareCommand::Create {
            workspace_id: workspace,
            name,
            description,
            access_options,
            password,
            anonymous_uploads,
            intelligence,
            download_security,
        },
        cli::ShareCommands::Info { share_id } => ShareCommand::Info { share_id },
        cli::ShareCommands::Update {
            share_id,
            name,
            description,
            access_options,
            download_enabled,
            comments_enabled,
            anonymous_uploads,
            download_security,
        } => ShareCommand::Update {
            share_id,
            name,
            description,
            access_options,
            download_enabled,
            comments_enabled,
            anonymous_uploads,
            download_security,
        },
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
        cli::ShareCommands::WorkflowEnable { share_id } => {
            ShareCommand::WorkflowEnable { share_id }
        }
        cli::ShareCommands::WorkflowDisable { share_id } => {
            ShareCommand::WorkflowDisable { share_id }
        }
        cli::ShareCommands::Files(f) => ShareCommand::Files(map_share_files_command(f)),
        cli::ShareCommands::Members(m) => ShareCommand::Members(map_share_members_command(m)),
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
        } => ShareMembersCommand::Add {
            share_id,
            email,
            role,
        },
        cli::ShareMembersCommands::Remove {
            share_id,
            member_id,
        } => ShareMembersCommand::Remove {
            share_id,
            member_id,
        },
    }
}

/// Convert clap-parsed comment commands to the internal enum.
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
        } => CommentCommand::Create {
            entity_type,
            entity_id,
            node_id,
            text,
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
    }
}

/// Convert clap-parsed event commands to the internal enum.
fn map_event_command(cmd: cli::EventCommands) -> EventCommand {
    match cmd {
        cli::EventCommands::List {
            workspace,
            share,
            event,
            category,
            limit,
            offset,
        } => EventCommand::List {
            workspace,
            share,
            event,
            category,
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
            event,
            category,
            subcategory,
            user_context,
            limit,
            offset,
        } => EventCommand::Summarize {
            workspace,
            share,
            event,
            category,
            subcategory,
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
        } => PreviewCommand::Transform {
            context_type,
            context_id,
            node_id,
            transform_name,
            width,
            height,
            output_format,
            size,
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

/// Convert clap-parsed AI commands to the internal enum.
fn map_ai_command(cmd: cli::AiCommands) -> AiCommand {
    match cmd {
        cli::AiCommands::Chat {
            workspace,
            message,
            chat_id,
            node_ids,
            folder_id,
            intelligence,
        } => AiCommand::Chat {
            workspace,
            message,
            chat_id,
            node_ids,
            folder_id,
            intelligence,
        },
        cli::AiCommands::Search {
            workspace,
            query,
            limit,
            offset,
        } => AiCommand::Search {
            workspace,
            query,
            limit,
            offset,
        },
        cli::AiCommands::History {
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
        cli::AiCommands::Summary {
            workspace,
            node_ids,
        } => AiCommand::Summary {
            workspace,
            node_ids,
        },
    }
}

/// Convert clap-parsed task commands to the internal enum.
fn map_task_command(cmd: cli::TaskCommands) -> TaskCommand {
    match cmd {
        cli::TaskCommands::List {
            workspace,
            list_id,
            limit,
            offset,
        } => TaskCommand::List {
            workspace,
            list_id,
            limit,
            offset,
        },
        cli::TaskCommands::Create {
            workspace,
            list_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        } => TaskCommand::Create {
            workspace,
            list_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        },
        cli::TaskCommands::Info { list_id, task_id } => TaskCommand::Info { list_id, task_id },
        cli::TaskCommands::Update {
            list_id,
            task_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        } => TaskCommand::Update {
            list_id,
            task_id,
            title,
            description,
            status,
            priority,
            assignee_id,
        },
        cli::TaskCommands::Delete { list_id, task_id } => TaskCommand::Delete { list_id, task_id },
        cli::TaskCommands::Assign {
            list_id,
            task_id,
            assignee_id,
        } => TaskCommand::Assign {
            list_id,
            task_id,
            assignee_id,
        },
        cli::TaskCommands::Complete { list_id, task_id } => {
            TaskCommand::Complete { list_id, task_id }
        }
        cli::TaskCommands::Move {
            list_id,
            task_id,
            target_list_id,
            sort_order,
        } => TaskCommand::Move {
            list_id,
            task_id,
            target_list_id,
            sort_order,
        },
        cli::TaskCommands::BulkStatus {
            list_id,
            task_ids,
            status,
        } => TaskCommand::BulkStatus {
            list_id,
            task_ids,
            status,
        },
        cli::TaskCommands::Reorder { list_id, task_ids } => {
            TaskCommand::Reorder { list_id, task_ids }
        }
        cli::TaskCommands::ReorderLists {
            profile_type,
            profile_id,
            list_ids,
        } => TaskCommand::ReorderLists {
            profile_type,
            profile_id,
            list_ids,
        },
        cli::TaskCommands::Lists(lists_cmd) => TaskCommand::Lists(map_task_list_command(lists_cmd)),
    }
}

/// Convert clap-parsed task list commands to the internal enum.
fn map_task_list_command(cmd: cli::TaskListCommands) -> TaskListCommand {
    match cmd {
        cli::TaskListCommands::List {
            workspace,
            share,
            limit,
            offset,
        } => {
            let (profile_type, profile_id) = resolve_todo_profile(workspace, share);
            TaskListCommand::List {
                profile_type,
                profile_id,
                limit,
                offset,
            }
        }
        cli::TaskListCommands::Create {
            profile_type,
            profile_id,
            name,
            description,
        } => TaskListCommand::Create {
            profile_type,
            profile_id,
            name,
            description,
        },
        cli::TaskListCommands::Update {
            list_id,
            name,
            description,
        } => TaskListCommand::Update {
            list_id,
            name,
            description,
        },
        cli::TaskListCommands::Delete { list_id } => TaskListCommand::Delete { list_id },
    }
}

/// Convert clap-parsed worklog commands to the internal enum.
fn map_worklog_command(cmd: cli::WorklogCommands) -> WorklogCommand {
    match cmd {
        cli::WorklogCommands::List {
            workspace,
            entity_type,
            entity_id,
            limit,
            offset,
        } => WorklogCommand::List {
            workspace,
            entity_type,
            entity_id,
            limit,
            offset,
        },
        cli::WorklogCommands::Append {
            workspace,
            message,
            entity_type,
            entity_id,
        } => WorklogCommand::Append {
            workspace,
            message,
            entity_type,
            entity_id,
        },
        cli::WorklogCommands::Interject {
            workspace,
            message,
            entity_type,
            entity_id,
        } => WorklogCommand::Interject {
            workspace,
            message,
            entity_type,
            entity_id,
        },
        cli::WorklogCommands::Details { entry_id } => WorklogCommand::Details { entry_id },
        cli::WorklogCommands::ListInterjections {
            entity_type,
            entity_id,
            limit,
            offset,
        } => WorklogCommand::ListInterjections {
            entity_type,
            entity_id,
            limit,
            offset,
        },
        cli::WorklogCommands::Acknowledge { entry_id } => WorklogCommand::Acknowledge { entry_id },
    }
}

/// Convert clap-parsed approval commands to the internal enum.
fn map_approval_command(cmd: cli::ApprovalCommands) -> ApprovalCommand {
    match cmd {
        cli::ApprovalCommands::List {
            workspace,
            status,
            limit,
            offset,
        } => ApprovalCommand::List {
            workspace,
            status,
            limit,
            offset,
        },
        cli::ApprovalCommands::Request {
            workspace,
            entity_type,
            entity_id,
            description,
            approver_id,
        } => ApprovalCommand::Request {
            workspace,
            entity_type,
            entity_id,
            description,
            approver_id,
        },
        cli::ApprovalCommands::Approve {
            approval_id,
            comment,
        } => ApprovalCommand::Approve {
            approval_id,
            comment,
        },
        cli::ApprovalCommands::Reject {
            approval_id,
            comment,
        } => ApprovalCommand::Reject {
            approval_id,
            comment,
        },
    }
}

/// Convert clap-parsed todo commands to the internal enum.
fn map_todo_command(cmd: cli::TodoCommands) -> TodoCommand {
    match cmd {
        cli::TodoCommands::List {
            profile_type,
            profile_id,
            limit,
            offset,
        } => TodoCommand::List {
            profile_type,
            profile_id,
            limit,
            offset,
        },
        cli::TodoCommands::Create {
            workspace,
            share,
            title,
            assignee_id,
        } => {
            let (profile_type, profile_id) = resolve_todo_profile(workspace, share);
            TodoCommand::Create {
                profile_type,
                profile_id,
                title,
                assignee_id,
            }
        }
        cli::TodoCommands::Update {
            todo_id,
            title,
            done,
            assignee_id,
        } => TodoCommand::Update {
            todo_id,
            title,
            done,
            assignee_id,
        },
        cli::TodoCommands::Toggle { todo_id } => TodoCommand::Toggle { todo_id },
        cli::TodoCommands::Delete { todo_id } => TodoCommand::Delete { todo_id },
        cli::TodoCommands::BulkToggle {
            workspace,
            share,
            todo_ids,
            done,
        } => {
            let (profile_type, profile_id) = resolve_todo_profile(workspace, share);
            let ids: Vec<String> = todo_ids.split(',').map(|s| s.trim().to_owned()).collect();
            TodoCommand::BulkToggle {
                profile_type,
                profile_id,
                todo_ids: ids,
                done,
            }
        }
    }
}

/// Resolve workspace/share options into a `(profile_type, profile_id)` pair.
///
/// Clap ensures at least one of `workspace` or `share` is present via
/// `required_unless_present`. If both are `None` (should not happen), returns
/// an empty workspace profile ID which will fail server-side.
fn resolve_todo_profile(workspace: Option<String>, share: Option<String>) -> (String, String) {
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
        cli::AppsCommands::Details { app_id } => AppsCommand::Details {
            app_id: app_id.clone(),
        },
        cli::AppsCommands::Launch {
            app_id,
            context_type,
            context_id,
        } => AppsCommand::Launch {
            app_id: app_id.clone(),
            context_type: context_type.clone(),
            context_id: context_id.clone(),
        },
        cli::AppsCommands::GetToolApps { tool_name } => AppsCommand::GetToolApps {
            tool_name: tool_name.clone(),
        },
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
        } => ImportCommand::ProvisionIdentity {
            workspace_id: workspace.clone(),
            provider: provider.clone(),
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
        cli::ImportCommands::Discover {
            workspace,
            identity_id,
        } => ImportCommand::Discover {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
        },
        cli::ImportCommands::CreateSource {
            workspace,
            identity_id,
            remote_path,
            remote_name,
            sync_interval,
            access_mode,
        } => ImportCommand::CreateSource {
            workspace_id: workspace.clone(),
            identity_id: identity_id.clone(),
            remote_path: remote_path.clone(),
            remote_name: remote_name.clone(),
            sync_interval: *sync_interval,
            access_mode: access_mode.clone(),
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
fn map_lock_command(cmd: &cli::LockCommands) -> LockCommand {
    match cmd {
        cli::LockCommands::Acquire {
            context_type,
            context_id,
            node_id,
        } => LockCommand::Acquire {
            context_type: context_type.clone(),
            context_id: context_id.clone(),
            node_id: node_id.clone(),
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

/// Convert clap-parsed metadata commands to the internal enum.
#[allow(clippy::too_many_lines)]
fn map_metadata_command(cmd: cli::MetadataCommands) -> MetadataCommand {
    match cmd {
        cli::MetadataCommands::Eligible {
            workspace,
            limit,
            offset,
        } => MetadataCommand::Eligible {
            workspace,
            limit,
            offset,
        },
        cli::MetadataCommands::AddNodes {
            workspace,
            template_id,
            node_ids,
        } => MetadataCommand::AddNodes {
            workspace,
            template_id,
            node_ids,
        },
        cli::MetadataCommands::RemoveNodes {
            workspace,
            template_id,
            node_ids,
        } => MetadataCommand::RemoveNodes {
            workspace,
            template_id,
            node_ids,
        },
        cli::MetadataCommands::ListNodes {
            workspace,
            template_id,
            limit,
            offset,
            sort_field,
            sort_dir,
        } => MetadataCommand::ListNodes {
            workspace,
            template_id,
            limit,
            offset,
            sort_field,
            sort_dir,
        },
        cli::MetadataCommands::AutoMatch {
            workspace,
            template_id,
        } => MetadataCommand::AutoMatch {
            workspace,
            template_id,
        },
        cli::MetadataCommands::ExtractAll {
            workspace,
            template_id,
        } => MetadataCommand::ExtractAll {
            workspace,
            template_id,
        },
        cli::MetadataCommands::Extract {
            workspace,
            node_id,
            template_id,
            fields,
        } => MetadataCommand::Extract {
            workspace,
            node_id,
            template_id,
            fields,
        },
        cli::MetadataCommands::PreviewMatch {
            workspace,
            name,
            description,
        } => MetadataCommand::PreviewMatch {
            workspace,
            name,
            description,
        },
        cli::MetadataCommands::SuggestFields {
            workspace,
            node_ids,
            description,
            user_context,
        } => MetadataCommand::SuggestFields {
            workspace,
            node_ids,
            description,
            user_context,
        },
        cli::MetadataCommands::CreateTemplate {
            workspace,
            name,
            description,
            category,
            fields,
        } => MetadataCommand::CreateTemplate {
            workspace,
            name,
            description,
            category,
            fields,
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
