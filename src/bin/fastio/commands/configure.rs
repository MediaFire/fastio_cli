/// `fastio configure` command implementation.
///
/// Manages CLI profiles: create, list, set default, and show configuration.
use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::json;

use crate::cli::ConfigureCommands;
use fastio_cli::config::{Config, DEFAULT_API_BASE, Profile};
use fastio_cli::output::{OutputConfig, OutputFormat};

/// Execute a configure subcommand.
pub fn execute(cmd: &ConfigureCommands, output: &OutputConfig) -> Result<()> {
    match cmd {
        ConfigureCommands::Init {
            name,
            api_base,
            auth_method,
        } => init_profile(name, api_base.as_deref(), auth_method.as_deref(), output),
        ConfigureCommands::List => list_profiles(output),
        ConfigureCommands::SetDefault { name } => set_default(name, output),
        ConfigureCommands::Show => show_config(output),
        ConfigureCommands::Delete { name } => delete_profile(name, output),
    }
}

/// Create or update a profile.
fn init_profile(
    name: &str,
    api_base: Option<&str>,
    auth_method: Option<&str>,
    output: &OutputConfig,
) -> Result<()> {
    let mut config = Config::load().context("failed to load configuration")?;

    let base = api_base.unwrap_or(DEFAULT_API_BASE);
    let method = auth_method.unwrap_or("pkce");

    let is_update = config.profiles.contains_key(name);

    config.profiles.insert(
        name.to_owned(),
        Profile {
            api_base: base.to_owned(),
            auth_method: method.to_owned(),
        },
    );

    config.save().context("failed to save configuration")?;

    if output.quiet {
        return Ok(());
    }

    if output.format == OutputFormat::Json {
        let val = json!({
            "action": if is_update { "updated" } else { "created" },
            "profile": name,
            "api_base": base,
            "auth_method": method,
        });
        output.render(&val)?;
        return Ok(());
    }

    let action = if is_update { "Updated" } else { "Created" };
    eprintln!("{} profile '{}'", action.green().bold(), name.bold());
    eprintln!("  API base:    {base}");
    eprintln!("  Auth method: {method}");

    Ok(())
}

/// List all configured profiles.
fn list_profiles(output: &OutputConfig) -> Result<()> {
    let config = Config::load().context("failed to load configuration")?;

    if output.format == OutputFormat::Json {
        let profiles: Vec<serde_json::Value> = config
            .profiles
            .iter()
            .map(|(name, profile)| {
                json!({
                    "name": name,
                    "api_base": profile.api_base,
                    "auth_method": profile.auth_method,
                    "default": name == &config.default_profile,
                })
            })
            .collect();
        output.render(&json!(profiles))?;
        return Ok(());
    }

    if output.quiet {
        return Ok(());
    }

    if config.profiles.is_empty() {
        eprintln!("No profiles configured. Run `fastio configure init` to create one.");
        return Ok(());
    }

    eprintln!("{}", "Configured profiles:".bold());
    for (name, profile) in &config.profiles {
        let default_marker = if name == &config.default_profile {
            " (default)".green().to_string()
        } else {
            String::new()
        };
        eprintln!("  {}{default_marker}", name.bold());
        eprintln!("    API base:    {}", profile.api_base);
        eprintln!("    Auth method: {}", profile.auth_method);
    }

    Ok(())
}

/// Set the default profile.
fn set_default(name: &str, output: &OutputConfig) -> Result<()> {
    let mut config = Config::load().context("failed to load configuration")?;

    if !config.profiles.contains_key(name) {
        anyhow::bail!(
            "Profile '{name}' does not exist. Run `fastio configure list` to see available profiles."
        );
    }

    name.clone_into(&mut config.default_profile);
    config.save().context("failed to save configuration")?;

    if output.quiet {
        return Ok(());
    }

    if output.format == OutputFormat::Json {
        output.render(&json!({"default_profile": name}))?;
        return Ok(());
    }

    eprintln!(
        "{} Default profile set to '{}'",
        "OK".green().bold(),
        name.bold()
    );

    Ok(())
}

/// Show the current configuration.
fn show_config(output: &OutputConfig) -> Result<()> {
    let config = Config::load().context("failed to load configuration")?;

    if output.format == OutputFormat::Json {
        let val = serde_json::to_value(&config).context("failed to serialize configuration")?;
        output.render(&val)?;
        return Ok(());
    }

    if output.quiet {
        return Ok(());
    }

    let path = config.path();
    eprintln!("{} {}", "Config file:".bold(), path.display());
    eprintln!("{} {}", "Default profile:".bold(), config.default_profile);
    eprintln!();

    for (name, profile) in &config.profiles {
        let default_marker = if name == &config.default_profile {
            " (default)".green().to_string()
        } else {
            String::new()
        };
        eprintln!("  {}{default_marker}", name.bold());
        eprintln!("    API base:    {}", profile.api_base);
        eprintln!("    Auth method: {}", profile.auth_method);
    }

    Ok(())
}

/// Delete a named profile.
fn delete_profile(name: &str, output: &OutputConfig) -> Result<()> {
    let mut config = Config::load().context("failed to load configuration")?;
    config
        .delete_profile(name)
        .context("failed to delete profile")?;

    if output.quiet {
        return Ok(());
    }

    if output.format == OutputFormat::Json {
        output.render(&json!({"deleted": name}))?;
        return Ok(());
    }

    eprintln!("{} Deleted profile '{}'", "OK".green().bold(), name.bold());

    Ok(())
}
