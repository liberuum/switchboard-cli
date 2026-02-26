use std::io::Write;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, Editor, Helper};

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
    /// Profile names from config
    profile_names: Vec<String>,
    /// Document IDs for completion (the raw UUID)
    doc_ids: Vec<String>,
    /// Document display labels for completion ("uuid  name  (type)")
    doc_labels: Vec<String>,
}

/// A document entry for tab-completion.
struct DocEntry {
    id: String,
    name: String,
    doc_type: String,
}

impl ReplHelper {
    fn new(
        drive_slugs: Vec<String>,
        model_types: Vec<String>,
        profile_names: Vec<String>,
        docs: Vec<DocEntry>,
    ) -> Self {
        let (doc_ids, doc_labels) = Self::build_doc_completions(&docs);

        let commands = vec![
            // Drives
            "drives list".into(),
            "drives get ".into(),
            "drives create".into(),
            "drives delete ".into(),
            // Docs
            "docs list".into(),
            "docs list --drive ".into(),
            "docs get ".into(),
            "docs get --state ".into(),
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
            "export all".into(),
            "export all --out ".into(),
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
            profile_names,
            doc_ids,
            doc_labels,
        }
    }

    fn build_doc_completions(docs: &[DocEntry]) -> (Vec<String>, Vec<String>) {
        // replacements: what gets inserted (name, quoted if spaces; fallback to ID)
        let replacements: Vec<String> = docs
            .iter()
            .map(|d| {
                if d.name.is_empty() {
                    d.id.clone()
                } else if d.name.contains(' ') {
                    format!("\"{}\"", d.name)
                } else {
                    d.name.clone()
                }
            })
            .collect();
        // labels: for matching — include id, name, and type so partial matches work
        let labels: Vec<String> = docs
            .iter()
            .map(|d| format!("{} {} {}", d.id, d.name, d.doc_type))
            .collect();
        (replacements, labels)
    }

    fn update_docs(&mut self, docs: Vec<DocEntry>) {
        let (replacements, labels) = Self::build_doc_completions(&docs);
        self.doc_ids = replacements;
        self.doc_labels = labels;
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

/// Build Pairs for document completion: replacement is the name (or ID),
/// matching is done against a label that contains id + name + type.
fn filter_doc_pairs(replacements: &[String], labels: &[String], partial: &str) -> Vec<Pair> {
    let partial_lower = partial.to_lowercase();
    // Also match against partial without surrounding quotes
    let partial_unquoted = partial.trim_matches('"').to_lowercase();
    replacements
        .iter()
        .zip(labels.iter())
        .filter(|(_repl, label)| {
            label.to_lowercase().contains(&partial_lower)
                || label.to_lowercase().contains(&partial_unquoted)
        })
        .map(|(repl, _label)| Pair {
            display: repl.clone(),
            replacement: repl.clone(),
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

        // ── Drive slug completion ────────────────────────────
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

        // ── Document ID/name completion ──────────────────────
        // After commands that take a doc ID as a positional arg
        if input.starts_with("docs get ")
            || input.starts_with("docs delete ")
            || input.starts_with("docs mutate ")
            || input.starts_with("export doc ")
            || input.starts_with("access show ")
            || input.starts_with("access grant ")
            || input.starts_with("access revoke ")
            || input.starts_with("access ops ")
        {
            // Only complete the first positional arg (the doc ID)
            let after_cmd = words_before.len();
            if after_cmd <= 2 {
                let matches = filter_doc_pairs(&self.doc_ids, &self.doc_labels, partial);
                if !matches.is_empty() {
                    return Ok((word_start, matches));
                }
            }
        }
        // ops takes doc ID as first arg
        if input.starts_with("ops ") && words_before.len() <= 1 {
            let matches = filter_doc_pairs(&self.doc_ids, &self.doc_labels, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }
        // after --doc flag
        if prev_word == Some("--doc") {
            let matches = filter_doc_pairs(&self.doc_ids, &self.doc_labels, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // ── Profile name completion ──────────────────────────
        if input.starts_with("config use ") || input.starts_with("config remove ") {
            let matches = filter_pairs(&self.profile_names, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }
        // after --profile / -p flag
        if prev_word == Some("--profile") || prev_word == Some("-p") {
            let matches = filter_pairs(&self.profile_names, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // ── Model type completion ────────────────────────────
        if prev_word == Some("--type")
            || prev_word == Some("-t")
            || input.starts_with("models get ")
        {
            let matches = filter_pairs(&self.model_types, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // ── Guide topic completion ───────────────────────────
        if input.starts_with("guide ") {
            let matches = filter_pairs(&self.guide_topics, partial);
            return Ok((word_start, matches));
        }

        // ── First-level command completion ────────────────────
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

// ── Terminal helpers ─────────────────────────────────────────────────────────

/// Ensure the terminal cursor is visible (dialoguer widgets may hide it).
fn show_cursor() {
    eprint!("\x1b[?25h");
}

/// Spawn a background task that shows an animated spinner on stderr.
/// The first frame is printed synchronously so it's visible immediately.
fn spawn_spinner(message: &str) -> tokio::task::JoinHandle<()> {
    // Print first frame synchronously so it's visible before any await
    eprint!("\r\x1b[2K⠋ {message}");
    let _ = std::io::stderr().flush();

    let msg = message.to_string();
    tokio::spawn(async move {
        let frames = ['⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '⠋'];
        let mut i = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            eprint!("\r\x1b[2K{} {msg}", frames[i % frames.len()]);
            let _ = std::io::stderr().flush();
            i += 1;
        }
    })
}

/// Stop the spinner and clear its line.
fn stop_spinner(handle: tokio::task::JoinHandle<()>) {
    handle.abort();
    eprint!("\r\x1b[2K");
    let _ = std::io::stderr().flush();
}

/// Print a visual separator before command output so it's easy to spot.
fn print_command_separator(cmd: &str) {
    let display = if cmd.len() > 40 {
        format!("{}...", &cmd[..37])
    } else {
        cmd.to_string()
    };
    let label = format!("──── {display} ");
    let total_width: usize = 60;
    let padding_len = total_width.saturating_sub(label.chars().count());
    eprintln!();
    eprintln!("{}", format!("{label}{}", "─".repeat(padding_len)).dimmed());
}

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

// ── Doc-fetching for tab completion ──────────────────────────────────────────

async fn fetch_doc_entries(client: &crate::graphql::GraphQLClient) -> Vec<DocEntry> {
    let data = match client
        .query(
            r#"{ driveDocuments { id state { nodes { ... on DocumentDrive_FileNode { id name kind documentType } } } } }"#,
            None,
        )
        .await
    {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let drives = data
        .get("driveDocuments")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut docs = Vec::new();
    for drv in &drives {
        let nodes = drv
            .pointer("/state/nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if nodes.is_empty() {
            let drive_id = drv["id"].as_str().unwrap_or("");
            if !drive_id.is_empty() {
                let q = format!(
                    r#"{{ driveDocument(idOrSlug: "{drive_id}") {{ state {{ nodes {{ ... on DocumentDrive_FileNode {{ id name kind documentType }} }} }} }} }}"#
                );
                if let Ok(d) = client.query(&q, None).await
                    && let Some(n) = d
                        .pointer("/driveDocument/state/nodes")
                        .and_then(|v| v.as_array())
                {
                    for node in n {
                        if node["kind"].as_str() == Some("file") {
                            docs.push(DocEntry {
                                id: node["id"].as_str().unwrap_or("").to_string(),
                                name: node["name"].as_str().unwrap_or("").to_string(),
                                doc_type: node["documentType"].as_str().unwrap_or("").to_string(),
                            });
                        }
                    }
                }
            }
        } else {
            for node in &nodes {
                if node["kind"].as_str() == Some("file") {
                    docs.push(DocEntry {
                        id: node["id"].as_str().unwrap_or("").to_string(),
                        name: node["name"].as_str().unwrap_or("").to_string(),
                        doc_type: node["documentType"].as_str().unwrap_or("").to_string(),
                    });
                }
            }
        }
    }

    docs
}

// ── REPL entry point ────────────────────────────────────────────────────────

pub async fn run(profile_name: Option<&str>, quiet: bool) -> Result<()> {
    let (name, _profile, mut client) = helpers::setup(profile_name)?;

    // Load introspection cache for context
    let cache = crate::graphql::introspection::load_cache(&name)?;
    let model_count = cache.as_ref().map(|c| c.models.len()).unwrap_or(0);

    // Collect model types for tab completion
    let model_types: Vec<String> = cache
        .as_ref()
        .map(|c| c.models.values().map(|m| m.document_type.clone()).collect())
        .unwrap_or_default();

    // Fetch completion data with a loading indicator
    let spinner = spawn_spinner("Loading...");

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

    // Fetch document entries for tab completion
    let doc_entries = fetch_doc_entries(&client).await;

    stop_spinner(spinner);

    // Fetch profile names for tab completion
    let profile_names: Vec<String> = crate::config::load_config()
        .map(|cfg| cfg.profile_names())
        .unwrap_or_default();

    if !quiet {
        eprintln!("Switchboard interactive mode");
        eprintln!("Profile: {} ({})", name, client.url);
        eprintln!("Models:  {model_count}");
        eprintln!();
        eprintln!("Type 'help' for commands, or press Tab for auto-completion.");
        eprintln!();
    }

    // Set up rustyline with history and completion
    let config = Config::builder()
        .max_history_size(1000)?
        .auto_add_history(true)
        .completion_type(CompletionType::Circular)
        .build();

    let helper = ReplHelper::new(drive_slugs, model_types, profile_names, doc_entries);
    let mut rl: Editor<ReplHelper, rustyline::history::DefaultHistory> =
        Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    // Load history from ~/.switchboard/history
    let history_path = dirs::home_dir().map(|h| h.join(".switchboard").join("history"));
    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    }

    let mut current_profile = name;

    loop {
        let prompt = format!("{current_profile}> ");
        show_cursor();
        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // ── REPL-only commands ──────────────────────────────
                match line {
                    "exit" | "quit" | "q" => break,
                    _ => {}
                }

                // Visual separator so command output is easy to spot
                print_command_separator(line);

                if matches!(line, "help" | "?") {
                    print_repl_help();
                    continue;
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

                        // Check if this command modifies docs (for refreshing completions)
                        let modifies_docs = line.starts_with("docs create")
                            || line.starts_with("docs delete")
                            || line.starts_with("docs mutate")
                            || line.starts_with("import ");

                        if let Err(e) =
                            crate::cli::dispatch(command, format, cmd_profile, cmd_quiet).await
                        {
                            eprintln!("Error: {e:#}");
                        }

                        // Refresh doc completions after doc-modifying commands
                        if modifies_docs {
                            let spinner = spawn_spinner("Refreshing completions...");
                            let new_docs = fetch_doc_entries(&client).await;
                            stop_spinner(spinner);
                            if let Some(helper) = rl.helper_mut() {
                                helper.update_docs(new_docs);
                            }
                        }

                        // Re-resolve default profile in case `config use` changed it
                        if profile_name.is_none()
                            && let Ok(cfg) = crate::config::load_config()
                            && let Some((new_name, _)) = cfg.default_profile()
                            && new_name != current_profile.as_str()
                        {
                            current_profile = new_name.to_string();

                            // Rebuild client and refresh completions for new profile
                            if let Ok((_n, _p, new_client)) = helpers::setup(None) {
                                eprintln!(
                                    "Switched to profile: {} ({})",
                                    current_profile, new_client.url
                                );
                                client = new_client;

                                let spinner = spawn_spinner("Loading profile data...");

                                let new_slugs: Vec<String> =
                                    match client.query("{ driveDocuments { slug } }", None).await {
                                        Ok(data) => data
                                            .get("driveDocuments")
                                            .and_then(|v| v.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                    .filter_map(|d| {
                                                        d["slug"].as_str().map(String::from)
                                                    })
                                                    .collect()
                                            })
                                            .unwrap_or_default(),
                                        Err(_) => Vec::new(),
                                    };

                                let new_docs = fetch_doc_entries(&client).await;

                                let new_model_types: Vec<String> =
                                    crate::graphql::introspection::load_cache(&current_profile)
                                        .ok()
                                        .flatten()
                                        .map(|c| {
                                            c.models
                                                .values()
                                                .map(|m| m.document_type.clone())
                                                .collect()
                                        })
                                        .unwrap_or_default();

                                stop_spinner(spinner);

                                if let Some(helper) = rl.helper_mut() {
                                    helper.drive_slugs = new_slugs;
                                    helper.model_types = new_model_types;
                                    helper.update_docs(new_docs);
                                }
                            }
                        }

                        eprintln!(); // blank line between command output and next prompt
                    }
                    Err(e) => {
                        // Try interpreting as a bare guide topic
                        // (e.g., "overview" → "guide overview")
                        let guide_args = std::iter::once("switchboard".to_string())
                            .chain(std::iter::once("guide".to_string()))
                            .chain(shell_split(line));
                        if let Ok(parsed) = Cli::try_parse_from(guide_args)
                            && let Some(command) = parsed.command
                        {
                            if let Err(ge) = crate::cli::dispatch(
                                command,
                                OutputFormat::Table,
                                profile_name,
                                quiet,
                            )
                            .await
                            {
                                eprintln!("Error: {ge:#}");
                            }
                            eprintln!();
                        } else {
                            let _ = e.print();
                            eprintln!();
                        }
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
    eprintln!("    export   all | drive | doc");
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
