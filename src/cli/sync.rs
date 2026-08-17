use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json};

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Create or update a sync channel
    Touch {
        /// Channel input as JSON (or path to JSON file prefixed with @)
        input: String,
    },
    /// Push sync envelopes
    Push {
        /// Envelopes JSON (or path to JSON file prefixed with @)
        envelopes: String,
    },
    /// Poll for sync envelopes
    Poll {
        /// Channel ID
        channel_id: String,
        /// Acknowledge up to this outbox sequence number
        #[arg(long, default_value_t = 0)]
        ack: i64,
        /// Latest known outbox sequence number
        #[arg(long, default_value_t = 0)]
        latest: i64,
    },
}

pub async fn run(cmd: SyncCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        SyncCommand::Touch { input } => touch(&input, format, profile_name).await,
        SyncCommand::Push { envelopes } => push(&envelopes, format, profile_name).await,
        SyncCommand::Poll {
            channel_id,
            ack,
            latest,
        } => poll(&channel_id, ack, latest, format, profile_name).await,
    }
}

fn load_json_arg(input: &str) -> Result<Value> {
    if let Some(path) = input.strip_prefix('@') {
        let contents = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&contents)?)
    } else {
        Ok(serde_json::from_str(input)?)
    }
}

async fn touch(input: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let input_val = load_json_arg(input)?;

    // Pass the input as a GraphQL variable — inlining JSON as a GraphQL
    // literal breaks on enum-typed fields (they'd be sent as quoted strings).
    let mutation = "mutation($input: TouchChannelInput!) { touchChannel(input: $input) { success ackOrdinal } }";
    let vars = serde_json::json!({ "input": input_val });

    let data = client.query(mutation, Some(&vars)).await?;
    let result = &data["touchChannel"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(result),
        _ => {
            println!(
                "Success:     {}",
                result["success"].as_bool().unwrap_or(false)
            );
            println!("Ack ordinal: {}", result["ackOrdinal"]);
        }
    }

    Ok(())
}

async fn push(
    envelopes_input: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let envelopes_val = load_json_arg(envelopes_input)?;

    // pushSyncEnvelopes returns a plain Boolean — no selection set allowed.
    // Envelopes go through a GraphQL variable so enum fields like
    // `type: "OPERATIONS"` coerce correctly (inlined literals would be sent
    // as quoted strings, which enums reject).
    let mutation =
        "mutation($envelopes: [SyncEnvelopeInput!]!) { pushSyncEnvelopes(envelopes: $envelopes) }";
    let vars = serde_json::json!({ "envelopes": envelopes_val });

    let data = client.query(mutation, Some(&vars)).await?;
    let result = &data["pushSyncEnvelopes"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(result),
        _ => {
            println!("Accepted: {}", result.as_bool().unwrap_or(false));
        }
    }

    Ok(())
}

async fn poll(
    channel_id: &str,
    ack: i64,
    latest: i64,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    // outboxAck and outboxLatest are required (Int!) on the server.
    let args = format!(
        r#"channelId: "{id}", outboxAck: {ack}, outboxLatest: {latest}"#,
        id = channel_id.replace('"', r#"\""#)
    );

    let query = format!(
        r#"{{ pollSyncEnvelopes({args}) {{ ackOrdinal hasMore envelopes {{ type channelMeta {{ id }} key dependsOn cursor {{ remoteName cursorOrdinal lastSyncedAtUtcMs }} operations {{ operation {{ id index timestampUtcMs hash skip error action {{ id type input scope timestampUtcMs }} }} context {{ documentId documentType scope branch ordinal }} }} }} deadLetters {{ documentId error errorType jobId branch scopes operationCount }} }} }}"#
    );

    let data = client.query(&query, None).await?;
    let result = &data["pollSyncEnvelopes"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(result),
        _ => {
            let envelopes = result["envelopes"].as_array().map(|a| a.len()).unwrap_or(0);
            let dead = result["deadLetters"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            println!("Channel:     {channel_id}");
            println!("Ack ordinal: {}", result["ackOrdinal"]);
            println!(
                "Has more:    {}",
                result["hasMore"].as_bool().unwrap_or(false)
            );
            println!("Envelopes:   {envelopes}");
            if dead > 0 {
                println!("Dead letters: {dead}");
                print_json(&result["deadLetters"]);
            }
            if envelopes > 0 {
                println!();
                print_json(&result["envelopes"]);
            }
        }
    }

    Ok(())
}
