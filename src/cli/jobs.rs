use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;

use crate::cli::helpers;
use crate::graphql::websocket;
use crate::output::{OutputFormat, print_json};

#[derive(Subcommand)]
pub enum JobsCommand {
    /// Get the current status of a job
    Status {
        /// Job ID
        job_id: String,
    },
    /// Block until a job completes, then print the result
    Wait {
        /// Job ID
        job_id: String,
        /// Polling interval in seconds
        #[arg(long, default_value = "2")]
        interval: u64,
        /// Timeout in seconds (0 = no timeout)
        #[arg(long, default_value = "300")]
        timeout: u64,
    },
    /// Stream job status updates via WebSocket
    Watch {
        /// Job ID
        job_id: String,
    },
}

pub async fn run(
    cmd: JobsCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    match cmd {
        JobsCommand::Status { job_id } => status(&job_id, format, profile_name).await,
        JobsCommand::Wait {
            job_id,
            interval,
            timeout,
        } => wait(&job_id, interval, timeout, format, profile_name, quiet).await,
        JobsCommand::Watch { job_id } => watch(&job_id, format, profile_name, quiet).await,
    }
}

async fn status(job_id: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let query = format!(
        r#"{{ jobStatus(jobId: "{id}") {{ id status progress result error createdAt updatedAt }} }}"#,
        id = job_id.replace('"', r#"\""#)
    );

    let data = client.query(&query, None).await?;
    let job = &data["jobStatus"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(job),
        _ => {
            println!("Job:      {}", job["id"].as_str().unwrap_or("-"));
            println!("Status:   {}", job["status"].as_str().unwrap_or("-"));
            if let Some(p) = job["progress"].as_f64() {
                println!("Progress: {:.0}%", p * 100.0);
            }
            if let Some(err) = job["error"].as_str().filter(|e| !e.is_empty()) {
                println!("Error:    {err}");
            }
            if let Some(created) = job["createdAt"].as_str() {
                println!("Created:  {created}");
            }
            if let Some(updated) = job["updatedAt"].as_str() {
                println!("Updated:  {updated}");
            }
        }
    }

    Ok(())
}

async fn wait(
    job_id: &str,
    interval: u64,
    timeout: u64,
    format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let start = std::time::Instant::now();

    loop {
        let query = format!(
            r#"{{ jobStatus(jobId: "{id}") {{ id status progress result error }} }}"#,
            id = job_id.replace('"', r#"\""#)
        );

        let data = client.query(&query, None).await?;
        let job = &data["jobStatus"];
        let status_str = job["status"].as_str().unwrap_or("UNKNOWN");

        match status_str {
            "COMPLETED" | "FAILED" | "CANCELLED" => {
                match format {
                    OutputFormat::Json | OutputFormat::Raw => print_json(job),
                    _ => {
                        println!("Job {} finished: {}", job_id, status_str);
                        if let Some(err) = job["error"].as_str().filter(|e| !e.is_empty()) {
                            println!("Error: {err}");
                        }
                    }
                }
                return Ok(());
            }
            _ => {
                if !quiet && matches!(format, OutputFormat::Table) {
                    if let Some(p) = job["progress"].as_f64() {
                        eprint!("\r[{status_str}] {:.0}%", p * 100.0);
                    } else {
                        eprint!("\r[{status_str}] waiting...");
                    }
                }
            }
        }

        if timeout > 0 && start.elapsed().as_secs() >= timeout {
            anyhow::bail!("Timeout after {timeout}s — job still {status_str}");
        }

        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
    }
}

async fn watch(
    job_id: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, profile, _client) = helpers::setup(profile_name)?;

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

    let subscription = format!(
        r#"subscription {{ jobChanges(jobId: "{id}") {{ jobId status result error }} }}"#,
        id = job_id.replace('"', r#"\""#)
    );

    if !quiet && matches!(format, OutputFormat::Table) {
        eprintln!("Watching job {job_id}...");
        eprintln!("Press Ctrl+C to stop.\n");
    }

    websocket::subscribe(
        &ws_url,
        profile.token.as_deref(),
        &subscription,
        |data: Value| {
            if let Some(job) = data.get("jobChanges") {
                match format {
                    OutputFormat::Json | OutputFormat::Raw => {
                        println!("{}", serde_json::to_string(job).unwrap_or_default());
                    }
                    _ => {
                        let s = job["status"].as_str().unwrap_or("?");
                        let error = job["error"].as_str();
                        if let Some(err) = error {
                            println!("[{s}] Error: {err}");
                        } else {
                            println!("[{s}]");
                        }
                    }
                }
            }
        },
    )
    .await
}
