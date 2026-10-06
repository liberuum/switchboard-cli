use std::io::Write;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
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
    /// Document model types from introspection cache
    model_types: Vec<String>,
    /// Guide topic names
    guide_topics: Vec<String>,
    /// Profile names from config
    profile_names: Vec<String>,
    /// Drives, folders and documents from the server. Loaded in the background
    /// and swapped in under the lock, so the prompt never waits for them.
    dynamic: Arc<RwLock<DynamicCompletions>>,
}

/// The completion data that comes from the server.
#[derive(Default)]
struct DynamicCompletions {
    /// Drive slugs (and names) for `--drive` and drive positionals
    drive_slugs: Vec<String>,
    /// Folder names across all drives
    folder_names: Vec<String>,
    /// Drive slug each folder lives in (parallel to folder_names)
    folder_drive_slugs: Vec<String>,
    /// Document IDs for completion (the raw UUID)
    doc_ids: Vec<String>,
    /// Document display labels for completion ("uuid  name  (type)")
    doc_labels: Vec<String>,
    /// Drive slug for each doc entry (parallel to doc_ids/doc_labels)
    doc_drive_slugs: Vec<String>,
}

/// One load of the server's completion data.
struct CompletionData {
    drive_slugs: Vec<String>,
    docs: Vec<DocEntry>,
    folders: Vec<FolderEntry>,
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
        model_types: Vec<String>,
        profile_names: Vec<String>,
        dynamic: Arc<RwLock<DynamicCompletions>>,
    ) -> Self {
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
            "migrate ".into(),
            "migrate --from ".into(),
            "migrate --to ".into(),
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
            "migrate".into(),
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
            model_types,
            guide_topics,
            profile_names,
            dynamic,
        }
    }
}

impl DynamicCompletions {
    /// Replace everything with a fresh load (a failed load never gets here,
    /// so the previous data survives a transient error).
    fn apply(&mut self, data: CompletionData) {
        self.drive_slugs = data.drive_slugs;
        let (names, drives) = Self::build_folder_completions(&data.folders);
        self.folder_names = names;
        self.folder_drive_slugs = drives;
        let (ids, labels, drive_slugs) = Self::build_doc_completions(&data.docs);
        self.doc_ids = ids;
        self.doc_labels = labels;
        self.doc_drive_slugs = drive_slugs;
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
        // A poisoned lock only means a loader panicked mid-write; the data is
        // still the last good load.
        let dynamic = self.dynamic.read().unwrap_or_else(|e| e.into_inner());

        // ── Drive slug completion ────────────────────────────
        if prev_word == Some("--drive")
            || input.starts_with("drives get ")
            || input.starts_with("drives delete ")
            || input.starts_with("export drive ")
            || input.starts_with("docs tree ")
        {
            let matches = filter_pairs(&dynamic.drive_slugs, partial);
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }

        // ── Parent completion (drives + folders, labeled) ────
        // `--parent` is universal: it accepts a drive (root placement) or a
        // folder (nested placement). Surface both, with display labels so the
        // user can tell them apart.
        if prev_word == Some("--parent") {
            let mut matches: Vec<Pair> = dynamic
                .drive_slugs
                .iter()
                .filter(|s| s.to_lowercase().starts_with(&partial.to_lowercase()))
                .map(|s| Pair {
                    display: format!("{s}    (drive)"),
                    replacement: s.clone(),
                })
                .collect();
            matches.extend(
                dynamic
                    .folder_names
                    .iter()
                    .zip(dynamic.folder_drive_slugs.iter())
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
            let matches: Vec<Pair> = dynamic
                .folder_names
                .iter()
                .zip(dynamic.folder_drive_slugs.iter())
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
                    let matches: Vec<Pair> = dynamic
                        .doc_ids
                        .iter()
                        .zip(dynamic.doc_labels.iter())
                        .zip(dynamic.doc_drive_slugs.iter())
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
                        &dynamic.drive_slugs,
                        &dynamic.doc_ids,
                        &dynamic.doc_labels,
                        &dynamic.doc_drive_slugs,
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
                &dynamic.drive_slugs,
                &dynamic.doc_ids,
                &dynamic.doc_labels,
                &dynamic.doc_drive_slugs,
                partial,
            );
            if !matches.is_empty() {
                return Ok((word_start, matches));
            }
        }
        // after --doc flag
        if prev_word == Some("--doc") {
            let matches = filter_doc_pairs(&dynamic.doc_ids, &dynamic.doc_labels, partial);
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

/// An animated spinner on stderr, drawn by a background task.
///
/// Every frame is drawn under `stopped`'s lock, and `stop()` sets the flag and
/// clears the line under the same lock. Aborting the task alone was not enough:
/// an abort only lands at the task's next `.await`, so a frame already being
/// drawn could land after the clear — on top of the prompt, which then looked
/// like a REPL still "Loading...".
struct Spinner {
    stopped: Arc<Mutex<bool>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Spinner {
    fn start(message: &str) -> Self {
        // First frame synchronously, so it is visible before any await.
        eprint!("\r\x1b[2K⠋ {message}");
        let _ = std::io::stderr().flush();

        let stopped = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&stopped);
        let msg = message.to_string();
        let handle = tokio::spawn(async move {
            let frames = ['⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '⠋'];
            let mut i = 0;
            loop {
                tokio::time::sleep(Duration::from_millis(80)).await;
                let done = flag.lock().unwrap_or_else(|e| e.into_inner());
                if *done {
                    break;
                }
                eprint!("\r\x1b[2K{} {msg}", frames[i % frames.len()]);
                let _ = std::io::stderr().flush();
                drop(done);
                i += 1;
            }
        });
        Self { stopped, handle }
    }

    /// Stop drawing and clear the line. No frame can follow this.
    fn stop(self) {
        let mut done = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        *done = true;
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
        drop(done);
        self.handle.abort();
    }
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

/// How long one background load of completion data may take. Commands keep the
/// client's own (much longer) timeout; completions are a convenience, and a
/// server that cannot answer in this time should not hold anything up.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(8);
/// How old completion data may get before the prompt starts a background reload.
const COMPLETION_TTL: Duration = Duration::from_secs(5);

const DRIVES_QUERY: &str = r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug state } } }"#;

/// Everything one drive listing yields: completion slugs (slug and, when it
/// differs, name), folders from each drive's nodes, and `(id, slug)` per drive
/// for the per-drive document lookups. Deleted drives are skipped.
fn parse_drive_listing(
    data: &serde_json::Value,
) -> (Vec<String>, Vec<FolderEntry>, Vec<(String, String)>) {
    let mut slugs = Vec::new();
    let mut folders = Vec::new();
    let mut drives = Vec::new();
    let items = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    for d in items {
        if d.pointer("/state/document/isDeleted")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            continue;
        }
        let slug = d["slug"].as_str().unwrap_or("");
        if !slug.is_empty() {
            slugs.push(slug.to_string());
        }
        // Also the drive name, so users can tab-complete by name.
        if let Some(name) = d["name"].as_str()
            && !name.is_empty()
            && name != slug
        {
            slugs.push(name.to_string());
        }
        if let Some(nodes) = d.pointer("/state/global/nodes").and_then(|v| v.as_array()) {
            for n in nodes {
                if n["kind"].as_str() != Some("folder") {
                    continue;
                }
                if let Some(name) = n["name"].as_str()
                    && !name.is_empty()
                {
                    folders.push(FolderEntry {
                        name: name.to_string(),
                        drive_slug: slug.to_string(),
                    });
                }
            }
        }
        if let Some(id) = d["id"].as_str()
            && !id.is_empty()
        {
            drives.push((id.to_string(), slug.to_string()));
        }
    }
    (slugs, folders, drives)
}

/// A drive's documents (its `child` relationships). A failure yields none: one
/// unreadable drive should not cost the completions for every other.
async fn fetch_drive_docs(
    client: &crate::graphql::GraphQLClient,
    drive_id: &str,
    drive_slug: &str,
) -> Vec<DocEntry> {
    let query = format!(
        r#"{{ documentOutgoingRelationships(sourceIdentifier: "{drive_id}", relationshipType: "child") {{ items {{ id name documentType }} }} }}"#
    );
    let Ok(data) = client.query(&query, None).await else {
        return Vec::new();
    };
    data.pointer("/documentOutgoingRelationships/items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|node| DocEntry {
                    id: node["id"].as_str().unwrap_or("").to_string(),
                    name: node["name"].as_str().unwrap_or("").to_string(),
                    doc_type: node["documentType"].as_str().unwrap_or("").to_string(),
                    drive_slug: drive_slug.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One load: a single drive listing (it used to be fetched three times, each
/// with every drive's full state), then every drive's documents in parallel.
async fn fetch_completion_data(client: &crate::graphql::GraphQLClient) -> Result<CompletionData> {
    let listing = client.query(DRIVES_QUERY, None).await?;
    let (drive_slugs, folders, drives) = parse_drive_listing(&listing);

    let mut set = tokio::task::JoinSet::new();
    for (index, (id, slug)) in drives.into_iter().enumerate() {
        let client = client.clone();
        set.spawn(async move { (index, fetch_drive_docs(&client, &id, &slug).await) });
    }
    let mut per_drive = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(result) = joined {
            per_drive.push(result);
        }
    }
    // Keep drive order stable, whatever order the lookups finished in.
    per_drive.sort_by_key(|(index, _)| *index);
    let docs = per_drive.into_iter().flat_map(|(_, docs)| docs).collect();

    Ok(CompletionData {
        drive_slugs,
        docs,
        folders,
    })
}

/// Outcome of the latest load, for a one-line notice at the prompt.
#[derive(Default)]
struct LoadStatus {
    error: Option<String>,
    reported: bool,
}

/// Loads completion data off the prompt's path. A load runs as a background
/// task and writes into the helper's shared data when it finishes; the REPL
/// never awaits it except on an explicit `refresh`.
struct CompletionLoader {
    dynamic: Arc<RwLock<DynamicCompletions>>,
    status: Arc<Mutex<LoadStatus>>,
    inflight: Option<tokio::task::JoinHandle<()>>,
    last_started: Option<Instant>,
    timeout: Duration,
}

impl CompletionLoader {
    fn new(dynamic: Arc<RwLock<DynamicCompletions>>) -> Self {
        Self {
            dynamic,
            status: Arc::new(Mutex::new(LoadStatus::default())),
            inflight: None,
            last_started: None,
            timeout: COMPLETION_TIMEOUT,
        }
    }

    /// Start a load now, replacing one in flight (e.g. for the old profile).
    fn start(&mut self, client: &crate::graphql::GraphQLClient) {
        if let Some(previous) = self.inflight.take() {
            previous.abort();
        }
        let client = client.clone();
        let dynamic = Arc::clone(&self.dynamic);
        let status = Arc::clone(&self.status);
        let timeout = self.timeout;
        self.last_started = Some(Instant::now());
        self.inflight = Some(tokio::spawn(async move {
            let outcome = match tokio::time::timeout(timeout, fetch_completion_data(&client)).await
            {
                Ok(Ok(data)) => {
                    dynamic
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .apply(data);
                    None
                }
                Ok(Err(e)) => Some(format!("{e:#}")),
                Err(_) => Some(format!(
                    "{} did not answer within {}s",
                    client.url,
                    timeout.as_secs_f32()
                )),
            };
            let mut status = status.lock().unwrap_or_else(|e| e.into_inner());
            match outcome {
                None => *status = LoadStatus::default(),
                // Report a failure once, not after every retry.
                Some(error) => {
                    if status.error.is_none() {
                        status.reported = false;
                    }
                    status.error = Some(error);
                }
            }
        }));
    }

    /// Start a background load if the data is stale and none is running.
    fn refresh_if_stale(&mut self, client: &crate::graphql::GraphQLClient) {
        let busy = self.inflight.as_ref().is_some_and(|h| !h.is_finished());
        let stale = self
            .last_started
            .is_none_or(|t| t.elapsed() >= COMPLETION_TTL);
        if !busy && stale {
            self.start(client);
        }
    }

    /// Load now and wait for it (bounded by COMPLETION_TIMEOUT) — `refresh`.
    async fn reload(&mut self, client: &crate::graphql::GraphQLClient) {
        self.start(client);
        if let Some(handle) = self.inflight.take() {
            let _ = handle.await;
        }
    }

    /// A failure not yet shown to the user, once.
    fn take_unreported_error(&self) -> Option<String> {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if status.reported {
            return None;
        }
        status.reported = true;
        status.error.clone()
    }
}

/// The notice printed when completions could not be loaded.
fn completion_error_notice(error: &str) -> String {
    format!("(tab completion unavailable: {error} — type `refresh` to retry)")
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

    // Drives, folders and documents for tab completion load in the background:
    // the prompt is usable at once, and a slow or wedged server no longer holds
    // the REPL behind a spinner (it used to wait out every request's timeout).
    let dynamic = Arc::new(RwLock::new(DynamicCompletions::default()));
    let mut loader = CompletionLoader::new(Arc::clone(&dynamic));
    loader.start(&client);

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

    let helper = ReplHelper::new(model_types, profile_names, dynamic);
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
        // Pick up changes made outside the REPL (e.g. a drive created from
        // another terminal) without the user typing `refresh`. The reload runs
        // in the background, so the prompt below never waits for it.
        loader.refresh_if_stale(&client);
        if !quiet && let Some(error) = loader.take_unreported_error() {
            eprintln!("{}", completion_error_notice(&error).dimmed());
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
                    let spinner = Spinner::start("Refreshing completions...");
                    loader.reload(&client).await;
                    let new_model_types: Vec<String> =
                        crate::graphql::introspection::load_cache(&current_profile)
                            .ok()
                            .flatten()
                            .map(|c| c.models.values().map(|m| m.document_type.clone()).collect())
                            .unwrap_or_default();
                    spinner.stop();
                    if let Some(helper) = rl.helper_mut()
                        && !new_model_types.is_empty()
                    {
                        helper.model_types = new_model_types;
                    }
                    match loader.take_unreported_error() {
                        Some(error) => eprintln!("{}", completion_error_notice(&error)),
                        None => eprintln!("Completions refreshed."),
                    }
                    continue;
                }

                // ── Parse as CLI command via clap ────────────────────
                let tokens = shell_split(line);
                let args = std::iter::once("switchboard".to_string()).chain(tokens);

                match Cli::try_parse_ids_from(args) {
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

                        // Reload completions after a modifying command — in the
                        // background, so the next prompt is not held up by it.
                        if modifies_drives || modifies_docs || modifies_folders {
                            loader.start(&client);
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
                                if let Some(helper) = rl.helper_mut() {
                                    helper.model_types = new_model_types;
                                }
                                // The new profile's drives load in the background
                                // (and replace the old profile's in flight).
                                loader.start(&client);
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
                        if let Ok(parsed) = Cli::try_parse_ids_from(guide_args)
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
    use super::{
        CompletionData, CompletionLoader, DynamicCompletions, FolderEntry, ReplHelper,
        parse_drive_listing, shell_split,
    };
    use rustyline::completion::Completer;
    use rustyline::history::DefaultHistory;
    use std::sync::{Arc, RwLock};
    use std::time::{Duration, Instant};

    /// A helper whose server data is already loaded.
    fn helper_with_data(data: CompletionData) -> ReplHelper {
        let mut dynamic = DynamicCompletions::default();
        dynamic.apply(data);
        ReplHelper::new(vec![], vec![], Arc::new(RwLock::new(dynamic)))
    }

    /// Build a helper with fixed completion data for tests.
    fn helper_with_drives(drives: &[&str]) -> ReplHelper {
        helper_with_data(CompletionData {
            drive_slugs: drives.iter().map(|s| s.to_string()).collect(),
            docs: vec![],
            folders: vec![],
        })
    }

    /// Build a helper with both drive and folder data for completion tests.
    fn helper_with_drives_and_folders(
        drives: &[&str],
        folders: &[(&str, &str)], // (folder_name, drive_slug)
    ) -> ReplHelper {
        helper_with_data(CompletionData {
            drive_slugs: drives.iter().map(|s| s.to_string()).collect(),
            docs: vec![],
            folders: folders
                .iter()
                .map(|(name, slug)| FolderEntry {
                    name: (*name).to_string(),
                    drive_slug: (*slug).to_string(),
                })
                .collect(),
        })
    }

    // ── Background loading ───────────────────────────────────────────

    /// A minimal GraphQL server: answers each POST with `respond(body)`.
    async fn fake_switchboard(respond: fn(&str) -> String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Read the headers, then exactly Content-Length bytes of body.
                    let (head_end, length) = loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                            let length = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            break (i + 4, length);
                        }
                    };
                    while buf.len() < head_end + length {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                    let json = respond(&body);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}/graphql")
    }

    /// A server that accepts every connection and never answers — what a
    /// Switchboard busy replaying a large store looks like from outside.
    async fn wedged_switchboard() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        format!("http://{addr}/graphql")
    }

    fn vault_response(body: &str) -> String {
        if body.contains("findDocuments") {
            serde_json::json!({ "data": { "findDocuments": { "items": [
                { "id": "d1", "name": "Vault One", "slug": "vault-one", "state": { "global": { "nodes": [
                    { "kind": "folder", "name": "notes" },
                    { "kind": "file", "name": "a note" }
                ] } } },
                { "id": "d2", "name": "vault-two", "slug": "vault-two", "state": { "global": { "nodes": [] } } }
            ] } } })
            .to_string()
        } else if body.contains("d1") {
            serde_json::json!({ "data": { "documentOutgoingRelationships": { "items": [
                { "id": "n1", "name": "first", "documentType": "bai/knowledge-note" },
                { "id": "n2", "name": "with space", "documentType": "bai/moc" }
            ] } } })
            .to_string()
        } else {
            serde_json::json!({ "data": { "documentOutgoingRelationships": { "items": [
                { "id": "n3", "name": "third", "documentType": "bai/source" }
            ] } } })
            .to_string()
        }
    }

    fn loader_for(url: &str) -> (CompletionLoader, crate::graphql::GraphQLClient) {
        let loader = CompletionLoader::new(Arc::new(RwLock::new(DynamicCompletions::default())));
        (
            loader,
            crate::graphql::GraphQLClient::new(url.to_string(), None),
        )
    }

    #[test]
    fn one_drive_listing_yields_slugs_folders_and_drives() {
        let data = serde_json::json!({ "findDocuments": { "items": [
            { "id": "d1", "name": "Vault One", "slug": "vault-one", "state": { "global": { "nodes": [
                { "kind": "folder", "name": "notes" }, { "kind": "folder", "name": "" }, { "kind": "file", "name": "x" }
            ] } } },
            { "id": "d2", "name": "same", "slug": "same", "state": {} },
            { "id": "gone", "name": "Gone", "slug": "gone", "state": { "document": { "isDeleted": true } } },
            { "id": "", "name": "", "slug": "no-id" }
        ] } });
        let (slugs, folders, drives) = parse_drive_listing(&data);
        assert_eq!(slugs, vec!["vault-one", "Vault One", "same", "no-id"]);
        assert_eq!(
            folders
                .iter()
                .map(|f| (f.name.as_str(), f.drive_slug.as_str()))
                .collect::<Vec<_>>(),
            vec![("notes", "vault-one")]
        );
        assert_eq!(
            drives,
            vec![
                ("d1".to_string(), "vault-one".to_string()),
                ("d2".to_string(), "same".to_string())
            ]
        );
        assert_eq!(
            parse_drive_listing(&serde_json::json!({})).0,
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn a_load_fills_drives_folders_and_every_drives_documents() {
        let url = fake_switchboard(vault_response).await;
        let (mut loader, client) = loader_for(&url);
        loader.reload(&client).await;

        assert!(loader.take_unreported_error().is_none());
        let data = loader.dynamic.read().unwrap();
        assert_eq!(
            data.drive_slugs,
            vec!["vault-one", "Vault One", "vault-two"]
        );
        assert_eq!(data.folder_names, vec!["notes"]);
        // Documents of both drives, in drive order, the spaced name quoted.
        assert_eq!(data.doc_ids, vec!["first", "\"with space\"", "third"]);
        assert_eq!(
            data.doc_drive_slugs,
            vec!["vault-one", "vault-one", "vault-two"]
        );
    }

    #[tokio::test]
    async fn a_wedged_server_costs_the_timeout_not_the_prompt_and_keeps_the_last_good_data() {
        let url = wedged_switchboard().await;
        let (mut loader, client) = loader_for(&url);
        loader.timeout = Duration::from_millis(300);
        loader.dynamic.write().unwrap().drive_slugs = vec!["kept".into()];

        // Starting a load returns at once: the prompt is never behind it.
        let t = Instant::now();
        loader.start(&client);
        assert!(t.elapsed() < Duration::from_millis(50));

        // Even an explicit reload is bounded by the completion timeout, not
        // the client's 120 s request timeout.
        let t = Instant::now();
        loader.reload(&client).await;
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "took {:?}",
            t.elapsed()
        );

        let error = loader
            .take_unreported_error()
            .expect("the failure is reported");
        assert!(error.contains("did not answer"), "{error}");
        assert!(loader.take_unreported_error().is_none(), "reported once");
        assert_eq!(loader.dynamic.read().unwrap().drive_slugs, vec!["kept"]);

        // A repeat failure is not reported again.
        loader.reload(&client).await;
        assert!(loader.take_unreported_error().is_none());
    }

    #[tokio::test]
    async fn a_success_after_a_failure_clears_it() {
        let wedged = wedged_switchboard().await;
        let (mut loader, client) = loader_for(&wedged);
        loader.timeout = Duration::from_millis(200);
        loader.reload(&client).await;
        assert!(loader.take_unreported_error().is_some());

        let good = crate::graphql::GraphQLClient::new(fake_switchboard(vault_response).await, None);
        loader.timeout = Duration::from_secs(5);
        loader.reload(&good).await;
        assert!(loader.take_unreported_error().is_none());
        assert_eq!(loader.dynamic.read().unwrap().doc_ids.len(), 3);
    }

    #[tokio::test]
    async fn the_prompt_refresh_only_starts_a_load_when_stale_and_idle() {
        let url = wedged_switchboard().await;
        let (mut loader, client) = loader_for(&url);
        loader.timeout = Duration::from_secs(5);

        loader.refresh_if_stale(&client); // never loaded: starts one
        let first = loader.last_started.expect("started");
        loader.refresh_if_stale(&client); // in flight and fresh: no restart
        assert_eq!(loader.last_started, Some(first));

        // Stale but still in flight: still no restart.
        loader.last_started = Some(Instant::now() - Duration::from_secs(60));
        let marked = loader.last_started;
        loader.refresh_if_stale(&client);
        assert_eq!(loader.last_started, marked);
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
