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
    /// Block until a job completes, then print the result (uses WebSocket)
    Wait {
        /// Job ID
        job_id: String,
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
        JobsCommand::Wait { job_id, timeout } => {
            wait(&job_id, timeout, format, profile_name, quiet).await
        }
        JobsCommand::Watch { job_id } => watch(&job_id, format, profile_name, quiet).await,
    }
}

/// Format a job status with a progression indicator.
/// PENDING → RUNNING → WRITE_READY → READ_READY → COMPLETED
fn status_progress(status: &str) -> &str {
    match status {
        "PENDING" => "PENDING     [▱▱▱▱▱]",
        "RUNNING" => "RUNNING     [▰▱▱▱▱]",
        "WRITE_READY" => "WRITE_READY [▰▰▰▱▱]",
        "READ_READY" => "READ_READY  [▰▰▰▰▱]",
        "COMPLETED" => "COMPLETED   [▰▰▰▰▰]",
        "FAILED" => "FAILED      [✗✗✗✗✗]",
        "CANCELLED" => "CANCELLED   [—————]",
        other => other,
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
            let s = job["status"].as_str().unwrap_or("-");
            println!("Job:      {}", job["id"].as_str().unwrap_or("-"));
            println!("Status:   {}", status_progress(s));
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
    timeout: u64,
    format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, profile, client) = helpers::setup(profile_name)?;

    // First, check if the job is already in a terminal state.
    let query = format!(
        r#"{{ jobStatus(jobId: "{id}") {{ id status progress result error }} }}"#,
        id = job_id.replace('"', r#"\""#)
    );
    if let Ok(data) = client.query(&query, None).await {
        let job = &data["jobStatus"];
        let status_str = job["status"].as_str().unwrap_or("UNKNOWN");
        if matches!(
            status_str,
            "COMPLETED" | "FAILED" | "CANCELLED" | "READ_READY"
        ) {
            return print_job_result(job, job_id, status_str, format);
        }
        if !quiet {
            eprintln!("[{status_str}] Waiting for job {job_id}...");
        }
    }

    // Use WebSocket subscription for real-time status updates (no polling).
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

    let job_id_owned = job_id.to_string();
    let result: std::sync::Arc<std::sync::Mutex<Option<(String, Value)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let result_clone = result.clone();

    let timeout_dur = if timeout > 0 {
        Some(std::time::Duration::from_secs(timeout))
    } else {
        None
    };

    let ws_task = tokio::spawn(async move {
        websocket::subscribe(
            &ws_url,
            profile.token.as_deref(),
            &subscription,
            |data: Value| {
                if let Some(job) = data.get("jobChanges") {
                    let s = job["status"].as_str().unwrap_or("?");
                    if !quiet {
                        eprintln!("[{s}]");
                    }
                    if matches!(s, "COMPLETED" | "FAILED" | "CANCELLED" | "READ_READY") {
                        *result_clone.lock().unwrap() = Some((s.to_string(), job.clone()));
                    }
                }
            },
        )
        .await
    });

    // Wait for terminal status or timeout
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        if let Some((status_str, job_data)) = result.lock().unwrap().take() {
            ws_task.abort();
            return print_job_result(&job_data, &job_id_owned, &status_str, format);
        }

        if let Some(dur) = timeout_dur
            && start.elapsed() >= dur
        {
            ws_task.abort();
            anyhow::bail!("Timeout after {timeout}s");
        }

        if ws_task.is_finished() {
            break;
        }
    }

    // WebSocket closed without terminal status — fall back to a final poll
    let data = client
        .query(
            &format!(
                r#"{{ jobStatus(jobId: "{id}") {{ id status progress result error }} }}"#,
                id = job_id_owned.replace('"', r#"\""#)
            ),
            None,
        )
        .await?;
    let job = &data["jobStatus"];
    let status_str = job["status"].as_str().unwrap_or("UNKNOWN");
    print_job_result(job, &job_id_owned, status_str, format)
}

fn print_job_result(
    job: &Value,
    job_id: &str,
    status_str: &str,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(job),
        _ => {
            println!("Job {job_id} finished: {status_str}");
            if let Some(err) = job["error"].as_str().filter(|e| !e.is_empty()) {
                println!("Error: {err}");
            }
        }
    }
    Ok(())
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
                            println!("{} Error: {err}", status_progress(s));
                        } else {
                            println!("{}", status_progress(s));
                        }
                        if matches!(s, "COMPLETED" | "FAILED" | "CANCELLED") {
                            eprintln!("Job finished.");
                        }
                    }
                }
            }
        },
    )
    .await
}
