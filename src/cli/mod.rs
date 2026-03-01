pub mod access;
pub mod auth;
pub mod completions;
pub mod config;
pub mod docs;
pub mod drives;
pub mod field_editor;
pub mod groups;
pub mod guide;
pub mod helpers;
pub mod import_export;
pub mod init;
pub mod interactive;
pub mod introspect;
pub mod jobs;
pub mod models;
pub mod mutate;
pub mod ops;
pub mod query;
pub mod schema;
pub mod sync;
pub mod update;
pub mod visualize;
pub mod watch;

use anyhow::Result;
use colored::Colorize;

use crate::output::OutputFormat;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "switchboard", about = "CLI for Switchboard GraphQL instances")]
#[command(version, propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Output format (table, json, raw). Defaults to table for TTY, json for pipes.
    #[arg(long, global = true)]
    pub format: Option<OutputFormat>,

    /// Suppress extra output
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Use a specific profile instead of the default
    #[arg(long, short, global = true)]
    pub profile: Option<String>,

    /// Launch interactive REPL mode (shorthand for `interactive`)
    #[arg(short = 'i', global = true)]
    pub interactive: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new Switchboard connection
    Init,

    /// Manage connection profiles
    #[command(subcommand)]
    Config(config::ConfigCommand),

    /// Re-discover schema from current instance
    Introspect,

    /// Quick connection health check
    Ping,

    /// Show instance info (drive count, model count)
    Info,

    /// Dump the full GraphQL schema
    Schema,

    /// Manage drives
    #[command(subcommand)]
    Drives(drives::DrivesCommand),

    /// Manage documents
    #[command(subcommand)]
    Docs(docs::DocsCommand),

    /// Discover and inspect document models
    #[command(subcommand)]
    Models(models::ModelsCommand),

    /// View operation history
    Ops(ops::OpsArgs),

    /// Run a raw GraphQL query
    Query(query::QueryArgs),

    /// Export documents or drives as .phd files
    #[command(subcommand)]
    Export(import_export::ExportCommand),

    /// Import .phd files into a drive
    Import {
        /// .phd file paths
        files: Vec<String>,
        /// Target drive ID or slug
        #[arg(long)]
        drive: String,
    },

    /// Manage authentication
    #[command(subcommand)]
    Auth(auth::AuthCommand),

    /// Manage document access permissions
    #[command(subcommand)]
    Access(access::AccessCommand),

    /// Manage user groups
    #[command(subcommand)]
    Groups(groups::GroupsCommand),

    /// Watch for real-time changes via WebSocket
    #[command(subcommand)]
    Watch(watch::WatchCommand),

    /// Track async job status
    #[command(subcommand)]
    Jobs(jobs::JobsCommand),

    /// Sync channel operations
    #[command(subcommand)]
    Sync(sync::SyncCommand),

    /// Update the CLI to the latest version
    Update(update::UpdateArgs),

    /// Visualize all drives and documents as a diagram
    Visualize {
        /// Output file path (required for PNG, optional for SVG/Mermaid)
        #[arg(long, short)]
        out: Option<String>,
    },

    /// Launch interactive REPL mode
    Interactive,

    /// Built-in documentation and guides
    #[command(subcommand)]
    Guide(guide::GuideCommand),

    /// Generate shell completions (auto-detects shell, or specify explicitly)
    Completions(completions::CompletionsArgs),
}

/// Central dispatcher shared by both the CLI entry point and the interactive REPL.
pub async fn dispatch(
    command: Commands,
    format: OutputFormat,
    profile: Option<&str>,
    quiet: bool,
) -> Result<()> {
    match command {
        Commands::Init => init::run().await,
        Commands::Config(cmd) => config::run(cmd, format).await,
        Commands::Introspect => introspect::run(profile, quiet).await,
        Commands::Ping => ping(profile, quiet).await,
        Commands::Info => info(profile, format).await,
        Commands::Schema => schema::run(format, profile).await,
        Commands::Drives(cmd) => drives::run(cmd, format, profile).await,
        Commands::Docs(cmd) => docs::run(cmd, format, profile).await,
        Commands::Models(cmd) => models::run(cmd, format, profile).await,
        Commands::Ops(args) => ops::run(args, format, profile).await,
        Commands::Query(args) => query::run(args, format, profile).await,
        Commands::Export(cmd) => import_export::run_export(cmd, format, profile, quiet).await,
        Commands::Import { files, drive } => {
            import_export::run_import(files, drive, format, profile, quiet).await
        }
        Commands::Auth(cmd) => auth::run(cmd, format, profile).await,
        Commands::Access(cmd) => access::run(cmd, format, profile).await,
        Commands::Groups(cmd) => groups::run(cmd, format, profile).await,
        Commands::Watch(cmd) => watch::run(cmd, format, profile, quiet).await,
        Commands::Jobs(cmd) => jobs::run(cmd, format, profile, quiet).await,
        Commands::Sync(cmd) => sync::run(cmd, format, profile).await,
        Commands::Update(args) => update::run(args.check, quiet).await,
        Commands::Visualize { out } => visualize::run(format, out.as_deref(), profile, quiet).await,
        Commands::Interactive => anyhow::bail!("Already in interactive mode"),
        Commands::Guide(topic) => guide::run(topic),
        Commands::Completions(args) => completions::run(args),
    }
}

async fn ping(profile_name: Option<&str>, quiet: bool) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

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

async fn info(profile_name: Option<&str>, format: OutputFormat) -> Result<()> {
    let (name, _profile, client) = helpers::setup(profile_name)?;

    let data = client
        .query("{ driveDocuments { id name slug } }", None)
        .await?;

    let drives = data
        .get("driveDocuments")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let cache = crate::graphql::introspection::load_cache(&name)?;
    let models = cache.as_ref().map(|c| c.models.len()).unwrap_or(0);

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            crate::output::print_json(&serde_json::json!({
                "profile": name,
                "url": client.url,
                "drives": drives,
                "models": models,
                "has_token": client.has_token(),
            }));
        }
        _ => {
            println!("Profile:  {}", name.green());
            println!("URL:      {}", client.url);
            println!(
                "Auth:     {}",
                if client.has_token() {
                    "configured"
                } else {
                    "none"
                }
            );
            println!("Drives:   {drives}");
            println!(
                "Models:   {models}{}",
                if models == 0 {
                    " (run `switchboard introspect`)"
                } else {
                    ""
                }
            );
        }
    }

    Ok(())
}
