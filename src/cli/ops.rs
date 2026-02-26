use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Args)]
pub struct OpsArgs {
    /// Document ID
    pub doc_id: String,

    /// Drive ID or slug
    #[arg(long)]
    pub drive: String,

    /// Number of operations to skip
    #[arg(long, default_value = "0")]
    pub skip: usize,

    /// Maximum number of operations to show (default: all)
    #[arg(long)]
    pub first: Option<usize>,
}

pub async fn run(args: OpsArgs, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve slug to UUID — model-specific queries require the actual drive UUID
    let drive_id = helpers::resolve_drive_id(&client, &args.drive).await?;

    // Try each model namespace to find the document and its operations
    let mut ops: Option<Vec<Value>> = None;

    for model in cache.models.values() {
        if !model.query_fields.iter().any(|f| f == "getDocument") {
            continue;
        }

        let query = format!(
            r#"{{ {prefix} {{ getDocument(docId: "{doc_id}", driveId: "{drive_id}") {{ operations {{ id type index timestampUtcMs hash skip }} }} }} }}"#,
            prefix = model.prefix,
            doc_id = args.doc_id.replace('"', r#"\""#),
        );

        if let Ok(data) = client.query(&query, None).await
            && let Some(operations) = data
                .get(&model.prefix)
                .and_then(|v| v.get("getDocument"))
                .and_then(|v| v.get("operations"))
                .and_then(|v| v.as_array())
                .filter(|operations| !operations.is_empty())
        {
            ops = Some(operations.clone());
            break;
        }
    }

    let all_ops =
        ops.ok_or_else(|| anyhow::anyhow!("No operations found for document {}", args.doc_id))?;

    let total = all_ops.len();
    let displayed: Vec<&Value> = match args.first {
        Some(n) => all_ops.iter().skip(args.skip).take(n).collect(),
        None => all_ops.iter().skip(args.skip).collect(),
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let slice: Vec<&Value> = displayed.into_iter().collect();
            print_json(&serde_json::to_value(slice)?);
        }
        OutputFormat::Table => {
            if displayed.is_empty() {
                println!("No operations found.");
                return Ok(());
            }

            let rows: Vec<Vec<String>> = displayed
                .iter()
                .map(|op| {
                    let index = op["index"]
                        .as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let op_type = op["type"].as_str().unwrap_or("-").to_string();
                    let timestamp = op["timestampUtcMs"]
                        .as_str()
                        .or_else(|| op["timestampUtcMs"].as_u64().map(|_| ""))
                        .unwrap_or("-")
                        .to_string();
                    let hash = op["hash"].as_str().unwrap_or("-").to_string();
                    vec![index, op_type, timestamp, hash]
                })
                .collect();

            print_table(&["Index", "Type", "Timestamp", "Hash"], &rows);
            if rows.len() < total {
                println!("Showing {} of {total} operations", rows.len());
            } else {
                println!("{total} operations");
            }
        }
    }

    Ok(())
}
