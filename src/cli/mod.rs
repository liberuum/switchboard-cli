pub mod analytics;
pub mod auth;
pub mod completions;
pub mod config;
pub mod docs;
pub mod drives;
pub mod field_editor;
pub mod folders;
pub mod guide;
pub mod helpers;
pub mod import_export;
pub mod init;
pub mod interactive;
pub mod introspect;
pub mod jobs;
pub mod migrate;
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
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

/// Build the clap command so hyphen-leading document ids parse as values.
///
/// v2 document ids (reactor >= 6.2.3) are base64url and start with `-` about
/// once in 64 (e.g. `-eMy6lwrc7uU2GgHmkW_8vnjoSNRsXefF6kIyGzDH1g`). By default
/// clap rejects `--drive <id>` or a positional `<id>` like that as an unknown
/// flag (`unexpected argument '-e' found`) — which never happened with UUIDs.
///
/// Options and single-value positionals take exactly one token, so they are
/// safe to mark `allow_hyphen_values`. Multi-value positionals (`docs delete
/// <IDS>...`) are left alone — with the flag they would swallow every
/// following flag (`-y`, `--format json`) as ids; `try_parse_ids_from`
/// handles hyphen ids there instead.
pub fn command() -> clap::Command {
    fn allow_hyphen_ids(cmd: clap::Command) -> clap::Command {
        cmd.mut_args(|arg| {
            let single_value =
                !arg.is_positional() || matches!(arg.get_action(), clap::ArgAction::Set);
            if arg.get_action().takes_values() && single_value {
                arg.allow_hyphen_values(true)
            } else {
                arg
            }
        })
        .mut_subcommands(allow_hyphen_ids)
    }
    allow_hyphen_ids(Cli::command())
}

impl Cli {
    /// Parse `args` with [`command`], accepting hyphen-leading document ids.
    pub fn try_parse_ids_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let cmd = command();
        let args = escape_hyphen_ids_in_lists(&cmd, args.into_iter().map(Into::into).collect());
        let mut matches = cmd.try_get_matches_from(args)?;
        Self::from_arg_matches_mut(&mut matches)
    }
}

/// Move hyphen-leading v2 document ids that would fill a multi-value
/// positional list (`docs delete <IDS>...`) behind a `--` separator.
///
/// Those lists can't be marked `allow_hyphen_values` (they would swallow the
/// flags that follow), and clap would otherwise read such an id as a flag
/// cluster — `-eMy…` fails as an unknown `-e`, and worse, `-pAbc…` parses
/// silently as `--profile Abc…`. Walking clap's own command tree, this tracks
/// the subcommand, which options take a value and which positional slot each
/// token fills, so option values and single-value positionals are left in
/// place (clap already accepts hyphen values there).
fn escape_hyphen_ids_in_lists(
    root: &clap::Command,
    args: Vec<std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    fn takes_value(
        root: &clap::Command,
        cur: &clap::Command,
        long: Option<&str>,
        short: Option<char>,
    ) -> bool {
        root.get_arguments().chain(cur.get_arguments()).any(|a| {
            !a.is_positional()
                && a.get_action().takes_values()
                && (long.is_some_and(|l| {
                    a.get_long() == Some(l) || a.get_all_aliases().is_some_and(|al| al.contains(&l))
                }) || short.is_some_and(|c| a.get_short() == Some(c)))
        })
    }
    fn nth_positional_is_list(cmd: &clap::Command, n: usize) -> bool {
        let positionals: Vec<&clap::Arg> = cmd.get_positionals().collect();
        let arg = positionals.get(n).or(positionals.last());
        arg.is_some_and(|a| matches!(a.get_action(), clap::ArgAction::Append))
    }

    let mut out = Vec::with_capacity(args.len() + 1);
    let mut tail = Vec::new();
    let mut iter = args.into_iter();
    if let Some(bin) = iter.next() {
        out.push(bin);
    }
    let mut cur = root.clone();
    let mut positional = 0usize;
    let mut expect_value = false;
    let mut after_separator = false;
    for tok in iter {
        let Some(s) = tok.to_str().map(str::to_owned) else {
            out.push(tok);
            continue;
        };
        if after_separator || expect_value {
            expect_value = false;
            out.push(tok);
            continue;
        }
        if s == "--" {
            after_separator = true;
            out.push(tok);
            continue;
        }
        if s.starts_with('-') && helpers::is_derived_id(&s) {
            if nth_positional_is_list(&cur, positional) {
                tail.push(tok);
            } else {
                positional += 1;
                out.push(tok);
            }
            continue;
        }
        if let Some(long) = s.strip_prefix("--") {
            expect_value = !long.contains('=') && takes_value(root, &cur, Some(long), None);
            out.push(tok);
            continue;
        }
        if s.len() > 1 && s.starts_with('-') {
            let mut chars = s[1..].chars();
            let first = chars.next();
            expect_value = chars.next().is_none() && takes_value(root, &cur, None, first);
            out.push(tok);
            continue;
        }
        if positional == 0
            && let Some(sub) = cur.find_subcommand(&s).cloned()
        {
            cur = sub;
            out.push(tok);
            continue;
        }
        if !nth_positional_is_list(&cur, positional) {
            positional += 1;
        }
        out.push(tok);
    }
    if !tail.is_empty() {
        if !after_separator {
            out.push("--".into());
        }
        out.extend(tail);
    }
    out
}

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
    /// Initialize a new Switchboard connection (interactive, or non-interactive with --url)
    Init {
        /// Switchboard GraphQL URL (skips the prompt; `/graphql` is appended if missing)
        #[arg(long)]
        url: Option<String>,
        /// Profile name (default: derived from the URL host)
        #[arg(long, requires = "url")]
        name: Option<String>,
        /// Bearer token to store (optional)
        #[arg(long, requires = "url")]
        token: Option<String>,
        /// Make this profile the default (default: only when it is the first profile)
        #[arg(long, requires = "url")]
        use_profile: bool,
        /// Overwrite an existing profile of the same name without asking
        #[arg(long, requires = "url")]
        force: bool,
    },

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

    /// Manage folders inside drives
    #[command(subcommand)]
    Folders(folders::FoldersCommand),

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
        /// Treat per-op failures as hard errors. With this flag, any op
        /// rejected by the reactor ends the import with a non-zero exit
        /// code. Without it, failures are reported per-doc and the import
        /// continues with whatever ops did succeed.
        #[arg(long)]
        strict: bool,
        /// Optional path to a JSON file mapping old document UUIDs → new
        /// UUIDs, applied to op inputs to rewrite cross-document
        /// references when importing a snapshot from another reactor.
        /// Within a single import invocation the CLI also builds this map
        /// automatically as documents are created.
        #[arg(long, value_name = "FILE")]
        id_mapping: Option<String>,
    },

    /// Migrate a drive (with full op history and preserved UUIDs) between two profiles
    Migrate {
        /// Source drive ID or slug (resolved on the --from profile)
        source_drive: String,
        /// Source profile name
        #[arg(long)]
        from: String,
        /// Destination profile name
        #[arg(long)]
        to: String,
    },

    /// Manage authentication
    #[command(subcommand)]
    Auth(auth::AuthCommand),

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

    /// Query analytics (metrics, dimensions, time series)
    #[command(subcommand)]
    Analytics(analytics::AnalyticsCommand),

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
        Commands::Init {
            url,
            name,
            token,
            use_profile,
            force,
        } => match url {
            Some(url) => init::run_non_interactive(url, name, token, use_profile, force).await,
            None => init::run().await,
        },
        Commands::Config(cmd) => config::run(cmd, format, profile).await,
        Commands::Introspect => introspect::run(profile, quiet).await,
        Commands::Ping => ping(profile, quiet).await,
        Commands::Info => info(profile, format).await,
        Commands::Schema => schema::run(format, profile).await,
        Commands::Drives(cmd) => drives::run(cmd, format, profile).await,
        Commands::Docs(cmd) => docs::run(cmd, format, profile).await,
        Commands::Folders(cmd) => folders::run(cmd, format, profile).await,
        Commands::Models(cmd) => models::run(cmd, format, profile).await,
        Commands::Ops(args) => ops::run(args, format, profile).await,
        Commands::Query(args) => query::run(args, format, profile).await,
        Commands::Export(cmd) => import_export::run_export(cmd, format, profile, quiet).await,
        Commands::Import {
            files,
            drive,
            strict,
            id_mapping,
        } => {
            import_export::run_import(files, drive, strict, id_mapping, format, profile, quiet)
                .await
        }
        Commands::Migrate {
            source_drive,
            from,
            to,
        } => migrate::run(source_drive, from, to, quiet).await,
        Commands::Auth(cmd) => auth::run(cmd, format, profile).await,
        Commands::Watch(cmd) => watch::run(cmd, format, profile, quiet).await,
        Commands::Jobs(cmd) => jobs::run(cmd, format, profile, quiet).await,
        Commands::Sync(cmd) => sync::run(cmd, format, profile).await,
        Commands::Update(args) => update::run(args.check, quiet).await,
        Commands::Visualize { out } => visualize::run(format, out.as_deref(), profile, quiet).await,
        Commands::Interactive => anyhow::bail!("Already in interactive mode"),
        Commands::Guide(topic) => guide::run(topic),
        Commands::Analytics(cmd) => analytics::run(cmd, format, profile).await,
        Commands::Completions(args) => completions::run(args),
    }
}

async fn ping(profile_name: Option<&str>, quiet: bool) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let start = std::time::Instant::now();
    client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }, paging: { limit: 1 }) { items { id } } }"#,
            None,
        )
        .await?;
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

    let drives = helpers::count_documents(&client, "powerhouse/document-drive").await?;

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

#[cfg(test)]
mod hyphen_id_tests {
    use super::{Cli, Commands};

    const ID_E: &str = "-eMy6lwrc7uU2GgHmkW_8vnjoSNRsXefF6kIyGzDH1g";
    // Start with real short flags: -p (--profile) and -y (--yes).
    const ID_P: &str = "-pBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA";
    const ID_Y: &str = "-yAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    fn parse(args: &[&str]) -> Cli {
        let argv = std::iter::once("switchboard").chain(args.iter().copied());
        Cli::try_parse_ids_from(argv).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    #[test]
    fn hyphen_id_as_option_value_and_single_positional() {
        let cli = parse(&["drives", "get", ID_P, "--format", "json"]);
        assert!(cli.profile.is_none(), "id must not become --profile");
        let cli = parse(&["docs", "list", "--drive", ID_E]);
        assert!(matches!(cli.command, Some(Commands::Docs(_))));
    }

    #[test]
    fn hyphen_ids_in_a_list_keep_trailing_flags() {
        let cli = parse(&[
            "docs", "delete", "plain", ID_P, ID_Y, "-y", "--format", "json",
        ]);
        assert!(cli.profile.is_none(), "id must not become --profile");
        match cli.command {
            Some(Commands::Docs(crate::cli::docs::DocsCommand::Delete { ids, yes })) => {
                assert!(yes, "-y after the list must still be --yes");
                let mut ids = ids;
                ids.sort();
                let mut want = vec!["plain".to_string(), ID_P.to_string(), ID_Y.to_string()];
                want.sort();
                assert_eq!(ids, want, "exactly the three ids, no flags");
            }
            _ => panic!("expected docs delete"),
        }
    }

    #[test]
    fn ordinary_commands_are_unchanged() {
        let cli = parse(&["-p", "local", "docs", "delete", "a", "b", "-y"]);
        assert_eq!(cli.profile.as_deref(), Some("local"));
    }
}
