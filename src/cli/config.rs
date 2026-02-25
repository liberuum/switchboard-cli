use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;

use crate::config::{load_config, save_config};
use crate::output::{OutputFormat, print_table, print_json};

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List all profiles
    List,
    /// Show the active profile details
    Show,
    /// Switch the active profile
    Use {
        /// Profile name to switch to
        name: String,
    },
    /// Remove a profile
    Remove {
        /// Profile name to remove
        name: String,
    },
}

pub async fn run(cmd: ConfigCommand, format: OutputFormat) -> Result<()> {
    match cmd {
        ConfigCommand::List => list(format),
        ConfigCommand::Show => show(format),
        ConfigCommand::Use { name } => use_profile(&name),
        ConfigCommand::Remove { name } => remove(&name),
    }
}

fn list(format: OutputFormat) -> Result<()> {
    let config = load_config()?;

    if config.profiles.is_empty() {
        println!("No profiles configured. Run `switchboard init` to get started.");
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            let profiles: Vec<_> = config
                .profiles
                .iter()
                .map(|(name, p)| {
                    serde_json::json!({
                        "name": name,
                        "url": p.url,
                        "default": p.default,
                        "has_token": p.token.is_some(),
                    })
                })
                .collect();
            print_json(&serde_json::Value::Array(profiles));
        }
        _ => {
            let rows: Vec<Vec<String>> = config
                .profiles
                .iter()
                .map(|(name, p)| {
                    vec![
                        if p.default {
                            format!("{} {}", "→".green(), name)
                        } else {
                            format!("  {name}")
                        },
                        p.url.clone(),
                        if p.token.is_some() {
                            "yes".to_string()
                        } else {
                            "no".to_string()
                        },
                    ]
                })
                .collect();
            print_table(&["Profile", "URL", "Auth"], &rows);
        }
    }

    Ok(())
}

fn show(format: OutputFormat) -> Result<()> {
    let config = load_config()?;
    let (name, profile) = config
        .default_profile()
        .ok_or_else(|| anyhow::anyhow!("No default profile set"))?;

    match format {
        OutputFormat::Json => {
            print_json(&serde_json::json!({
                "name": name,
                "url": profile.url,
                "default": profile.default,
                "has_token": profile.token.is_some(),
            }));
        }
        _ => {
            println!("Active profile: {}", name.green().bold());
            println!("URL: {}", profile.url);
            println!(
                "Auth: {}",
                if profile.token.is_some() {
                    "configured"
                } else {
                    "none"
                }
            );
        }
    }

    Ok(())
}

fn use_profile(name: &str) -> Result<()> {
    let mut config = load_config()?;
    if !config.set_default(name) {
        bail!("Profile '{name}' not found.");
    }
    save_config(&config)?;
    println!("{} Switched to profile \"{}\"", "✓".green(), name);
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let mut config = load_config()?;
    if !config.remove_profile(name) {
        bail!("Profile '{name}' not found.");
    }
    save_config(&config)?;
    println!("{} Profile \"{}\" removed.", "✓".green(), name);
    Ok(())
}
