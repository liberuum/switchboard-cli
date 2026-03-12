use anyhow::Result;
use clap::Args;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Args)]
pub struct OpsArgs {
    /// Document ID or name
    pub doc_id: String,

    /// Drive ID or slug (omit to auto-detect)
    #[arg(long)]
    pub drive: Option<String>,

    /// Number of operations to skip
    #[arg(long, default_value = "0")]
    pub skip: usize,

    /// Maximum number of operations to show (default: all)
    #[arg(long)]
    pub first: Option<usize>,
}

pub async fn run(args: OpsArgs, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let doc_id = match &args.drive {
        Some(d) => helpers::resolve_doc(&client, &format!("{d}/{}", args.doc_id)).await?,
        None => helpers::resolve_doc(&client, &args.doc_id).await?,
    };

    let limit = args.first.unwrap_or(1000);
    let offset = args.skip;

    let query = format!(
        r#"{{ documentOperations(filter: {{ documentId: "{doc_id}" }}, paging: {{ limit: {limit}, offset: {offset} }}) {{ items {{ id index action {{ type input scope }} timestampUtcMs hash skip error }} totalCount }} }}"#,
        doc_id = doc_id.replace('"', r#"\""#),
    );
    let data = client.query(&query, None).await?;
    let items = data
        .pointer("/documentOperations/items")
        .and_then(|v| v.as_array());
    let total = data
        .pointer("/documentOperations/totalCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let all_ops =
        items.ok_or_else(|| anyhow::anyhow!("No operations found for document {}", doc_id))?;

    if all_ops.is_empty() {
        println!("No operations found.");
        return Ok(());
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::to_value(all_ops)?);
        }
        _ => {
            let rows: Vec<Vec<String>> = all_ops
                .iter()
                .map(|op| {
                    let index = op["index"]
                        .as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let op_type = op
                        .pointer("/action/type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-")
                        .to_string();
                    let scope = op
                        .pointer("/action/scope")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-")
                        .to_string();
                    let timestamp = op["timestampUtcMs"]
                        .as_str()
                        .or_else(|| op["timestampUtcMs"].as_u64().map(|_| ""))
                        .unwrap_or("-")
                        .to_string();
                    let input_cell = format_input_cell(get_op_input(op));
                    vec![index, op_type, scope, timestamp, input_cell]
                })
                .collect();

            print_table(&["Index", "Type", "Scope", "Timestamp", "Input"], &rows);

            let displayed = all_ops.len();
            if displayed < total {
                println!("Showing {displayed} of {total} operations");
            } else {
                println!("{total} operations");
            }
        }
    }

    Ok(())
}

/// Extract the input data from an operation — from action.input.
fn get_op_input(op: &Value) -> Option<&Value> {
    op.pointer("/action/input").filter(|v| !v.is_null())
}

/// Format input data as multi-line cell content: one `field: value` per line.
fn format_input_cell(input: Option<&Value>) -> String {
    let Some(input) = input else {
        return "-".to_string();
    };

    // inputText is a JSON-encoded string — parse it into an object
    let parsed;
    let map = match input {
        Value::Object(map) => map,
        Value::String(s) => {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(s) {
                parsed = m;
                &parsed
            } else {
                // Raw string value (e.g. SET_NAME's "Test Profile")
                return s.clone();
            }
        }
        _ => return "-".to_string(),
    };

    if map.is_empty() {
        return "-".to_string();
    }

    format_fields(map, "")
}

/// Recursively format an object's fields as `key: value` lines.
fn format_fields(map: &serde_json::Map<String, Value>, prefix: &str) -> String {
    let mut lines = Vec::new();

    for (k, v) in map {
        if v.is_null() {
            continue;
        }
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };

        match v {
            Value::Object(inner) => {
                lines.push(format_fields(inner, &key));
            }
            Value::Array(arr) => {
                let items: Vec<String> = arr
                    .iter()
                    .map(|item| match item {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                lines.push(format!("{key}: [{}]", items.join(", ")));
            }
            Value::String(s) => {
                lines.push(format!("{key}: {s}"));
            }
            other => {
                lines.push(format!("{key}: {other}"));
            }
        }
    }

    lines.join("\n")
}
