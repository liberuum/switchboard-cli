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

/// What became of one action the caller submitted.
#[derive(Debug, PartialEq)]
enum Outcome {
    Applied,
    ReducerError(String),
    Denied(String),
    /// A kind this CLI does not know. Never counted as applied.
    Unknown(String),
}

#[derive(Debug, PartialEq)]
struct SubmittedAction {
    action_id: String,
    scope: String,
    index: Option<i64>,
    outcome: Outcome,
}

impl SubmittedAction {
    fn position(&self) -> String {
        match (self.scope.as_str(), self.index) {
            ("", None) => String::new(),
            ("", Some(i)) => format!(" [#{i}]"),
            (scope, None) => format!(" [{scope}]"),
            (scope, Some(i)) => format!(" [{scope}#{i}]"),
        }
    }
}

/// `JobInfo.result`: one entry per submitted action that produced an
/// operation, plus whether every one of them applied.
#[derive(Debug, PartialEq)]
struct JobResult {
    actions: Vec<SubmittedAction>,
    all_applied: bool,
}

impl JobResult {
    fn applied_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| a.outcome == Outcome::Applied)
            .count()
    }

    fn tally(&self) -> String {
        format!("{}/{} applied", self.applied_count(), self.actions.len())
    }

    /// One line per action that did not apply, saying which and why.
    fn failure_lines(&self) -> Vec<String> {
        self.actions
            .iter()
            .filter_map(|a| {
                let why = match &a.outcome {
                    Outcome::Applied => return None,
                    Outcome::ReducerError(message) => format!("reducer error: {message}"),
                    Outcome::Denied(reason) => format!("denied: {reason}"),
                    Outcome::Unknown(kind) => format!("unrecognized outcome '{kind}'"),
                };
                Some(format!("✗ {}{}: {why}", a.action_id, a.position()))
            })
            .collect()
    }
}

/// Read `result` off a `JobInfo` or a `jobChanges` event.
///
/// Returns `None` when there is no per-action information to report, which is
/// what every Switchboard before the job-result fix gives: `jobStatus` omits
/// the field and `jobChanges` sends `{}`, because its `result` was declared
/// non-null. Absence is never reported as success — the caller falls back to
/// the plain status line.
fn parse_job_result(job: &Value) -> Option<JobResult> {
    let entries = job.pointer("/result/actions")?.as_array()?;
    if entries.is_empty() {
        return None;
    }

    let actions: Vec<SubmittedAction> = entries
        .iter()
        .map(|entry| SubmittedAction {
            action_id: entry["actionId"]
                .as_str()
                .unwrap_or("(unknown action)")
                .to_string(),
            scope: entry["scope"].as_str().unwrap_or_default().to_string(),
            index: entry["index"].as_i64(),
            outcome: match entry["kind"].as_str().unwrap_or_default() {
                "applied" => Outcome::Applied,
                "reducer-error" => Outcome::ReducerError(
                    entry["message"]
                        .as_str()
                        .unwrap_or("(no message)")
                        .to_string(),
                ),
                "denied" => Outcome::Denied(
                    entry["reason"]
                        .as_str()
                        .unwrap_or("(no reason)")
                        .to_string(),
                ),
                other => Outcome::Unknown(other.to_string()),
            },
        })
        .collect();

    // The per-action detail is authoritative: a summary claiming everything
    // applied cannot outvote an entry that says otherwise.
    let all_applied = job["result"]["allApplied"].as_bool().unwrap_or(true)
        && actions.iter().all(|a| a.outcome == Outcome::Applied);

    Some(JobResult {
        actions,
        all_applied,
    })
}

/// Query `jobStatus`, selecting `result`.
///
/// `JobInfo.result` carries what became of each submitted action. Older
/// Switchboards declare it `JSONObject!` and resolve it null, which nullifies
/// the whole selection and errors the query; fall back to the result-less
/// selection there, so this CLI keeps working against a server that simply has
/// no per-action detail to give. Only a server-answered error triggers the
/// retry — a dead connection is reported as-is rather than dialled twice.
async fn query_job_status(client: &crate::graphql::GraphQLClient, job_id: &str) -> Result<Value> {
    let id = job_id.replace('"', r#"\""#);
    let with_result = format!(
        r#"{{ jobStatus(jobId: "{id}") {{ id status result error createdAt completedAt }} }}"#
    );
    match client.query(&with_result, None).await {
        Ok(data) => Ok(data),
        Err(e) if crate::graphql::is_server_error(&e) => {
            let legacy = format!(
                r#"{{ jobStatus(jobId: "{id}") {{ id status error createdAt completedAt }} }}"#
            );
            client.query(&legacy, None).await.map_err(|_| e)
        }
        Err(e) => Err(e),
    }
}

async fn status(job_id: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = query_job_status(&client, job_id).await?;
    let job = &data["jobStatus"];

    // Unknown job ids come back as a synthesized FAILED job with error
    // "Job not found" — surface that as a clean not-found.
    if job["error"].as_str() == Some("Job not found") {
        match format {
            OutputFormat::Json | OutputFormat::Raw => print_json(job),
            _ => println!("Job {job_id} not found."),
        }
        return Ok(());
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(job),
        _ => {
            let s = job["status"].as_str().unwrap_or("-");
            println!("Job:      {}", job["id"].as_str().unwrap_or("-"));
            println!("Status:   {}", status_progress(s));
            if let Some(summary) = parse_job_result(job) {
                println!("Actions:  {}", summary.tally());
                for line in summary.failure_lines() {
                    println!("  {line}");
                }
            }
            if let Some(err) = job["error"].as_str().filter(|e| !e.is_empty()) {
                println!("Error:    {err}");
            }
            if let Some(created) = job["createdAt"].as_str() {
                println!("Created:  {created}");
            }
            if let Some(completed) = job["completedAt"].as_str() {
                println!("Completed: {completed}");
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
    if let Ok(data) = query_job_status(&client, job_id).await {
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

    // `result` is safe to select here on every server: the pre-fix ones declare
    // `JobChangeEvent.result` non-null and publish `{}` rather than null, which
    // `parse_job_result` reads as "no per-action detail". It was only
    // `jobStatus` that errored on it.
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
    let data = query_job_status(&client, &job_id_owned).await?;
    let job = &data["jobStatus"];
    let status_str = job["status"].as_str().unwrap_or("UNKNOWN");
    print_job_result(job, &job_id_owned, status_str, format)
}

/// Print a finished job, then fail if the actions it was given did not all
/// apply.
///
/// A reducer error or a denial does not fail the job — the operation is still
/// written, the job still reaches READ_READY — so the status line alone would
/// report a partly-rejected batch as a clean success. Exit non-zero instead,
/// after printing, so both a human and a script see it. JSON output is the job
/// object as the server sent it, `result` included: nothing is reshaped.
fn print_job_result(
    job: &Value,
    job_id: &str,
    status_str: &str,
    format: OutputFormat,
) -> Result<()> {
    let summary = parse_job_result(job);

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(job),
        _ => {
            match &summary {
                Some(s) => println!("Job {job_id} finished: {status_str} ({})", s.tally()),
                None => println!("Job {job_id} finished: {status_str}"),
            }
            if let Some(err) = job["error"].as_str().filter(|e| !e.is_empty()) {
                println!("Error: {err}");
            }
            if let Some(s) = &summary {
                for line in s.failure_lines() {
                    println!("  {line}");
                }
            }
        }
    }

    if let Some(s) = summary
        && !s.all_applied
    {
        let rejected = s.actions.len() - s.applied_count();
        anyhow::bail!(
            "{rejected} of {} submitted action(s) did not apply",
            s.actions.len()
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job_with(result: Value) -> Value {
        json!({ "id": "job-1", "status": "READ_READY", "error": null, "result": result })
    }

    #[test]
    fn reports_every_submitted_action() {
        let job = job_with(json!({
            "allApplied": false,
            "actions": [
                { "actionId": "a1", "scope": "global", "index": 4, "kind": "applied" },
                { "actionId": "a2", "scope": "global", "index": 5,
                  "kind": "reducer-error", "message": "name is required" },
                { "actionId": "a3", "scope": "auth", "index": 2,
                  "kind": "denied", "reason": "no write grant" },
            ],
        }));

        let summary = parse_job_result(&job).expect("result should parse");
        assert!(!summary.all_applied);
        assert_eq!(summary.tally(), "1/3 applied");
        assert_eq!(
            summary.failure_lines(),
            vec![
                "✗ a2 [global#5]: reducer error: name is required".to_string(),
                "✗ a3 [auth#2]: denied: no write grant".to_string(),
            ]
        );
    }

    #[test]
    fn a_clean_batch_applies_everything() {
        let job = job_with(json!({
            "allApplied": true,
            "actions": [{ "actionId": "a1", "scope": "global", "index": 0, "kind": "applied" }],
        }));

        let summary = parse_job_result(&job).expect("result should parse");
        assert!(summary.all_applied);
        assert_eq!(summary.tally(), "1/1 applied");
        assert!(summary.failure_lines().is_empty());
    }

    /// A pre-fix Switchboard cannot select `result` on `jobStatus` at all, so
    /// the field is simply absent. That is missing information, not success.
    #[test]
    fn an_absent_result_yields_no_summary() {
        let job = json!({ "id": "job-1", "status": "READ_READY", "error": null });
        assert_eq!(parse_job_result(&job), None);
    }

    #[test]
    fn a_null_result_yields_no_summary() {
        assert_eq!(parse_job_result(&job_with(Value::Null)), None);
    }

    /// What a pre-fix server publishes on the `jobChanges` subscription, where
    /// `result` is declared non-null: an empty object standing in for nothing.
    #[test]
    fn an_empty_result_object_yields_no_summary() {
        assert_eq!(parse_job_result(&job_with(json!({}))), None);
        assert_eq!(parse_job_result(&job_with(json!({ "actions": [] }))), None);
    }

    /// `allApplied` is derived when the server omits it, so an older shape of
    /// the field cannot turn a rejection into a success.
    #[test]
    fn all_applied_is_derived_when_missing() {
        let job = job_with(json!({
            "actions": [
                { "actionId": "a1", "scope": "global", "index": 0, "kind": "applied" },
                { "actionId": "a2", "scope": "global", "index": 1,
                  "kind": "denied", "reason": "nope" },
            ],
        }));
        assert!(!parse_job_result(&job).unwrap().all_applied);
    }

    /// And a summary that contradicts its own entries does not get the benefit
    /// of the doubt.
    #[test]
    fn a_rejected_action_outvotes_all_applied() {
        let job = job_with(json!({
            "allApplied": true,
            "actions": [{ "actionId": "a1", "scope": "global", "index": 0,
                          "kind": "reducer-error", "message": "boom" }],
        }));
        assert!(!parse_job_result(&job).unwrap().all_applied);
    }

    /// A kind added after this CLI shipped is reported verbatim, never counted
    /// as applied.
    #[test]
    fn an_unknown_kind_is_not_success() {
        let job = job_with(json!({
            "actions": [{ "actionId": "a1", "scope": "global", "index": 0, "kind": "deferred" }],
        }));
        let summary = parse_job_result(&job).unwrap();
        assert!(!summary.all_applied);
        assert_eq!(
            summary.failure_lines(),
            vec!["✗ a1 [global#0]: unrecognized outcome 'deferred'".to_string()]
        );
    }

    /// Missing detail must not panic or read as success.
    #[test]
    fn missing_fields_degrade_to_placeholders() {
        let job = job_with(json!({
            "actions": [{ "kind": "reducer-error" }, { "kind": "denied", "scope": "global" }],
        }));
        let summary = parse_job_result(&job).unwrap();
        assert_eq!(
            summary.failure_lines(),
            vec![
                "✗ (unknown action): reducer error: (no message)".to_string(),
                "✗ (unknown action) [global]: denied: (no reason)".to_string(),
            ]
        );
    }

    /// A partly-rejected batch exits non-zero; the same job with no result
    /// reported keeps today's exit code.
    #[test]
    fn a_partial_failure_is_an_error() {
        let rejected = job_with(json!({
            "allApplied": false,
            "actions": [
                { "actionId": "a1", "scope": "global", "index": 0, "kind": "applied" },
                { "actionId": "a2", "scope": "global", "index": 1,
                  "kind": "reducer-error", "message": "boom" },
            ],
        }));
        let err = print_job_result(&rejected, "job-1", "READ_READY", OutputFormat::Json)
            .expect_err("a partial failure must not exit clean");
        assert!(err.to_string().contains("1 of 2 submitted action(s)"));

        let unknown = json!({ "id": "job-1", "status": "READ_READY", "error": null });
        assert!(print_job_result(&unknown, "job-1", "READ_READY", OutputFormat::Json).is_ok());
    }
}
