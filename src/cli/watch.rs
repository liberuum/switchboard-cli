use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Mutex;

use crate::cli::helpers;
use crate::graphql::websocket;
use crate::output::OutputFormat;

/// Simple HH:MM:SS.mmm timestamp from system clock.
fn ts() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() % 86400; // seconds within current day (UTC)
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let ms = d.subsec_millis();
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[derive(Subcommand)]
pub enum WatchCommand {
    /// Watch for document changes in real-time
    Docs {
        /// Filter by document type
        #[arg(long, short = 't')]
        r#type: Option<String>,
        /// Filter by drive (ID, slug or name): changes to the drive and to
        /// the documents in it. Filtered here: the server's `parentId` filter
        /// only matches add/remove events, never edits to a member.
        #[arg(long)]
        drive: Option<String>,
        /// Filter by document (ID, slug or name). Filtered here: the server's
        /// subscription filter only knows type and parent.
        #[arg(long)]
        doc: Option<String>,
        /// Execute a shell command for each event (receives JSON on stdin)
        #[arg(long)]
        exec: Option<String>,
    },
    /// Watch a job's status updates
    Job {
        /// Job ID to watch
        job_id: String,
    },
}

pub async fn run(
    cmd: WatchCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    // Derive WebSocket URL from the profile's HTTP URL
    // /graphql -> /graphql/subscriptions for the graphql-ws WebSocket endpoint
    let http_url = &profile.url;
    let base = http_url.trim_end_matches("/graphql").trim_end_matches('/');
    let ws_scheme = if base.starts_with("https") {
        "wss"
    } else {
        "ws"
    };
    let host = base
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let ws_url = format!("{ws_scheme}://{host}/graphql/subscriptions");

    match cmd {
        WatchCommand::Docs {
            r#type,
            drive,
            doc,
            exec,
        } => {
            // The server compares `parentId` with ids, and the document filter
            // is applied here by id, so resolve slugs and names up front.
            let drive = match drive {
                Some(d) => {
                    let id = helpers::resolve_doc(&client, &d).await?;
                    let members = drive_members(&client, &id).await?;
                    Some(DriveFilter::new(id, members))
                }
                None => None,
            };
            let doc = match doc {
                Some(d) => Some(helpers::resolve_doc(&client, &d).await?),
                None => None,
            };
            watch_docs(
                &ws_url,
                profile.token.as_deref(),
                r#type,
                drive,
                doc,
                exec,
                format,
                quiet,
            )
            .await
        }
        WatchCommand::Job { job_id } => {
            watch_job(&ws_url, profile.token.as_deref(), &job_id, format, quiet).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn watch_docs(
    ws_url: &str,
    token: Option<&str>,
    doc_type: Option<String>,
    drive: Option<DriveFilter>,
    doc: Option<String>,
    exec: Option<String>,
    format: OutputFormat,
    quiet: bool,
) -> Result<()> {
    // Build the search filter (required by the API). The server filters
    // documentChanges by `type` and `parentId` only: `identifiers` was never
    // applied and is deprecated as ignored (reactor-api 6.2.3-dev.35), and
    // `parentId` matches only events whose context names that parent (a child
    // added or removed), never an edit to a member. So `--doc` and `--drive`
    // are applied to each event below; only `type` goes to the server.
    let mut search_parts = Vec::new();
    if let Some(ref t) = doc_type {
        search_parts.push(format!(r#"type: "{t}""#));
    }

    let search_inner = search_parts.join(", ");
    let subscription = format!(
        r#"subscription {{ documentChanges(search: {{ {search_inner} }}) {{ type documents {{ id slug name documentType createdAtUtcIso lastModifiedAtUtcIso revisionsList {{ scope revision }} }} context {{ parentId childId }} }} }}"#
    );

    if !quiet && matches!(format, OutputFormat::Table) {
        eprintln!("Watching for document changes on {ws_url}...");
        eprintln!("Press Ctrl+C to stop.\n");
    }

    websocket::subscribe(ws_url, token, &subscription, move |data: Value| {
        let change = data
            .get("documentChanges")
            .cloned()
            .and_then(|c| match drive {
                Some(ref f) => f.apply(&c),
                None => Some(c),
            })
            .and_then(|c| match doc.as_deref() {
                Some(id) => filter_change(&c, id),
                None => Some(c),
            });
        if let Some(ref change) = change {
            // Execute shell command if --exec is set
            if let Some(ref cmd) = exec {
                let json = serde_json::to_string(change).unwrap_or_default();
                let _ = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .env("SWITCHBOARD_EVENT", &json)
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        if let Some(ref mut stdin) = child.stdin {
                            use std::io::Write;
                            let _ = stdin.write_all(json.as_bytes());
                        }
                        child.wait()
                    });
            }
            match format {
                OutputFormat::Json | OutputFormat::Raw => {
                    println!("{}", serde_json::to_string(change).unwrap_or_default());
                }
                _ => {
                    let event = change["type"].as_str().unwrap_or("?");
                    let ts = ts();
                    let docs = change["documents"].as_array();
                    if let Some(docs) = docs {
                        for doc in docs {
                            let id = doc["id"].as_str().unwrap_or("?");
                            let name = doc["name"].as_str().unwrap_or("?");
                            let dtype = doc["documentType"].as_str().unwrap_or("?");
                            let slug = doc["slug"].as_str().filter(|s| !s.is_empty() && *s != id);
                            let modified = doc["lastModifiedAtUtcIso"]
                                .as_str()
                                .map(|s| s.get(11..23).unwrap_or(s))
                                .unwrap_or("");
                            let rev_str = doc["revisionsList"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter()
                                        .map(|r| {
                                            format!(
                                                "{}:{}",
                                                r["scope"].as_str().unwrap_or("?"),
                                                r["revision"].as_u64().unwrap_or(0)
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join(",")
                                })
                                .unwrap_or_default();

                            let slug_part = slug.map(|s| format!(" ({s})")).unwrap_or_default();
                            let rev_part = if rev_str.is_empty() {
                                String::new()
                            } else {
                                format!(" rev=[{rev_str}]")
                            };
                            let mod_part = if modified.is_empty() {
                                String::new()
                            } else {
                                format!(" @{modified}")
                            };
                            println!(
                                "[{ts}] [{event}] {name}{slug_part} ({dtype}) {id}{rev_part}{mod_part}"
                            );
                        }
                    } else {
                        println!("[{ts}] [{event}]");
                    }
                    // Show context if present
                    if let Some(ctx) = change.get("context").filter(|c| !c.is_null()) {
                        let parent = ctx["parentId"].as_str().unwrap_or("");
                        let child = ctx["childId"].as_str().unwrap_or("");
                        if !parent.is_empty() || !child.is_empty() {
                            println!(
                                "         context: parent={} child={}",
                                if parent.is_empty() { "-" } else { parent },
                                if child.is_empty() { "-" } else { child },
                            );
                        }
                    }
                }
            }
        }
    })
    .await
}

/// Ids of the documents a drive holds (its file nodes), read once at start.
async fn drive_members(
    client: &crate::graphql::GraphQLClient,
    drive_id: &str,
) -> Result<HashSet<String>> {
    let escaped = drive_id.replace('"', r#"\""#);
    let query = format!(r#"{{ document(identifier: "{escaped}") {{ document {{ state }} }} }}"#);
    let data = client.query(&query, None).await?;
    Ok(data
        .pointer("/document/document/state/global/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter(|n| n["kind"].as_str() != Some("folder"))
                .filter_map(|n| n["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// `--drive`, applied per event: the drive's own events (its own edits, and
/// children added or removed, which also keep the member set current) and
/// edits to the documents it holds.
struct DriveFilter {
    drive_id: String,
    members: Mutex<HashSet<String>>,
}

impl DriveFilter {
    fn new(drive_id: String, members: HashSet<String>) -> Self {
        Self {
            drive_id,
            members: Mutex::new(members),
        }
    }

    fn apply(&self, change: &Value) -> Option<Value> {
        let mut members = self.members.lock().unwrap_or_else(|e| e.into_inner());
        if change.pointer("/context/parentId").and_then(|v| v.as_str()) == Some(&self.drive_id) {
            if let Some(child) = change.pointer("/context/childId").and_then(|v| v.as_str()) {
                match change["type"].as_str() {
                    Some("CHILD_ADDED") => {
                        members.insert(child.to_string());
                    }
                    Some("CHILD_REMOVED") => {
                        members.remove(child);
                    }
                    _ => {}
                }
            }
            return Some(change.clone());
        }
        let kept: Vec<Value> = change["documents"]
            .as_array()
            .map(|docs| {
                docs.iter()
                    .filter(|d| {
                        d["id"]
                            .as_str()
                            .is_some_and(|id| id == self.drive_id || members.contains(id))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if kept.is_empty() {
            return None;
        }
        let mut out = change.clone();
        out["documents"] = Value::Array(kept);
        Some(out)
    }
}

/// Narrow a `documentChanges` event to one document: keep only its entries in
/// `documents`, or the whole event when the document is the context's child
/// (a drive adding or removing it). `None` when the event is about others.
fn filter_change(change: &Value, doc_id: &str) -> Option<Value> {
    let child_match = change.pointer("/context/childId").and_then(|v| v.as_str()) == Some(doc_id);
    let docs = change["documents"].as_array();
    let kept: Vec<Value> = docs
        .map(|d| {
            d.iter()
                .filter(|doc| doc["id"].as_str() == Some(doc_id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if kept.is_empty() && !child_match {
        return None;
    }
    let mut out = change.clone();
    if !kept.is_empty() {
        out["documents"] = Value::Array(kept);
    }
    Some(out)
}

async fn watch_job(
    ws_url: &str,
    token: Option<&str>,
    job_id: &str,
    format: OutputFormat,
    quiet: bool,
) -> Result<()> {
    let subscription = format!(
        r#"subscription {{ jobChanges(jobId: "{id}") {{ jobId status result error }} }}"#,
        id = job_id.replace('"', r#"\""#)
    );

    if !quiet && matches!(format, OutputFormat::Table) {
        eprintln!("Watching job {job_id}...");
        eprintln!("Press Ctrl+C to stop.\n");
    }

    websocket::subscribe(ws_url, token, &subscription, |data: Value| {
        if let Some(job) = data.get("jobChanges") {
            match format {
                OutputFormat::Json | OutputFormat::Raw => {
                    println!("{}", serde_json::to_string(job).unwrap_or_default());
                }
                _ => {
                    let status = job["status"].as_str().unwrap_or("?");
                    let error = job["error"].as_str();
                    if let Some(err) = error {
                        println!("[{status}] Error: {err}");
                    } else {
                        println!("[{status}]");
                    }
                    if status == "COMPLETED" || status == "FAILED" {
                        eprintln!("Job finished with status: {status}");
                    }
                }
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{DriveFilter, filter_change};
    use serde_json::json;

    fn event(ids: &[&str], child: Option<&str>) -> serde_json::Value {
        json!({
            "type": "UPDATED",
            "documents": ids.iter().map(|id| json!({ "id": id, "name": id })).collect::<Vec<_>>(),
            "context": child.map(|c| json!({ "parentId": "drive", "childId": c })),
        })
    }

    #[test]
    fn drops_an_event_about_other_documents() {
        assert!(filter_change(&event(&["a", "b"], None), "x").is_none());
    }

    #[test]
    fn keeps_only_the_watched_document_of_a_batch() {
        let out = filter_change(&event(&["a", "x", "b"], None), "x").unwrap();
        let ids: Vec<_> = out["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["x"]);
        assert_eq!(out["type"], "UPDATED");
    }

    #[test]
    fn keeps_a_drive_event_whose_child_is_the_watched_document() {
        let out = filter_change(&event(&["drive"], Some("x")), "x").unwrap();
        assert_eq!(out["documents"][0]["id"], "drive");
    }

    #[test]
    fn tolerates_an_event_without_documents_or_context() {
        assert!(filter_change(&json!({ "type": "DELETED" }), "x").is_none());
    }

    fn drive() -> DriveFilter {
        DriveFilter::new("drive".into(), ["a".to_string()].into_iter().collect())
    }

    #[test]
    fn drive_keeps_edits_to_members_and_drops_outsiders() {
        let f = drive();
        let out = f.apply(&event(&["a", "z"], None)).unwrap();
        assert_eq!(out["documents"].as_array().unwrap().len(), 1);
        assert_eq!(out["documents"][0]["id"], "a");
        assert!(f.apply(&event(&["z"], None)).is_none());
    }

    #[test]
    fn drive_keeps_its_own_edits() {
        assert!(drive().apply(&event(&["drive"], None)).is_some());
    }

    #[test]
    fn drive_tracks_children_added_and_removed() {
        let f = drive();
        let added = json!({ "type": "CHILD_ADDED", "documents": [], "context": { "parentId": "drive", "childId": "n" } });
        assert!(f.apply(&added).is_some());
        assert!(f.apply(&event(&["n"], None)).is_some());
        let removed = json!({ "type": "CHILD_REMOVED", "documents": [], "context": { "parentId": "drive", "childId": "n" } });
        assert!(f.apply(&removed).is_some());
        assert!(f.apply(&event(&["n"], None)).is_none());
    }

    #[test]
    fn drive_ignores_another_drives_structure_events() {
        let other = json!({ "type": "CHILD_ADDED", "documents": [], "context": { "parentId": "other", "childId": "n" } });
        assert!(drive().apply(&other).is_none());
    }
}
