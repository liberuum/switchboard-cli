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
        #[arg(long)]
        ack: Option<i64>,
        /// Latest known outbox sequence number
        #[arg(long)]
        latest: Option<i64>,
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
    let gql_input = helpers::json_to_graphql(&input_val);

    let mutation =
        format!(r#"mutation {{ touchChannel(input: {gql_input}) {{ id name status }} }}"#);

    let data = client.query(&mutation, None).await?;
    let channel = &data["touchChannel"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(channel),
        OutputFormat::Table => {
            println!("Channel:  {}", channel["id"].as_str().unwrap_or("-"));
            println!("Name:     {}", channel["name"].as_str().unwrap_or("-"));
            println!("Status:   {}", channel["status"].as_str().unwrap_or("-"));
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
    let gql_envelopes = helpers::json_to_graphql(&envelopes_val);

    let mutation = format!(
        r#"mutation {{ pushSyncEnvelopes(envelopes: {gql_envelopes}) {{ status acknowledged }} }}"#
    );

    let data = client.query(&mutation, None).await?;
    let result = &data["pushSyncEnvelopes"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(result),
        OutputFormat::Table => {
            println!("Status:       {}", result["status"].as_str().unwrap_or("-"));
            println!("Acknowledged: {}", result["acknowledged"]);
        }
    }

    Ok(())
}

async fn poll(
    channel_id: &str,
    ack: Option<i64>,
    latest: Option<i64>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let mut args = format!(
        r#"channelId: "{id}""#,
        id = channel_id.replace('"', r#"\""#)
    );
    if let Some(a) = ack {
        args.push_str(&format!(", outboxAck: {a}"));
    }
    if let Some(l) = latest {
        args.push_str(&format!(", outboxLatest: {l}"));
    }

    let query =
        format!(r#"{{ pollSyncEnvelopes({args}) {{ channelId envelopes {{ id data }} }} }}"#);

    let data = client.query(&query, None).await?;
    let result = &data["pollSyncEnvelopes"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(result),
        OutputFormat::Table => {
            let channel = result["channelId"].as_str().unwrap_or("-");
            let envelopes = result["envelopes"].as_array().map(|a| a.len()).unwrap_or(0);
            println!("Channel:   {channel}");
            println!("Envelopes: {envelopes}");
            if envelopes > 0 {
                println!();
                print_json(&result["envelopes"]);
            }
        }
    }

    Ok(())
}
