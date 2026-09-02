use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use dialoguer::Input;

use crate::cli::helpers;
use crate::config::{IdentityConfig, load_config, save_config};
use crate::output::{OutputFormat, print_json};

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Authenticate with a bearer token, or (--renown) sign writes with your `ph login` identity
    Login {
        /// JWT token (omit to enter interactively)
        #[arg(long, conflicts_with = "renown")]
        token: Option<String>,
        /// Use the Renown identity created by `ph login` to SIGN every action this
        /// profile writes. The Switchboard then attributes your operations to your
        /// key and address instead of to its own identity.
        #[arg(long)]
        renown: bool,
        /// Directory holding `ph login`'s `.keypair.json` and `.renown.json`
        /// (default: `./.ph`, then `~/.ph`). Only the path is stored, never the key.
        #[arg(long, value_name = "DIR", requires = "renown")]
        ph_dir: Option<String>,
        /// The `app.name` stamped on your signatures — how the vault labels this
        /// writer (e.g. `powerhouse-knowledge` for an agent). Default: switchboard-cli.
        #[arg(long, value_name = "NAME", requires = "renown")]
        app_name: Option<String>,
    },
    /// Remove the bearer token (and signing identity) from the current profile
    Logout {
        /// Remove only the signing identity, keep the token
        #[arg(long)]
        identity_only: bool,
    },
    /// Show authentication and signing status
    Status,
    /// Print the current bearer token
    Token,
}

pub async fn run(cmd: AuthCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        AuthCommand::Login {
            token: _,
            renown: true,
            ph_dir,
            app_name,
        } => login_renown(ph_dir, app_name, format, profile_name).await,
        AuthCommand::Login { token, .. } => login(token, format, profile_name).await,
        AuthCommand::Logout { identity_only } => logout(identity_only, profile_name).await,
        AuthCommand::Status => status(format, profile_name).await,
        AuthCommand::Token => print_token(profile_name).await,
    }
}

/// `auth login --renown`: point the profile at a `ph login` identity.
///
/// Loads and validates it first — keypair parses, derives the did the Renown
/// credential authorised, credential not expired — so a bad directory is
/// refused here rather than on the first write.
async fn login_renown(
    ph_dir: Option<String>,
    app_name: Option<String>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let mut config = load_config()?;
    let (name, _profile) = helpers::resolve_profile(&config, profile_name)?;

    let dir = match ph_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => crate::identity::default_ph_dir().ok_or_else(|| {
            anyhow::anyhow!(
                "no `ph login` identity found in ./.ph or ~/.ph. Run `ph login` first \
                 (it writes .ph/.keypair.json and .ph/.renown.json), or pass --ph-dir."
            )
        })?,
    };
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let identity = crate::identity::Identity::load(&dir)?;
    let app_name = app_name.unwrap_or_else(|| "switchboard-cli".to_string());
    if app_name.trim().is_empty() {
        bail!("--app-name cannot be empty");
    }

    if let Some(p) = config.profiles.get_mut(&name) {
        p.identity = Some(IdentityConfig {
            ph_dir: dir.to_string_lossy().to_string(),
            app_name: app_name.clone(),
        });
    }
    save_config(&config)?;

    let expires = identity.credential_expires().map(str::to_string);
    let expired = identity.credential_expired();
    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&serde_json::json!({
            "profile": name,
            "signing": true,
            "did": identity.did,
            "address": identity.user.address,
            "app_name": app_name,
            "ph_dir": dir,
            "credential_expires": expires,
            "credential_expired": expired,
        })),
        _ => {
            println!("{} Profile '{name}' will sign every write.", "✓".green());
            println!("  Key (did):  {}", identity.did);
            println!("  Acting for: {}", identity.user.address);
            println!("  App name:   {app_name}");
            println!("  Identity:   {}", dir.display());
            match (expires, expired) {
                (Some(e), false) => println!("  Credential: valid until {e}"),
                (Some(e), true) => println!(
                    "  Credential: {} expired {e} — run `ph login` in {} to renew",
                    "⚠".yellow(),
                    dir.display()
                ),
                (None, _) => println!("  Credential: (no expiry recorded)"),
            }
        }
    }
    Ok(())
}

async fn login(
    token: Option<String>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
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
        _ => {
            println!("{} Token saved to profile '{name}'", "✓".green());
        }
    }

    Ok(())
}

async fn logout(identity_only: bool, profile_name: Option<&str>) -> Result<()> {
    let mut config = load_config()?;
    let (name, _profile) = helpers::resolve_profile(&config, profile_name)?;

    let mut removed: Vec<&str> = Vec::new();
    if let Some(p) = config.profiles.get_mut(&name) {
        if p.identity.take().is_some() {
            removed.push("signing identity");
        }
        if !identity_only && p.token.take().is_some() {
            removed.push("token");
        }
    }
    if removed.is_empty() {
        println!(
            "Profile '{name}' has no {} configured.",
            if identity_only {
                "signing identity"
            } else {
                "token or signing identity"
            }
        );
        return Ok(());
    }
    save_config(&config)?;

    println!(
        "{} Removed {} from profile '{name}'",
        "✓".green(),
        removed.join(" and ")
    );
    Ok(())
}

async fn status(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let (name, profile) = helpers::resolve_profile(&config, profile_name)?;

    let has_token = profile.token.is_some();
    let has_env = std::env::var("SWITCHBOARD_TOKEN").is_ok();

    // Signing: report what the NEXT write would do, resolving env overrides
    // exactly as the write paths do. A configured-but-broken identity is
    // reported as such, not hidden.
    let signing = match helpers::load_identity(&profile) {
        Ok(Some((identity, app_name))) => serde_json::json!({
            "signing": true,
            "did": identity.did,
            "address": identity.user.address,
            "app_name": app_name,
            "ph_dir": identity.ph_dir,
            "credential_expires": identity.credential_expires(),
            "credential_expired": identity.credential_expired(),
        }),
        Ok(None) => serde_json::json!({ "signing": false }),
        Err(e) => serde_json::json!({ "signing": false, "identity_error": e.to_string() }),
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let mut out = serde_json::json!({
                "profile": name,
                "url": profile.url,
                "has_token": has_token,
                "env_override": has_env,
            });
            if let (Some(o), Some(s)) = (out.as_object_mut(), signing.as_object()) {
                for (k, v) in s {
                    o.insert(k.clone(), v.clone());
                }
            }
            print_json(&out);
        }
        _ => {
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
            if signing["signing"] == true {
                println!(
                    "Signing:  {} as {} ({})",
                    "on".green(),
                    signing["app_name"].as_str().unwrap_or("?"),
                    signing["did"].as_str().unwrap_or("?")
                );
                println!(
                    "          acting for {}",
                    signing["address"].as_str().unwrap_or("?")
                );
                if signing["credential_expired"] == true {
                    println!(
                        "          {} credential expired {} — run `ph login` to renew",
                        "⚠".yellow(),
                        signing["credential_expires"].as_str().unwrap_or("?")
                    );
                }
            } else if let Some(err) = signing["identity_error"].as_str() {
                println!("Signing:  {} — {err}", "broken".red());
            } else {
                println!(
                    "Signing:  {} (writes are signed by the Switchboard's own identity; run \
                     `switchboard auth login --renown` to sign as yourself)",
                    "off".dimmed()
                );
            }
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
