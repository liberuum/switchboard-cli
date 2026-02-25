use anyhow::Result;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Config, Editor, Helper};

use crate::cli::helpers;
use crate::output::{print_json, print_table};

/// Tab-completion helper for the REPL.
/// Knows about commands, drive slugs, and model types from the introspection cache.
struct ReplHelper {
    /// Static command prefixes for first-level completion
    commands: Vec<String>,
    /// Drive slugs fetched at startup (for `docs list <drive>`, etc.)
    drive_slugs: Vec<String>,
    /// Document model types from introspection cache
    model_types: Vec<String>,
}

impl ReplHelper {
    fn new(drive_slugs: Vec<String>, model_types: Vec<String>) -> Self {
        let commands = vec![
            "drives list".into(),
            "docs list ".into(),
            "docs get ".into(),
            "docs tree ".into(),
            "docs create".into(),
            "models list".into(),
            "query ".into(),
            "help".into(),
            "exit".into(),
            "quit".into(),
        ];
        Self {
            commands,
            drive_slugs,
            model_types,
        }
    }
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

        // If we're completing after "docs list ", "docs get ", "docs tree " → suggest drive slugs
        if input.starts_with("docs list ")
            || input.starts_with("docs get ")
            || input.starts_with("docs tree ")
        {
            let prefix_end = input.find(' ').unwrap_or(0);
            let after_cmd = &input[prefix_end..].trim_start();
            // Find the last space to get the current word being typed
            let parts: Vec<&str> = after_cmd.split_whitespace().collect();
            // For "docs list <partial>" → complete drive slug
            // For "docs get <drive> <partial>" → we don't complete doc IDs (too many)
            if (input.starts_with("docs list ") && parts.len() <= 1)
                || ((input.starts_with("docs get ") || input.starts_with("docs tree "))
                    && parts.len() <= 1)
            {
                let word_start = input.rfind(' ').map(|i| i + 1).unwrap_or(0);
                let partial = &input[word_start..];
                let matches: Vec<Pair> = self
                    .drive_slugs
                    .iter()
                    .filter(|s| s.starts_with(partial))
                    .map(|s| Pair {
                        display: s.clone(),
                        replacement: s.clone(),
                    })
                    .collect();
                return Ok((word_start, matches));
            }
        }

        // If we're completing after "docs create --type " → suggest model types
        if input.starts_with("docs create --type ") || input.starts_with("query ") {
            // For "query" there's nothing useful to complete
            if input.starts_with("query ") {
                return Ok((pos, vec![]));
            }
            let word_start = input.rfind(' ').map(|i| i + 1).unwrap_or(0);
            let partial = &input[word_start..];
            let matches: Vec<Pair> = self
                .model_types
                .iter()
                .filter(|t| t.starts_with(partial))
                .map(|t| Pair {
                    display: t.clone(),
                    replacement: t.clone(),
                })
                .collect();
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
        eprintln!("Commands:");
        eprintln!("  drives list              List all drives");
        eprintln!("  docs list <drive>        List documents in a drive");
        eprintln!("  docs get <drive> <id>    Get a document");
        eprintln!("  docs tree <drive>        Show hierarchical tree view");
        eprintln!("  models list              List document models");
        eprintln!("  query <graphql>          Run a raw GraphQL query");
        eprintln!("  help                     Show available commands");
        eprintln!("  exit / quit              Exit the REPL");
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

                let parts: Vec<&str> = line.splitn(4, ' ').collect();

                match parts.as_slice() {
                    ["exit"] | ["quit"] | ["q"] => break,
                    ["help"] | ["?"] => print_repl_help(),
                    ["drives", "list"] => {
                        match client
                            .query(
                                "{ driveDocuments { id name slug documentType revision } }",
                                None,
                            )
                            .await
                        {
                            Ok(data) => {
                                let drives = data
                                    .get("driveDocuments")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                if drives.is_empty() {
                                    println!("No drives found.");
                                } else {
                                    let rows: Vec<Vec<String>> = drives
                                        .iter()
                                        .map(|d| {
                                            vec![
                                                d["id"].as_str().unwrap_or("-").to_string(),
                                                d["name"].as_str().unwrap_or("-").to_string(),
                                                d["slug"].as_str().unwrap_or("-").to_string(),
                                            ]
                                        })
                                        .collect();
                                    print_table(&["ID", "Name", "Slug"], &rows);
                                }
                            }
                            Err(e) => eprintln!("Error: {e:#}"),
                        }
                    }
                    ["docs", "list", drive] => {
                        let query = format!(
                            r#"{{
                                driveDocument(idOrSlug: "{drive}") {{
                                    state {{
                                        nodes {{
                                            ... on DocumentDrive_FileNode {{ id name kind documentType }}
                                            ... on DocumentDrive_FolderNode {{ id name kind }}
                                        }}
                                    }}
                                }}
                            }}"#,
                            drive = drive.replace('"', r#"\""#)
                        );
                        match client.query(&query, None).await {
                            Ok(data) => {
                                let nodes = data
                                    .pointer("/driveDocument/state/nodes")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                let files: Vec<_> = nodes
                                    .iter()
                                    .filter(|n| n["kind"].as_str() == Some("file"))
                                    .collect();
                                if files.is_empty() {
                                    println!("No documents in drive '{drive}'.");
                                } else {
                                    let rows: Vec<Vec<String>> = files
                                        .iter()
                                        .map(|d| {
                                            vec![
                                                d["id"].as_str().unwrap_or("-").to_string(),
                                                d["name"].as_str().unwrap_or("-").to_string(),
                                                d["documentType"]
                                                    .as_str()
                                                    .unwrap_or("-")
                                                    .to_string(),
                                            ]
                                        })
                                        .collect();
                                    print_table(&["ID", "Name", "Type"], &rows);
                                }
                            }
                            Err(e) => eprintln!("Error: {e:#}"),
                        }
                    }
                    ["docs", "tree", drive] => {
                        let query = format!(
                            r#"{{
                                driveDocument(idOrSlug: "{drive}") {{
                                    name
                                    state {{
                                        nodes {{
                                            ... on DocumentDrive_FileNode {{ id name kind documentType parentFolder }}
                                            ... on DocumentDrive_FolderNode {{ id name kind parentFolder }}
                                        }}
                                    }}
                                }}
                            }}"#,
                            drive = drive.replace('"', r#"\""#)
                        );
                        match client.query(&query, None).await {
                            Ok(data) => {
                                let drive_name = data
                                    .pointer("/driveDocument/name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(drive);
                                let nodes = data
                                    .pointer("/driveDocument/state/nodes")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                println!("{drive_name}/");
                                crate::cli::docs::print_tree(&nodes, None, "");
                            }
                            Err(e) => eprintln!("Error: {e:#}"),
                        }
                    }
                    ["docs", "get", drive, doc_id] => {
                        let cache = crate::graphql::introspection::load_cache(&name);
                        let mut found = false;
                        if let Ok(Some(ref cache)) = cache {
                            let drive_id = match helpers::resolve_drive_id(&client, drive).await {
                                Ok(id) => id,
                                Err(e) => {
                                    eprintln!("Error resolving drive: {e:#}");
                                    continue;
                                }
                            };
                            for model in cache.models.values() {
                                if !model.query_fields.contains(&"getDocument".to_string()) {
                                    continue;
                                }
                                let query = format!(
                                    r#"{{ {prefix} {{ getDocument(docId: "{doc_id}", driveId: "{drive_id}") {{ id name documentType revision stateJSON }} }} }}"#,
                                    prefix = model.prefix,
                                    doc_id = doc_id.replace('"', r#"\""#),
                                );
                                if let Ok(data) = client.query(&query, None).await
                                    && let Some(doc) = data
                                        .get(&model.prefix)
                                        .and_then(|v| v.get("getDocument"))
                                        .filter(|doc| !doc.is_null())
                                {
                                    print_json(doc);
                                    found = true;
                                    break;
                                }
                            }
                        }
                        if !found {
                            eprintln!(
                                "Document not found. Run `switchboard introspect` if cache is stale."
                            );
                        }
                    }
                    ["models", "list"] => {
                        let cache = crate::graphql::introspection::load_cache(&name);
                        match cache {
                            Ok(Some(ref c)) => {
                                let rows: Vec<Vec<String>> = c
                                    .models
                                    .values()
                                    .map(|m| {
                                        vec![
                                            m.document_type.clone(),
                                            m.prefix.clone(),
                                            m.operations.len().saturating_sub(1).to_string(),
                                        ]
                                    })
                                    .collect();
                                print_table(&["Type", "Prefix", "Operations"], &rows);
                            }
                            _ => eprintln!(
                                "No introspection cache. Run `switchboard introspect` first."
                            ),
                        }
                    }
                    _ if parts.first() == Some(&"query") => {
                        let raw = line.strip_prefix("query").unwrap().trim();
                        if raw.is_empty() {
                            eprintln!("Usage: query {{ your GraphQL query }}");
                            continue;
                        }
                        match client.query(raw, None).await {
                            Ok(data) => print_json(&data),
                            Err(e) => eprintln!("Error: {e:#}"),
                        }
                    }
                    _ => {
                        eprintln!("Unknown command: '{line}'. Type 'help' for available commands.");
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

fn print_repl_help() {
    eprintln!("Available commands:");
    eprintln!();
    eprintln!("  drives list                   List all drives");
    eprintln!("  docs list <drive>             List documents in a drive");
    eprintln!("  docs get <drive> <doc-id>     Get document details (JSON)");
    eprintln!("  docs tree <drive>             Show hierarchical tree view");
    eprintln!("  models list                   List document models from cache");
    eprintln!("  query <graphql>               Run a raw GraphQL query");
    eprintln!("  help                          Show this help");
    eprintln!("  exit / quit / q               Exit interactive mode");
}
