use anyhow::Result;
use colored::Colorize;
use dialoguer::{Confirm, Input};

use crate::config::{Profile, load_config, save_config};
use crate::graphql::GraphQLClient;
use crate::graphql::introspection::{run_introspection, save_cache};

pub async fn run() -> Result<()> {
    let mut config = load_config()?;

    // Prompt for URL
    let url: String = Input::new()
        .with_prompt("Paste your Switchboard GraphQL URL")
        .interact_text()?;

    // Strip bracketed-paste escape sequences and other control chars that
    // terminals inject when the user pastes a URL
    let url = strip_terminal_escapes(&url);

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
    let token = strip_terminal_escapes(&token);
    let token = if token.is_empty() { None } else { Some(token) };

    // Test connection
    println!("Connecting to {url}...");
    let mut token = token;
    let mut client = GraphQLClient::new(url.clone(), token.clone());
    let test_query = r#"{ findDocuments(search: { type: "powerhouse/document-drive" }, paging: { limit: 1 }) { totalCount } }"#;
    let data = client.query(test_query, None).await;

    match data {
        Ok(d) => {
            let count = d
                .pointer("/findDocuments/totalCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            println!("{} Connected. {count} documents found.", "✓".green());
        }
        Err(e) => {
            let err_str = format!("{e:#}");
            // If we get Forbidden and no token was provided, prompt for one and retry
            if (err_str.contains("Forbidden") || err_str.contains("forbidden"))
                && !client.has_token()
            {
                eprintln!("{} Server requires authentication.", "⚠".yellow());
                let retry_token: String = Input::new()
                    .with_prompt("Bearer token")
                    .interact_text()?;
                let retry_token = strip_terminal_escapes(&retry_token);
                if !retry_token.is_empty() {
                    token = Some(retry_token);
                    client = GraphQLClient::new(url.clone(), token.clone());
                    match client.query(test_query, None).await {
                        Ok(d) => {
                            let count = d
                                .pointer("/findDocuments/totalCount")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            println!("{} Connected. {count} documents found.", "✓".green());
                        }
                        Err(e2) => {
                            eprintln!("{} Connection still failed: {e2}", "✗".red());
                            let proceed = Confirm::new()
                                .with_prompt("Save profile anyway?")
                                .default(false)
                                .interact()?;
                            if !proceed {
                                return Ok(());
                            }
                        }
                    }
                }
            } else {
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
    }
    // Run introspection
    println!("Introspecting schema...");
    match run_introspection(&client).await {
        Ok(cache) => {
            let model_count = cache.models.len();
            save_cache(&name, &cache)?;
            println!("{} {model_count} document models discovered.", "✓".green());
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
        if config
            .get_profile(&name)
            .map(|p| p.default)
            .unwrap_or(false)
        {
            " as default"
        } else {
            ""
        }
    );

    Ok(())
}

/// Strip ANSI escape sequences (e.g. bracketed-paste `\x1b[200~` / `\x1b[201~`)
/// and other non-printable control characters that terminals inject on paste.
fn strip_terminal_escapes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip the ESC and everything up to the end of the sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // Consume until we hit a letter (the terminator)
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() || next == '~' {
                        break;
                    }
                }
            }
        } else if !c.is_control() || c == '\n' {
            result.push(c);
        }
    }
    result
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
