use anyhow::Result;
use colored::Colorize;
use dialoguer::{Input, Confirm};

use crate::config::{Profile, load_config, save_config};
use crate::graphql::GraphQLClient;
use crate::graphql::introspection::{run_introspection, save_cache};

pub async fn run() -> Result<()> {
    let mut config = load_config()?;

    // Prompt for URL
    let url: String = Input::new()
        .with_prompt("Paste your Switchboard GraphQL URL")
        .interact_text()?;

    // Normalize URL: ensure it ends with /graphql
    let url = normalize_url(&url);

    // Prompt for profile name
    let default_name = profile_name_from_url(&url);
    let name: String = Input::new()
        .with_prompt("Profile name")
        .default(default_name)
        .interact_text()?;

    // Check if profile already exists
    if config.get_profile(&name).is_some() {
        let overwrite = Confirm::new()
            .with_prompt(format!("Profile '{name}' already exists. Overwrite?"))
            .default(false)
            .interact()?;
        if !overwrite {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Optional auth token
    let token: String = Input::new()
        .with_prompt("Auth token (optional, press Enter to skip)")
        .default(String::new())
        .interact_text()?;
    let token = if token.is_empty() { None } else { Some(token) };

    // Test connection
    println!("Connecting to {url}...");
    let client = GraphQLClient::new(url.clone(), token.clone());
    let data = client.query("{ drives }", None).await;

    match data {
        Ok(d) => {
            let drive_count = d
                .get("drives")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!("{} Connected. {drive_count} drives found.", "✓".green());
        }
        Err(e) => {
            eprintln!("{} Connection failed: {e}", "✗".red());
            let proceed = Confirm::new()
                .with_prompt("Save profile anyway?")
                .default(false)
                .interact()?;
            if !proceed {
                return Ok(());
            }
        }
    }

    // Run introspection
    println!("Introspecting schema...");
    match run_introspection(&client).await {
        Ok(cache) => {
            let model_count = cache.models.len();
            save_cache(&name, &cache)?;
            println!(
                "{} {model_count} document models discovered.",
                "✓".green()
            );
        }
        Err(e) => {
            eprintln!(
                "{} Introspection failed: {e}. You can retry with `switchboard introspect`.",
                "⚠".yellow()
            );
        }
    }

    // Save profile
    let profile = Profile {
        url,
        token,
        default: config.profiles.is_empty(),
    };
    config.add_profile(name.clone(), profile);
    save_config(&config)?;

    println!(
        "{} Profile \"{}\" saved{}.",
        "✓".green(),
        name,
        if config.get_profile(&name).map(|p| p.default).unwrap_or(false) {
            " as default"
        } else {
            ""
        }
    );

    Ok(())
}

fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if !url.ends_with("/graphql") {
        format!("{url}/graphql")
    } else {
        url.to_string()
    }
}

fn profile_name_from_url(url: &str) -> String {
    // Extract hostname and derive a profile name
    url.replace("https://", "")
        .replace("http://", "")
        .split('/')
        .next()
        .unwrap_or("default")
        .split('.')
        .next()
        .unwrap_or("default")
        .replace("switchboard-", "")
        .replace("localhost", "local")
        .to_string()
}
