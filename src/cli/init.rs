use anyhow::Result;
use colored::Colorize;
use dialoguer::{Confirm, Input};

use crate::config::{Profile, load_config, save_config};
use crate::graphql::GraphQLClient;
use crate::graphql::introspection::{run_introspection, save_cache};

/// `switchboard init --url … [--name …] [--token …] [--use-profile] [--force]`
///
/// The scriptable form: no prompts, so an agent or a setup script can create a
/// profile. Connection and introspection are attempted exactly as in the
/// interactive flow, but a failure is reported and the profile is still
/// saved — the caller asked for a profile, and `switchboard ping` will tell
/// it the truth about the connection afterwards.
pub async fn run_non_interactive(
    url: String,
    name: Option<String>,
    token: Option<String>,
    use_profile: bool,
    force: bool,
) -> Result<()> {
    let mut config = load_config()?;
    let url = normalize_url(&strip_terminal_escapes(&url));
    let name = name.unwrap_or_else(|| profile_name_from_url(&url));
    if config.get_profile(&name).is_some() && !force {
        anyhow::bail!("Profile '{name}' already exists. Pass --force to overwrite it.");
    }
    let token = token
        .map(|t| strip_terminal_escapes(&t))
        .filter(|t| !t.is_empty());

    let client = GraphQLClient::new(url.clone(), token.clone());
    let test_query = r#"{ findDocuments(search: { type: "powerhouse/document-drive" }, paging: { limit: 1 }) { totalCount } }"#;
    let connected = match client.query(test_query, None).await {
        Ok(_) => true,
        Err(e) => {
            eprintln!("{} Connection failed: {e}", "⚠".yellow());
            false
        }
    };
    let mut models = 0usize;
    if connected {
        match run_introspection(&client).await {
            Ok(cache) => {
                models = cache.models.len();
                save_cache(&name, &cache)?;
            }
            Err(e) => eprintln!(
                "{} Introspection failed: {e}. Retry with `switchboard introspect`.",
                "⚠".yellow()
            ),
        }
    }

    let existing = config.get_profile(&name).cloned();
    let profile = Profile {
        url: url.clone(),
        token,
        default: use_profile
            || config.profiles.is_empty()
            || existing.as_ref().map(|p| p.default).unwrap_or(false),
        identity: existing.and_then(|p| p.identity),
    };
    config.add_profile(name.clone(), profile);
    save_config(&config)?;

    println!(
        "{} Profile \"{name}\" saved for {url} ({}{}).",
        "✓".green(),
        if connected {
            format!("connected, {models} document models")
        } else {
            "not reachable right now".to_string()
        },
        if config
            .get_profile(&name)
            .map(|p| p.default)
            .unwrap_or(false)
        {
            "; now the default"
        } else {
            ""
        }
    );
    Ok(())
}

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
                let retry_token: String =
                    Input::new().with_prompt("Bearer token").interact_text()?;
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

    // Save profile — preserve default flag when overwriting an existing profile
    let was_default = config
        .get_profile(&name)
        .map(|p| p.default)
        .unwrap_or(false);
    // Re-initialising a profile keeps its signing identity: the URL changed,
    // not who is writing.
    let identity = config.get_profile(&name).and_then(|p| p.identity.clone());
    let profile = Profile {
        url,
        token,
        default: config.profiles.is_empty() || was_default,
        identity,
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
