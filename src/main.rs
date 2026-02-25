mod cli;
mod config;
mod graphql;
mod output;
mod phd;

use std::io::IsTerminal;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{} {e:#}", "Error:".red().bold());
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    // Handle --no-color and NO_COLOR env var
    if cli.no_color || std::env::var("NO_COLOR").is_ok() {
        colored::control::set_override(false);
    }

    let profile = cli.profile.as_deref();

    // Auto-detect format: table for TTY, json for pipes
    let format = cli.format.unwrap_or_else(|| {
        if std::io::stdout().is_terminal() {
            output::OutputFormat::Table
        } else {
            output::OutputFormat::Json
        }
    });

    let quiet = cli.quiet;

    // Handle -i flag or no subcommand
    let command = match cli.command {
        Some(cmd) => cmd,
        None if cli.interactive => return cli::interactive::run(profile, quiet).await,
        None => {
            // No subcommand and no -i flag: print help
            use clap::CommandFactory;
            Cli::command().print_help()?;
            return Ok(());
        }
    };

    // -i flag with a subcommand: ignore -i, run the subcommand
    match command {
        Commands::Init => cli::init::run().await,
        Commands::Config(cmd) => cli::config::run(cmd, format).await,
        Commands::Introspect => cli::introspect::run(profile, quiet).await,
        Commands::Ping => ping(profile, quiet).await,
        Commands::Info => info(profile, format).await,
        Commands::Drives(cmd) => cli::drives::run(cmd, format, profile).await,
        Commands::Docs(cmd) => cli::docs::run(cmd, format, profile).await,
        Commands::Models(cmd) => cli::models::run(cmd, format, profile).await,
        Commands::Ops(args) => cli::ops::run(args, format, profile).await,
        Commands::Query(args) => cli::query::run(args, format, profile).await,
        Commands::Export(cmd) => cli::import_export::run_export(cmd, format, profile, quiet).await,
        Commands::Import { files, drive } => {
            cli::import_export::run_import(files, drive, format, profile, quiet).await
        }
        Commands::Auth(cmd) => cli::auth::run(cmd, format, profile).await,
        Commands::Access(cmd) => cli::access::run(cmd, format, profile).await,
        Commands::Groups(cmd) => cli::groups::run(cmd, format, profile).await,
        Commands::Schema => cli::schema::run(format, profile).await,
        Commands::Watch(cmd) => cli::watch::run(cmd, format, profile, quiet).await,
        Commands::Jobs(cmd) => cli::jobs::run(cmd, format, profile, quiet).await,
        Commands::Sync(cmd) => cli::sync::run(cmd, format, profile).await,
        Commands::Interactive => cli::interactive::run(profile, quiet).await,
        Commands::Guide(topic) => cli::guide::run(topic),
        Commands::Completions { shell } => cli::completions::run(shell),
    }
}



async fn ping(profile_name: Option<&str>, quiet: bool) -> Result<()> {
    let (_name, _profile, client) = cli::helpers::setup(profile_name)?;

    let start = std::time::Instant::now();
    client.query("{ drives }", None).await?;
    let elapsed = start.elapsed();

    if !quiet {
        println!(
            "{} {} responded in {:.0?}",
            "✓".green(),
            client.url,
            elapsed
        );
    }
    Ok(())
}

async fn info(profile_name: Option<&str>, format: output::OutputFormat) -> Result<()> {
    let (name, _profile, client) = cli::helpers::setup(profile_name)?;

    let data = client
        .query("{ driveDocuments { id name slug } }", None)
        .await?;

    let drives = data
        .get("driveDocuments")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let cache = graphql::introspection::load_cache(&name)?;
    let models = cache.as_ref().map(|c| c.models.len()).unwrap_or(0);

    match format {
        output::OutputFormat::Json | output::OutputFormat::Raw => {
            output::print_json(&serde_json::json!({
                "profile": name,
                "url": client.url,
                "drives": drives,
                "models": models,
                "has_token": client.has_token(),
            }));
        }
        output::OutputFormat::Table => {
            println!("Profile:  {}", name.green());
            println!("URL:      {}", client.url);
            println!("Auth:     {}", if client.has_token() { "configured" } else { "none" });
            println!("Drives:   {drives}");
            println!("Models:   {models}{}", if models == 0 { " (run `switchboard introspect`)" } else { "" });
        }
    }

    Ok(())
}
