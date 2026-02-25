use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json};

#[derive(Args)]
pub struct QueryArgs {
    /// GraphQL query string
    pub query: Option<String>,

    /// Path to a .graphql file
    #[arg(long)]
    pub file: Option<String>,

    /// Variables as JSON string
    #[arg(long)]
    pub variables: Option<String>,
}

pub async fn run(args: QueryArgs, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let query_str = match (&args.query, &args.file) {
        (Some(q), _) => q.clone(),
        (None, Some(f)) => std::fs::read_to_string(f)
            .map_err(|e| anyhow::anyhow!("Failed to read file {f}: {e}"))?,
        (None, None) => {
            anyhow::bail!("Provide a query string or --file path");
        }
    };

    let variables: Option<Value> = match &args.variables {
        Some(v) => Some(serde_json::from_str(v)?),
        None => None,
    };

    let data = client.query(&query_str, variables.as_ref()).await?;

    match format {
        OutputFormat::Raw | OutputFormat::Json => print_json(&data),
        OutputFormat::Table => {
            // For raw queries, just pretty-print the JSON since we don't know the shape
            print_json(&data);
        }
    }

    Ok(())
}
