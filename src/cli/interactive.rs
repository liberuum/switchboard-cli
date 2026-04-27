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
    /// Folder names across all drives, fetched at startup
    folder_names: Vec<String>,
    /// Drive slug each folder lives in (parallel to folder_names)
    folder_drive_slugs: Vec<String>,
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
    /// Drive slug for each doc entry (parallel to doc_ids/doc_labels)
    doc_drive_slugs: Vec<String>,
}

/// A folder entry for tab-completion.
struct FolderEntry {
    name: String,
    drive_slug: String,
}

/// A document entry for tab-completion.
struct DocEntry {
    id: String,
    name: String,
    doc_type: String,
    drive_slug: String,
}

impl ReplHelper {
    fn new(
        drive_slugs: Vec<String>,
        model_types: Vec<String>,
        profile_names: Vec<String>,
        docs: Vec<DocEntry>,
        folders: Vec<FolderEntry>,
    ) -> Self {
        let (doc_ids, doc_labels, doc_drive_slugs) = Self::build_doc_completions(&docs);
        let (folder_names, folder_drive_slugs) = Self::build_folder_completions(&folders);

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
            "docs tree".into(),
            "docs tree ".into(),
            "docs create".into(),
            "docs delete ".into(),
            "docs rename ".into(),
            "docs parents ".into(),
            "docs add-to ".into(),
            "docs remove-from ".into(),
            "docs move --from ".into(),
            "docs mutate ".into(),
            // Folders
            "folders create".into(),
            "folders create --name ".into(),
            "folders create --parent ".into(),
            "folders create --drive ".into(),
            "folders delete ".into(),
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
            // Visualize
            "visualize".into(),
            "visualize --format json".into(),
            "visualize --format svg --out ".into(),
            "visualize --format png --out ".into(),
            "visualize --format mermaid".into(),
            // Analytics
            "analytics metrics".into(),
            "analytics dimensions".into(),
            "analytics currencies".into(),
            "analytics series".into(),
            "analytics series --start ".into(),
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
            "watch".into(),
            "jobs".into(),
            "sync".into(),
            "interactive".into(),
            "output".into(),
            "visualize".into(),
            "graphql".into(),
            "commands".into(),
        ];

        Self {
            commands,
            drive_slugs,
            folder_names,
            folder_drive_slugs,
            model_types,
            guide_topics,
            profile_names,
            doc_ids,
            doc_labels,
            doc_drive_slugs,
        }
    }

    fn build_folder_completions(folders: &[FolderEntry]) -> (Vec<String>, Vec<String>) {
        let names = folders
            .iter()
            .map(|f| {
                if f.name.contains(' ') {
                    format!("\"{}\"", f.name)
                } else {
                    f.name.clone()
                }
            })
            .collect();
        let drives = folders.iter().map(|f| f.drive_slug.clone()).collect();
        (names, drives)
    }

    fn update_folders(&mut self, folders: Vec<FolderEntry>) {
        let (names, drives) = Self::build_folder_completions(&folders);
        self.folder_names = names;
        self.folder_drive_slugs = drives;
    }

    fn build_doc_completions(docs: &[DocEntry]) -> (Vec<String>, Vec<String>, Vec<String>) {
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
        // drive slugs: which drive each doc belongs to
        let drive_slugs: Vec<String> = docs.iter().map(|d| d.drive_slug.clone()).collect();
        (replacements, labels, drive_slugs)
    }

    fn update_docs(&mut self, docs: Vec<DocEntry>) {
        let (replacements, labels, drive_slugs) = Self::build_doc_completions(&docs);
        self.doc_ids = replacements;
        self.doc_labels = labels;
        self.doc_drive_slugs = drive_slugs;
    }
}

/// Check whether a positional (non-flag) argument has already been consumed.
/// Skips the first `cmd_prefix_len` words (the command itself, e.g. "docs get"),
/// then skips `--flag value` pairs and standalone `--flag` boolean flags.
fn has_positional_arg(words: &[&str], cmd_prefix_len: usize) -> bool {
    let args = if words.len() > cmd_prefix_len {
        &words[cmd_prefix_len..]
    } else {
        return false;
    };
    let value_flags = ["--drive", "--format", "--profile", "-p", "--type", "-t"];
    let mut skip_next = false;
    for w in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if value_flags.contains(w) {
            skip_next = true;
            continue;
        }
        if w.starts_with('-') {
            continue; // boolean flag like --state, --yes, -y
        }
        return true; // non-flag word → positional arg found
    }
    false
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

/// Hierarchical drive/doc completion.
/// Before `/`: shows `drive-slug/` entries plus flat doc matches.
/// After `/`: shows only docs inside the matched drive as `drive/doc`.
fn hierarchical_doc_pairs(
    drive_slugs: &[String],
    doc_ids: &[String],
    doc_labels: &[String],
    doc_drive_slugs: &[String],
    partial: &str,
) -> Vec<Pair> {
    if let Some(slash_pos) = partial.find('/') {
        // After "/": filter docs belonging to this drive
        let drive_part = &partial[..slash_pos];
        let doc_part = partial[slash_pos + 1..].to_lowercase();
        doc_ids
            .iter()
            .zip(doc_labels.iter())
            .zip(doc_drive_slugs.iter())
            .filter(|((_id, label), ds)| {
                ds.eq_ignore_ascii_case(drive_part)
                    && (doc_part.is_empty() || label.to_lowercase().contains(doc_part.as_str()))
            })
            .map(|((id, _label), ds)| Pair {
                display: id.clone(),
                replacement: format!("{ds}/{id}"),
            })
            .collect()
    } else {
        // Before "/": show drive slugs with trailing "/" plus regular doc matches
        let partial_lower = partial.to_lowercase();
        let mut matches: Vec<Pair> = drive_slugs
            .iter()
            .filter(|s| s.to_lowercase().starts_with(&partial_lower))
            .map(|s| Pair {
                display: format!("{s}/"),
                replacement: format!("{s}/"),
            })
            .collect();
        matches.extend(filter_doc_pairs(doc_ids, doc_labels, partial));
        matches
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
        let word_start = input.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let partial = &input[word_start..];
        let words_before: Vec<&str> = input[..word_start].split_whitespace().collect();
        let prev_word = words_before.last().copied();

        // ── Drive slug completion ────────────────────────────
        if prev_word == Some("--drive")
            || input.starts_with("drives get ")
            || input.starts_with("drives delete ")
            || input.starts_with("export drive ")
            || input.starts_with("docs tree ")
        {
            let matches = filter_pairs(&self.drive_slugs, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // ── Parent completion (drives + folders, labeled) ────
        // `--parent` is universal: it accepts a drive (root placement) or a
        // folder (nested placement). Surface both, with display labels so the
        // user can tell them apart.
        if prev_word == Some("--parent") {
            let mut matches: Vec<Pair> = self
                .drive_slugs
                .iter()
                .filter(|s| s.to_lowercase().starts_with(&partial.to_lowercase()))
                .map(|s| Pair {
                    display: format!("{s}    (drive)"),
                    replacement: s.clone(),
                })
                .collect();
            matches.extend(
                self.folder_names
                    .iter()
                    .zip(self.folder_drive_slugs.iter())
                    .filter(|(name, _)| {
                        let trim = name.trim_matches('"');
                        trim.to_lowercase().starts_with(&partial.to_lowercase())
                    })
                    .map(|(name, drive)| Pair {
                        display: format!("{name}    (folder in {drive})"),
                        replacement: name.clone(),
                    }),
            );
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // ── Folder-only completion ───────────────────────────
        // `--folder` is the strict-folder spelling; show only folders.
        if prev_word == Some("--folder") {
            let matches: Vec<Pair> = self
                .folder_names
                .iter()
                .zip(self.folder_drive_slugs.iter())
                .filter(|(name, _)| {
                    let trim = name.trim_matches('"');
                    trim.to_lowercase().starts_with(&partial.to_lowercase())
                })
                .map(|(name, drive)| Pair {
                    display: format!("{name}    (folder in {drive})"),
                    replacement: name.clone(),
                })
                .collect();
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // ── Document ID/name completion ──────────────────────
        // After commands that take a doc ID as a positional arg.
        // If --drive <slug> is present, scope completions to that drive.
        // Once a doc is selected, offer remaining flags instead.
        if input.starts_with("docs get ")
            || input.starts_with("docs delete ")
            || input.starts_with("docs mutate ")
            || input.starts_with("export doc ")
        {
            let drive_filter = words_before
                .windows(2)
                .find(|w| w[0] == "--drive")
                .map(|w| w[1]);
            let doc_selected = has_positional_arg(&words_before, 2);

            if !doc_selected {
                // Still need a doc — offer completions
                if let Some(slug) = drive_filter {
                    let matches: Vec<Pair> = self
                        .doc_ids
                        .iter()
                        .zip(self.doc_labels.iter())
                        .zip(self.doc_drive_slugs.iter())
                        .filter(|((_id, label), ds)| {
                            ds.eq_ignore_ascii_case(slug)
                                && (partial.is_empty()
                                    || label.to_lowercase().contains(&partial.to_lowercase()))
                        })
                        .map(|((id, _label), _ds)| Pair {
                            display: id.clone(),
                            replacement: id.clone(),
                        })
                        .collect();
                    if !matches.is_empty() {
                        return Ok((word_start, matches));
                    }
                } else {
                    let matches = hierarchical_doc_pairs(
                        &self.drive_slugs,
                        &self.doc_ids,
                        &self.doc_labels,
                        &self.doc_drive_slugs,
                        partial,
                    );
                    if !matches.is_empty() {
                        return Ok((word_start, matches));
                    }
                }
            } else {
                // Doc already selected — offer remaining flags
                let flags: &[&str] = if input.starts_with("docs get ") {
                    &["--state", "--drive", "--format"]
                } else if input.starts_with("docs delete ") {
                    &["-y", "--format"]
                } else if input.starts_with("docs mutate ") {
                    &["--drive", "--format"]
                } else if input.starts_with("export doc ") {
                    &["--out", "--drive", "--format"]
                } else {
                    &["--format"]
                };
                let matches: Vec<Pair> = flags
                    .iter()
                    .filter(|f| {
                        !input.contains(**f) && (partial.is_empty() || f.starts_with(partial))
                    })
                    .map(|f| Pair {
                        display: f.to_string(),
                        replacement: format!("{f} "),
                    })
                    .collect();
                if !matches.is_empty() {
                    return Ok((word_start, matches));
                }
            }
        }
        // ops takes doc ID as first arg — supports hierarchical drive/doc completion
        if input.starts_with("ops ") && words_before.len() <= 1 {
            let matches = hierarchical_doc_pairs(
                &self.drive_slugs,
                &self.doc_ids,
                &self.doc_labels,
                &self.doc_drive_slugs,
                partial,
            );
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

// ── Drive/doc-fetching for tab completion ────────────────────────────────────

async fn fetch_drive_slugs(client: &crate::graphql::GraphQLClient) -> Vec<String> {
    match client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { name slug state } } }"#,
            None,
        )
        .await
    {
        Ok(data) => data
            .pointer("/findDocuments/items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                let mut slugs: Vec<String> = Vec::new();
                for d in arr.iter().filter(|d| {
                    d.pointer("/state/document/isDeleted")
                        .and_then(|v| v.as_bool())
                        != Some(true)
                }) {
                    if let Some(slug) = d["slug"].as_str() {
                        slugs.push(slug.to_string());
                    }
                    // Also add the drive name so users can tab-complete by name
                    if let Some(name) = d["name"].as_str()
                        && !name.is_empty()
                        && d["slug"].as_str() != Some(name)
                    {
                        slugs.push(name.to_string());
                    }
                }
                slugs
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Fetch folder entries from every (non-deleted) drive's state.global.nodes.
/// Used for tab completion of `--parent` / `--folder`.
async fn fetch_folder_entries(client: &crate::graphql::GraphQLClient) -> Vec<FolderEntry> {
    let data = match client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { slug state } } }"#,
            None,
        )
        .await
    {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let drives = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut folders = Vec::new();
    for d in drives.iter().filter(|d| {
        d.pointer("/state/document/isDeleted")
            .and_then(|v| v.as_bool())
            != Some(true)
    }) {
        let drive_slug = d["slug"].as_str().unwrap_or("").to_string();
        let nodes = match d.pointer("/state/global/nodes").and_then(|v| v.as_array()) {
            Some(n) => n,
            None => continue,
        };
        for n in nodes {
            if n["kind"].as_str() != Some("folder") {
                continue;
            }
            if let Some(name) = n["name"].as_str()
                && !name.is_empty()
            {
                folders.push(FolderEntry {
                    name: name.to_string(),
                    drive_slug: drive_slug.clone(),
                });
            }
        }
    }
    folders
}

async fn fetch_doc_entries(client: &crate::graphql::GraphQLClient) -> Vec<DocEntry> {
    let data = match client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id slug state } } }"#,
            None,
        )
        .await
    {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let drives: Vec<_> = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|d| {
                    d.pointer("/state/document/isDeleted")
                        .and_then(|v| v.as_bool())
                        != Some(true)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let mut docs = Vec::new();
    for drv in &drives {
        let drv_slug = drv["slug"].as_str().unwrap_or("").to_string();
        let drv_id = drv["id"].as_str().unwrap_or("");
        if drv_id.is_empty() {
            continue;
        }

        let children_query = format!(
            r#"{{ documentChildren(parentIdentifier: "{drv_id}") {{ items {{ id name documentType }} }} }}"#
        );
        if let Ok(cd) = client.query(&children_query, None).await
            && let Some(items) = cd
                .pointer("/documentChildren/items")
                .and_then(|v| v.as_array())
        {
            for node in items {
                docs.push(DocEntry {
                    id: node["id"].as_str().unwrap_or("").to_string(),
                    name: node["name"].as_str().unwrap_or("").to_string(),
                    doc_type: node["documentType"].as_str().unwrap_or("").to_string(),
                    drive_slug: drv_slug.clone(),
                });
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
    let drive_slugs = fetch_drive_slugs(&client).await;

    // Fetch document entries for tab completion
    let doc_entries = fetch_doc_entries(&client).await;

    // Fetch folder entries for tab completion
    let folder_entries = fetch_folder_entries(&client).await;

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
        eprintln!("Type 'help' for commands, press Tab for auto-completion.");
        eprintln!(
            "Tip: ops [Tab] shows drives and docs. Use drive/[Tab] to browse inside a drive."
        );
        eprintln!();
    }

    // Set up rustyline with history and completion.
    // CompletionType::List shows ALL candidates at once, which is what users
    // typically want — Circular cycles through them one tab at a time and
    // hides matches behind extra keypresses.
    let config = Config::builder()
        .max_history_size(1000)?
        .auto_add_history(true)
        .completion_type(CompletionType::List)
        .build();

    let helper = ReplHelper::new(
        drive_slugs,
        model_types,
        profile_names,
        doc_entries,
        folder_entries,
    );
    let mut rl: Editor<ReplHelper, rustyline::history::DefaultHistory> =
        Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    // Load history from ~/.switchboard/history
    let history_path = dirs::home_dir().map(|h| h.join(".switchboard").join("history"));
    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    }

    let mut current_profile = name;

    // Track when we last refreshed completion caches so we can transparently
    // pick up changes made outside the REPL (e.g. a drive created from another
    // terminal session) without forcing the user to type `refresh`.
    let mut last_completion_refresh = std::time::Instant::now();
    const COMPLETION_TTL_SECS: u64 = 5;

    loop {
        // Auto-refresh completions if stale. Cheap (a couple of GraphQL queries)
        // and only runs at the prompt boundary, never mid-tab, so latency is hidden.
        if last_completion_refresh.elapsed().as_secs() >= COMPLETION_TTL_SECS {
            let new_slugs = fetch_drive_slugs(&client).await;
            let new_docs = fetch_doc_entries(&client).await;
            let new_folders = fetch_folder_entries(&client).await;
            if let Some(helper) = rl.helper_mut() {
                if !new_slugs.is_empty() {
                    helper.drive_slugs = new_slugs;
                }
                if !new_docs.is_empty() {
                    helper.update_docs(new_docs);
                }
                helper.update_folders(new_folders);
            }
            last_completion_refresh = std::time::Instant::now();
        }

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

                // ── Manual refresh ────────────────────────────────────
                if line.trim() == "refresh" {
                    let spinner = spawn_spinner("Refreshing completions...");
                    let new_slugs = fetch_drive_slugs(&client).await;
                    let new_docs = fetch_doc_entries(&client).await;
                    let new_folders = fetch_folder_entries(&client).await;
                    let new_model_types: Vec<String> =
                        crate::graphql::introspection::load_cache(&current_profile)
                            .ok()
                            .flatten()
                            .map(|c| c.models.values().map(|m| m.document_type.clone()).collect())
                            .unwrap_or_default();
                    stop_spinner(spinner);
                    if let Some(helper) = rl.helper_mut() {
                        if !new_slugs.is_empty() {
                            helper.drive_slugs = new_slugs;
                        }
                        if !new_docs.is_empty() {
                            helper.update_docs(new_docs);
                        }
                        helper.update_folders(new_folders);
                        if !new_model_types.is_empty() {
                            helper.model_types = new_model_types;
                        }
                    }
                    last_completion_refresh = std::time::Instant::now();
                    eprintln!("Completions refreshed.");
                    continue;
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

                        // Check which completion caches need refreshing after this command
                        let modifies_drives =
                            line.starts_with("drives create") || line.starts_with("drives delete");
                        let modifies_docs = line.starts_with("docs create")
                            || line.starts_with("docs delete")
                            || line.starts_with("docs rename")
                            || line.starts_with("docs add-to")
                            || line.starts_with("docs remove-from")
                            || line.starts_with("docs move")
                            || line.starts_with("docs mutate")
                            || line.starts_with("docs apply")
                            || line.starts_with("import ");
                        // Folders live in drive state and are also affected by
                        // drive-modifying commands (create/delete cascade) and
                        // by docs apply (raw ADD_FOLDER / DELETE_NODE actions).
                        let modifies_folders = line.starts_with("folders create")
                            || line.starts_with("folders delete")
                            || modifies_drives
                            || line.starts_with("docs apply");
                        if let Err(e) =
                            crate::cli::dispatch(command, format, cmd_profile, cmd_quiet).await
                        {
                            eprintln!("Error: {e:#}");
                        }

                        // Refresh completion caches after modifying commands
                        if modifies_drives || modifies_docs || modifies_folders {
                            let spinner = spawn_spinner("Refreshing completions...");
                            let new_docs = fetch_doc_entries(&client).await;
                            let new_slugs = if modifies_drives {
                                fetch_drive_slugs(&client).await
                            } else {
                                Vec::new() // no change needed
                            };
                            let new_folders = if modifies_folders {
                                Some(fetch_folder_entries(&client).await)
                            } else {
                                None
                            };
                            stop_spinner(spinner);
                            if let Some(helper) = rl.helper_mut() {
                                // Only replace drive slugs if we got results back — don't
                                // wipe the existing list on a transient fetch failure.
                                if modifies_drives && !new_slugs.is_empty() {
                                    helper.drive_slugs = new_slugs;
                                }
                                if !new_docs.is_empty() {
                                    helper.update_docs(new_docs);
                                }
                                if let Some(folders) = new_folders {
                                    helper.update_folders(folders);
                                }
                            }
                            last_completion_refresh = std::time::Instant::now();
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

                                let new_slugs = fetch_drive_slugs(&client).await;

                                let new_docs = fetch_doc_entries(&client).await;

                                let new_folders = fetch_folder_entries(&client).await;

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
                                    helper.update_folders(new_folders);
                                }
                                last_completion_refresh = std::time::Instant::now();
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
    eprintln!("    folders  create | delete");
    eprintln!("    models   list | get");
    eprintln!("    ops      <doc-id> --drive <drive>");
    eprintln!();
    eprintln!("  Configuration:");
    eprintln!("    config   list | show | use | remove");
    eprintln!("    auth     login | logout | status | token");
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
    eprintln!("    refresh          Reload tab-completion caches (drives, docs, models)");
    eprintln!("    exit | quit | q  Exit interactive mode");
    eprintln!();
    eprintln!("  Tip: Append --help to any command for details.");
}

#[cfg(test)]
mod tests {
    use super::{FolderEntry, ReplHelper, shell_split};
    use rustyline::completion::Completer;
    use rustyline::history::DefaultHistory;

    /// Build a helper with fixed completion data for tests.
    fn helper_with_drives(drives: &[&str]) -> ReplHelper {
        ReplHelper::new(
            drives.iter().map(|s| s.to_string()).collect(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    /// Build a helper with both drive and folder data for completion tests.
    fn helper_with_drives_and_folders(
        drives: &[&str],
        folders: &[(&str, &str)], // (folder_name, drive_slug)
    ) -> ReplHelper {
        ReplHelper::new(
            drives.iter().map(|s| s.to_string()).collect(),
            vec![],
            vec![],
            vec![],
            folders
                .iter()
                .map(|(name, slug)| FolderEntry {
                    name: (*name).to_string(),
                    drive_slug: (*slug).to_string(),
                })
                .collect(),
        )
    }

    /// Run the completer against a line and return the candidate replacements
    /// (drops the start-position part of the result tuple).
    fn complete(helper: &ReplHelper, line: &str) -> Vec<String> {
        let history = DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        let (_, pairs) = helper.complete(line, line.len(), &ctx).unwrap();
        pairs.into_iter().map(|p| p.replacement).collect()
    }

    /// Run the completer and return the *display* strings (what the user sees
    /// in the candidate list). Useful for testing labels like "(drive)" vs
    /// "(folder)".
    fn complete_display(helper: &ReplHelper, line: &str) -> Vec<String> {
        let history = DefaultHistory::new();
        let ctx = rustyline::Context::new(&history);
        let (_, pairs) = helper.complete(line, line.len(), &ctx).unwrap();
        pairs.into_iter().map(|p| p.display).collect()
    }

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

    // ── Completion tests ────────────────────────────────────────────────────
    //
    // Regression tests for the user-reported bug where typing
    // `drives get <TAB>` only surfaced a subset of available drives.

    #[test]
    fn drives_get_lists_every_drive() {
        let helper = helper_with_drives(&["my-builder-team-admin", "vetra-f80015b9", "Vetra"]);
        let matches = complete(&helper, "drives get ");
        assert_eq!(
            matches,
            vec!["my-builder-team-admin", "vetra-f80015b9", "Vetra"],
            "drives get <TAB> must surface every cached drive slug"
        );
    }

    #[test]
    fn drives_get_filters_by_partial_prefix() {
        let helper = helper_with_drives(&["my-builder-team-admin", "vetra-f80015b9", "Vetra"]);
        let matches = complete(&helper, "drives get my");
        assert_eq!(
            matches,
            vec!["my-builder-team-admin"],
            "partial prefix should narrow the candidate set"
        );
    }

    #[test]
    fn drives_delete_uses_drive_completion_too() {
        let helper = helper_with_drives(&["a", "b", "c"]);
        let matches = complete(&helper, "drives delete ");
        assert_eq!(matches, vec!["a", "b", "c"]);
    }

    #[test]
    fn bare_folders_lists_subcommands() {
        let helper = helper_with_drives(&[]);
        let matches = complete(&helper, "folders ");
        assert!(
            matches.iter().any(|m| m == "folders create"),
            "expected 'folders create' in matches, got {matches:?}"
        );
        assert!(
            matches.iter().any(|m| m == "folders delete "),
            "expected 'folders delete ' in matches, got {matches:?}"
        );
    }

    #[test]
    fn folders_create_drive_flag_completes_to_drives() {
        let helper = helper_with_drives(&["my-builder-team-admin", "vetra-f80015b9"]);
        // After --drive, drive slugs should be offered (not the static command list).
        let matches = complete(&helper, "folders create --drive ");
        assert_eq!(matches, vec!["my-builder-team-admin", "vetra-f80015b9"]);
    }

    #[test]
    fn parent_flag_lists_drives_and_folders_with_labels() {
        let helper = helper_with_drives_and_folders(
            &["my-builder-team-admin"],
            &[
                ("Products", "my-builder-team-admin"),
                ("Services And Offerings", "my-builder-team-admin"),
            ],
        );
        let displays = complete_display(&helper, "folders create --parent ");
        assert!(
            displays.iter().any(|d| d.contains("(drive)")),
            "expected at least one (drive) entry, got {displays:?}"
        );
        assert!(
            displays.iter().any(|d| d.contains("(folder in")),
            "expected at least one (folder in ...) entry, got {displays:?}"
        );

        // Replacements are bare names (the resolver handles the rest).
        let replacements = complete(&helper, "folders create --parent ");
        assert!(replacements.contains(&"my-builder-team-admin".to_string()));
        assert!(replacements.contains(&"Products".to_string()));
    }

    #[test]
    fn folder_flag_lists_only_folders() {
        let helper = helper_with_drives_and_folders(
            &["my-builder-team-admin"],
            &[("Products", "my-builder-team-admin")],
        );
        let replacements = complete(&helper, "folders create --folder ");
        assert_eq!(
            replacements,
            vec!["Products"],
            "--folder must NOT surface drives, only folders"
        );

        let displays = complete_display(&helper, "folders create --folder ");
        assert!(
            displays.iter().all(|d| d.contains("(folder in")),
            "every --folder candidate should be labeled as a folder, got {displays:?}"
        );
    }

    #[test]
    fn bare_drives_lists_subcommands() {
        let helper = helper_with_drives(&["x"]);
        let matches = complete(&helper, "drives ");
        // Should surface drives subcommand prefixes from the static command list.
        assert!(
            matches.iter().any(|m| m == "drives list"),
            "expected 'drives list' in matches, got {matches:?}"
        );
        assert!(
            matches.iter().any(|m| m == "drives get "),
            "expected 'drives get ' in matches, got {matches:?}"
        );
        assert!(
            matches.iter().any(|m| m == "drives create"),
            "expected 'drives create' in matches, got {matches:?}"
        );
        assert!(
            matches.iter().any(|m| m == "drives delete "),
            "expected 'drives delete ' in matches, got {matches:?}"
        );
    }
}
