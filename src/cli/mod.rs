pub mod init;
pub mod config;
pub mod introspect;
pub mod drives;
pub mod docs;
pub mod models;
pub mod ops;
pub mod mutate;
pub mod query;
pub mod helpers;
pub mod import_export;
pub mod auth;
pub mod access;
pub mod groups;
pub mod completions;
pub mod schema;
pub mod watch;
pub mod jobs;
pub mod sync;
pub mod interactive;
pub mod guide;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use crate::output::OutputFormat;

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

    /// Launch interactive REPL mode
    Interactive,

    /// Built-in documentation and guides
    #[command(subcommand)]
    Guide(guide::GuideCommand),

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}
