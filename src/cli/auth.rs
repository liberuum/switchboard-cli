use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use dialoguer::Input;

use crate::cli::helpers;
use crate::config::{load_config, save_config};
use crate::output::{OutputFormat, print_json};

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Authenticate with a bearer token
    Login {
        /// JWT token (omit to enter interactively)
        #[arg(long)]
        token: Option<String>,
    },
    /// Remove authentication token from current profile
    Logout,
    /// Show authentication status
    Status,
    /// Print the current bearer token
    Token,
}

pub async fn run(cmd: AuthCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        AuthCommand::Login { token } => login(token, format, profile_name).await,
        AuthCommand::Logout => logout(profile_name).await,
        AuthCommand::Status => status(format, profile_name).await,
        AuthCommand::Token => print_token(profile_name).await,
    }
}

async fn login(token: Option<String>, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let mut config = load_config()?;
    let (name, _profile) = helpers::resolve_profile(&config, profile_name)?;

    // Get token
    let token = match token {
        Some(t) => t,
        None => {
            // Check env var first
            if let Ok(env_token) = std::env::var("SWITCHBOARD_TOKEN") {
                println!("Using token from SWITCHBOARD_TOKEN environment variable");
                env_token
            } else {
                Input::new()
                    .with_prompt("Paste your bearer token (JWT)")
                    .interact_text()?
            }
        }
    };

    if token.is_empty() {
        bail!("Token cannot be empty");
    }

    // Validate token by making a test request
    let profile = config.profiles.get(&name).unwrap().clone();
    let client = crate::graphql::GraphQLClient::new(profile.url.clone(), Some(token.clone()));

    match client.query("{ drives }", None).await {
        Ok(_) => {
            println!("{} Token validated — connection successful", "✓".green());
        }
        Err(e) => {
            println!("{} Warning: connection test failed: {e}", "⚠".yellow());
            println!("  Token will be saved anyway. The server may require specific permissions.");
        }
    }

    // Save token to profile
    if let Some(p) = config.profiles.get_mut(&name) {
        p.token = Some(token);
    }
    save_config(&config)?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::json!({ "profile": name, "authenticated": true }));
        }
        OutputFormat::Table => {
            println!("{} Token saved to profile '{name}'", "✓".green());
        }
    }

    Ok(())
}

async fn logout(profile_name: Option<&str>) -> Result<()> {
    let mut config = load_config()?;
    let (name, _profile) = helpers::resolve_profile(&config, profile_name)?;

    if let Some(p) = config.profiles.get_mut(&name) {
        if p.token.is_none() {
            println!("Profile '{name}' has no token configured.");
            return Ok(());
        }
        p.token = None;
    }
    save_config(&config)?;

    println!("{} Token removed from profile '{name}'", "✓".green());
    Ok(())
}

async fn status(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let (name, profile) = helpers::resolve_profile(&config, profile_name)?;

    let has_token = profile.token.is_some();
    let has_env = std::env::var("SWITCHBOARD_TOKEN").is_ok();

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::json!({
                "profile": name,
                "url": profile.url,
                "has_token": has_token,
                "env_override": has_env,
            }));
        }
        OutputFormat::Table => {
            println!("Profile:  {}", name.green());
            println!("URL:      {}", profile.url);
            println!(
                "Auth:     {}",
                if has_env {
                    "SWITCHBOARD_TOKEN env var (overrides profile)".to_string()
                } else if has_token {
                    "Bearer token configured".to_string()
                } else {
                    "none".to_string()
                }
            );
        }
    }

    Ok(())
}

async fn print_token(profile_name: Option<&str>) -> Result<()> {
    // Check env var first (highest priority)
    if let Ok(token) = std::env::var("SWITCHBOARD_TOKEN") {
        println!("{token}");
        return Ok(());
    }

    let config = load_config()?;
    let (name, profile) = helpers::resolve_profile(&config, profile_name)?;

    match profile.token {
        Some(ref token) => println!("{token}"),
        None => bail!("No token configured for profile '{name}'"),
    }

    Ok(())
}
