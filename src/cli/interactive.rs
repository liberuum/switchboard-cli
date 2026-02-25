use anyhow::Result;
use clap::Parser;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Config, Editor, Helper};

use crate::cli::helpers;
use crate::cli::{Cli, Commands};
use crate::output::{OutputFormat, print_json};

// ── Tab-completion helper ───────────────────────────────────────────────────

struct ReplHelper {
    /// Static command prefixes for first-level completion
    commands: Vec<String>,
    /// Drive slugs fetched at startup
    drive_slugs: Vec<String>,
    /// Document model types from introspection cache
    model_types: Vec<String>,
    /// Guide topic names
    guide_topics: Vec<String>,
}

impl ReplHelper {
    fn new(drive_slugs: Vec<String>, model_types: Vec<String>) -> Self {
        let commands = vec![
            // Drives
            "drives list".into(),
            "drives get ".into(),
            "drives create".into(),
            "drives delete ".into(),
            // Docs
            "docs list --drive ".into(),
            "docs get ".into(),
            "docs tree --drive ".into(),
            "docs create".into(),
            "docs delete ".into(),
            "docs mutate ".into(),
            // Models
            "models list".into(),
            "models get ".into(),
            // Ops
            "ops ".into(),
            // Config
            "config list".into(),
            "config show".into(),
            "config use ".into(),
            "config remove ".into(),
            // Auth
            "auth login".into(),
            "auth logout".into(),
            "auth status".into(),
            "auth token".into(),
            // Access
            "access show ".into(),
            "access grant ".into(),
            "access revoke ".into(),
            "access grant-group ".into(),
            "access revoke-group ".into(),
            "access ops ".into(),
            // Groups
            "groups list".into(),
            "groups get ".into(),
            "groups create ".into(),
            "groups delete ".into(),
            "groups add-user ".into(),
            "groups remove-user ".into(),
            "groups user-groups ".into(),
            // Export / Import
            "export doc ".into(),
            "export drive ".into(),
            "import ".into(),
            // Watch
            "watch docs".into(),
            "watch docs --drive ".into(),
            "watch docs --doc ".into(),
            "watch docs --type ".into(),
            "watch job ".into(),
            // Jobs
            "jobs status ".into(),
            "jobs wait ".into(),
            "jobs watch ".into(),
            // Sync
            "sync touch ".into(),
            "sync push ".into(),
            "sync poll ".into(),
            // Other
            "query ".into(),
            "schema".into(),
            "ping".into(),
            "info".into(),
            "introspect".into(),
            "update".into(),
            "update --check".into(),
            "completions --install".into(),
            "guide ".into(),
            // REPL-only
            "help".into(),
            "exit".into(),
            "quit".into(),
        ];

        let guide_topics = vec![
            "overview".into(),
            "config".into(),
            "drives".into(),
            "docs".into(),
            "import-export".into(),
            "auth".into(),
            "permissions".into(),
            "watch".into(),
            "jobs".into(),
            "sync".into(),
            "interactive".into(),
            "output".into(),
            "graphql".into(),
            "commands".into(),
        ];

        Self {
            commands,
            drive_slugs,
            model_types,
            guide_topics,
        }
    }
}

fn filter_pairs(candidates: &[String], partial: &str) -> Vec<Pair> {
    candidates
        .iter()
        .filter(|s| s.starts_with(partial))
        .map(|s| Pair {
            display: s.clone(),
            replacement: s.clone(),
        })
        .collect()
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let input = &line[..pos];
        let word_start = input.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let partial = &input[word_start..];
        let words_before: Vec<&str> = input[..word_start].split_whitespace().collect();
        let prev_word = words_before.last().copied();

        // Drive slug completion: after --drive flag or positional drive args
        if prev_word == Some("--drive")
            || input.starts_with("drives get ")
            || input.starts_with("drives delete ")
            || input.starts_with("export drive ")
        {
            let matches = filter_pairs(&self.drive_slugs, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // Model type completion: after --type / -t flag or models get
        if prev_word == Some("--type")
            || prev_word == Some("-t")
            || input.starts_with("models get ")
        {
            let matches = filter_pairs(&self.model_types, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // Guide topic completion
        if input.starts_with("guide ") {
            let matches = filter_pairs(&self.guide_topics, partial);
            return Ok((word_start, matches));
        }

        // First-level command completion
        let matches: Vec<Pair> = self
            .commands
            .iter()
            .filter(|c| c.starts_with(input))
            .map(|c| Pair {
                display: c.clone(),
                replacement: c.clone(),
            })
            .collect();
        Ok((0, matches))
    }
}

impl Hinter for ReplHelper {
    type Hint = String;
}

impl Highlighter for ReplHelper {}
impl Validator for ReplHelper {}
impl Helper for ReplHelper {}

// ── Shell-like tokeniser ────────────────────────────────────────────────────

/// Split a line into tokens, respecting single and double quotes.
fn shell_split(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escape = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

// ── REPL entry point ────────────────────────────────────────────────────────

pub async fn run(profile_name: Option<&str>, quiet: bool) -> Result<()> {
    let (name, _profile, client) = helpers::setup(profile_name)?;

    // Load introspection cache for context
    let cache = crate::graphql::introspection::load_cache(&name)?;
    let model_count = cache.as_ref().map(|c| c.models.len()).unwrap_or(0);

    // Collect model types for tab completion
    let model_types: Vec<String> = cache
        .as_ref()
        .map(|c| c.models.values().map(|m| m.document_type.clone()).collect())
        .unwrap_or_default();

    // Fetch drive slugs for tab completion
    let drive_slugs: Vec<String> = match client.query("{ driveDocuments { slug } }", None).await {
        Ok(data) => data
            .get("driveDocuments")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| d["slug"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    if !quiet {
        eprintln!("Switchboard interactive mode");
        eprintln!("Profile: {} ({})", name, client.url);
        eprintln!("Models:  {model_count}");
        eprintln!();
        eprintln!("Type 'help' for commands, or append --help to any command.");
        eprintln!();
    }

    // Set up rustyline with history and completion
    let config = Config::builder()
        .max_history_size(1000)?
        .auto_add_history(true)
        .build();

    let helper = ReplHelper::new(drive_slugs, model_types);
    let mut rl: Editor<ReplHelper, rustyline::history::DefaultHistory> =
        Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    // Load history from ~/.switchboard/history
    let history_path = dirs::home_dir().map(|h| h.join(".switchboard").join("history"));
    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    }

    let prompt = format!("{name}> ");

    loop {
        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // ── REPL-only commands ──────────────────────────────
                match line {
                    "exit" | "quit" | "q" => break,
                    "help" | "?" => {
                        print_repl_help();
                        continue;
                    }
                    _ => {}
                }

                // ── Raw GraphQL shorthand: query { ... } ────────────
                if let Some(after_query) = line.strip_prefix("query ") {
                    let rest = after_query.trim_start();
                    if rest.starts_with('{')
                        || rest.starts_with("mutation")
                        || rest.starts_with("subscription")
                    {
                        match client.query(rest, None).await {
                            Ok(data) => print_json(&data),
                            Err(e) => eprintln!("Error: {e:#}"),
                        }
                        continue;
                    }
                }

                // ── Parse as CLI command via clap ────────────────────
                let tokens = shell_split(line);
                let args = std::iter::once("switchboard".to_string()).chain(tokens);

                match Cli::try_parse_from(args) {
                    Ok(parsed) => {
                        // Block recursive entry into interactive mode
                        if matches!(parsed.command, Some(Commands::Interactive)) {
                            eprintln!("Already in interactive mode.");
                            continue;
                        }

                        let Some(command) = parsed.command else {
                            eprintln!("Type 'help' for available commands.");
                            continue;
                        };

                        // Use parsed flags if given, otherwise fall back to REPL defaults
                        let cmd_profile = parsed.profile.as_deref().or(profile_name);
                        let format = parsed.format.unwrap_or(OutputFormat::Table);
                        let cmd_quiet = parsed.quiet || quiet;

                        if let Err(e) =
                            crate::cli::dispatch(command, format, cmd_profile, cmd_quiet).await
                        {
                            eprintln!("Error: {e:#}");
                        }
                    }
                    Err(e) => {
                        let _ = e.print();
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C — just print a new prompt
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl+D — exit
                break;
            }
            Err(err) => {
                eprintln!("Error: {err}");
                break;
            }
        }
    }

    // Save history
    if let Some(ref path) = history_path {
        let _ = rl.save_history(path);
    }

    Ok(())
}

// ── Help ────────────────────────────────────────────────────────────────────

fn print_repl_help() {
    eprintln!("Commands:");
    eprintln!();
    eprintln!("  Drives & Documents:");
    eprintln!("    drives   list | get | create | delete");
    eprintln!("    docs     list | get | tree | create | delete | mutate");
    eprintln!("    models   list | get");
    eprintln!("    ops      <doc-id> --drive <drive>");
    eprintln!();
    eprintln!("  Configuration:");
    eprintln!("    config   list | show | use | remove");
    eprintln!("    auth     login | logout | status | token");
    eprintln!();
    eprintln!("  Permissions:");
    eprintln!("    access   show | grant | revoke | grant-group | revoke-group | ops");
    eprintln!("    groups   list | get | create | delete | add-user | remove-user");
    eprintln!();
    eprintln!("  Import / Export:");
    eprintln!("    export   doc | drive");
    eprintln!("    import   <files> --drive <drive>");
    eprintln!();
    eprintln!("  Real-time & Jobs:");
    eprintln!("    watch    docs | job");
    eprintln!("    jobs     status | wait | watch");
    eprintln!("    sync     touch | push | poll");
    eprintln!();
    eprintln!("  Other:");
    eprintln!("    query    \"<graphql>\" | --file <path>");
    eprintln!("    schema | ping | info | introspect");
    eprintln!("    guide    <topic>");
    eprintln!();
    eprintln!("  Shortcuts:");
    eprintln!("    query {{ ... }}    Run raw GraphQL without quotes");
    eprintln!("    help | ?         Show this help");
    eprintln!("    exit | quit | q  Exit interactive mode");
    eprintln!();
    eprintln!("  Tip: Append --help to any command for details.");
}

#[cfg(test)]
mod tests {
    use super::shell_split;

    #[test]
    fn simple_words() {
        assert_eq!(shell_split("drives list"), vec!["drives", "list"]);
    }

    #[test]
    fn extra_whitespace() {
        assert_eq!(
            shell_split("  drives   delete  foo  bar "),
            vec!["drives", "delete", "foo", "bar"]
        );
    }

    #[test]
    fn double_quoted_string() {
        assert_eq!(
            shell_split(r#"query "{ drives { id name } }""#),
            vec!["query", "{ drives { id name } }"]
        );
    }

    #[test]
    fn single_quoted_string() {
        assert_eq!(
            shell_split("docs mutate --input '{\"key\": \"val\"}'"),
            vec!["docs", "mutate", "--input", r#"{"key": "val"}"#]
        );
    }

    #[test]
    fn backslash_escape() {
        assert_eq!(
            shell_split(r#"query hello\ world"#),
            vec!["query", "hello world"]
        );
    }

    #[test]
    fn empty_input() {
        assert!(shell_split("").is_empty());
        assert!(shell_split("   ").is_empty());
    }

    #[test]
    fn tabs_as_separators() {
        assert_eq!(shell_split("drives\tlist"), vec!["drives", "list"]);
    }

    #[test]
    fn mixed_quotes() {
        assert_eq!(
            shell_split(r#"--name "hello 'world'" --flag"#),
            vec!["--name", "hello 'world'", "--flag"]
        );
    }
}
