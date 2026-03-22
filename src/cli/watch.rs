use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;

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
        /// Filter by drive ID or slug
        #[arg(long)]
        drive: Option<String>,
        /// Filter by document ID
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
    let (_name, profile, _client) = helpers::setup(profile_name)?;

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
    drive: Option<String>,
    doc: Option<String>,
    exec: Option<String>,
    format: OutputFormat,
    quiet: bool,
) -> Result<()> {
    // Build the search filter (required by the API)
    // SearchFilterInput: { type?: String, parentId?: String, identifiers?: [String!] }
    let mut search_parts = Vec::new();
    if let Some(ref t) = doc_type {
        search_parts.push(format!(r#"type: "{t}""#));
    }
    if let Some(ref d) = drive {
        search_parts.push(format!(r#"parentId: "{d}""#));
    }
    if let Some(ref id) = doc {
        search_parts.push(format!(r#"identifiers: ["{id}"]"#));
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
        if let Some(change) = data.get("documentChanges") {
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
