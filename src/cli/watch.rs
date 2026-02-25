use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;

use crate::cli::helpers;
use crate::graphql::websocket;
use crate::output::OutputFormat;

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
        WatchCommand::Docs { r#type, drive, doc } => {
            watch_docs(
                &ws_url,
                profile.token.as_deref(),
                r#type,
                drive,
                doc,
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

async fn watch_docs(
    ws_url: &str,
    token: Option<&str>,
    doc_type: Option<String>,
    drive: Option<String>,
    doc: Option<String>,
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
        r#"subscription {{ documentChanges(search: {{ {search_inner} }}) {{ type documents {{ id name documentType }} context {{ parentId childId }} }} }}"#
    );

    if !quiet && matches!(format, OutputFormat::Table) {
        eprintln!("Watching for document changes on {ws_url}...");
        eprintln!("Press Ctrl+C to stop.\n");
    }

    websocket::subscribe(ws_url, token, &subscription, |data: Value| {
        if let Some(change) = data.get("documentChanges") {
            match format {
                OutputFormat::Json | OutputFormat::Raw => {
                    println!("{}", serde_json::to_string(change).unwrap_or_default());
                }
                OutputFormat::Table => {
                    let event = change["type"].as_str().unwrap_or("?");
                    let docs = change["documents"].as_array();
                    if let Some(docs) = docs {
                        for doc in docs {
                            let id = doc["id"].as_str().unwrap_or("?");
                            let name = doc["name"].as_str().unwrap_or("?");
                            let dtype = doc["documentType"].as_str().unwrap_or("?");
                            println!("[{event}] {name} ({dtype}) {id}");
                        }
                    } else {
                        println!("[{event}]");
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
                OutputFormat::Table => {
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
