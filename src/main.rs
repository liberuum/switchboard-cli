mod cli;
mod config;
mod graphql;
mod output;
mod phd;

use std::io::IsTerminal;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;

use cli::Cli;

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
    // Interactive is handled here (not in dispatch) to avoid async recursion.
    if matches!(command, cli::Commands::Interactive) {
        return cli::interactive::run(profile, quiet).await;
    }

    cli::dispatch(command, format, profile, quiet).await
}
