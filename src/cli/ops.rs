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
    let (_name, profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve doc (accepts name or UUID) and drive.
    // When --drive is given, use "drive/doc" format so name resolution is scoped.
    let (args_doc_id, drive_id) = match &args.drive {
        Some(d) => helpers::resolve_doc(&client, &format!("{d}/{}", args.doc_id)).await?,
        None => helpers::resolve_doc(&client, &args.doc_id).await?,
    };

    // Try each model namespace to find the document and its operations.
    // Try multiple query formats: paginated with action, flat with action, flat basic.
    let mut ops: Option<Vec<Value>> = None;
    let doc_id_escaped = args_doc_id.replace('"', r#"\""#);

    // Query formats to try, in order of richness
    #[derive(Clone, Copy)]
    enum QueryFormat {
        PaginatedAction,
        FlatAction,
        FlatInputText,
        FlatBasic,
    }
    let formats = [
        QueryFormat::PaginatedAction,
        QueryFormat::FlatAction,
        QueryFormat::FlatInputText,
        QueryFormat::FlatBasic,
    ];

    'outer: for fmt in formats {
        for model in cache.models.values() {
            if !model.query_fields.iter().any(|f| f == "getDocument") {
                continue;
            }

            let query = match fmt {
                QueryFormat::PaginatedAction => format!(
                    r#"{{ {prefix} {{ getDocument(docId: "{doc_id}", driveId: "{drive_id}") {{ operations {{ items {{ id type index timestampUtcMs hash skip action {{ type input }} }} }} }} }} }}"#,
                    prefix = model.prefix,
                    doc_id = doc_id_escaped,
                ),
                QueryFormat::FlatAction => format!(
                    r#"{{ {prefix} {{ getDocument(docId: "{doc_id}", driveId: "{drive_id}") {{ operations {{ id type index timestampUtcMs hash skip action {{ type input }} }} }} }} }}"#,
                    prefix = model.prefix,
                    doc_id = doc_id_escaped,
                ),
                QueryFormat::FlatInputText => format!(
                    r#"{{ {prefix} {{ getDocument(docId: "{doc_id}", driveId: "{drive_id}") {{ operations {{ id type index timestampUtcMs hash skip inputText }} }} }} }}"#,
                    prefix = model.prefix,
                    doc_id = doc_id_escaped,
                ),
                QueryFormat::FlatBasic => format!(
                    r#"{{ {prefix} {{ getDocument(docId: "{doc_id}", driveId: "{drive_id}") {{ operations {{ id type index timestampUtcMs hash skip }} }} }} }}"#,
                    prefix = model.prefix,
                    doc_id = doc_id_escaped,
                ),
            };

            if let Ok(data) = client.query(&query, None).await {
                let get_doc = data.get(&model.prefix).and_then(|v| v.get("getDocument"));

                // Extract operations — paginated format nests under items
                let operations = get_doc
                    .and_then(|v| v.get("operations"))
                    .and_then(|v| {
                        // Try paginated: operations.items[]
                        v.get("items")
                            .and_then(|i| i.as_array())
                            // Try flat: operations[]
                            .or_else(|| v.as_array())
                    })
                    .filter(|arr| !arr.is_empty());

                if let Some(arr) = operations {
                    ops = Some(arr.clone());
                    break 'outer;
                }
            }
        }
    }

    // Fallback for drive documents: use the drive-scoped endpoint
    if ops.is_none() && args_doc_id == drive_id {
        let base_url = helpers::base_url_from(&client.url);
        let drive_client = crate::graphql::GraphQLClient::new(
            format!("{base_url}/d/{drive_id}"),
            profile.token.clone(),
        );
        if let Ok(data) = drive_client
            .query(
                r#"query ($id: String!, $first: Int, $skip: Int) {
                    document(id: $id) {
                        operations(first: $first, skip: $skip) {
                            id type index skip hash timestampUtcMs inputText
                        }
                    }
                }"#,
                Some(&serde_json::json!({
                    "id": drive_id,
                    "first": 25000,
                    "skip": 0,
                })),
            )
            .await
        {
            ops = data
                .pointer("/document/operations")
                .and_then(|v| v.as_array())
                .filter(|arr| !arr.is_empty())
                .cloned();
        }
    }

    let all_ops =
        ops.ok_or_else(|| anyhow::anyhow!("No operations found for document {}", args_doc_id))?;

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
                    let input_cell = format_input_cell(get_op_input(op));
                    vec![index, op_type, timestamp, input_cell]
                })
                .collect();

            print_table(&["Index", "Type", "Timestamp", "Input"], &rows);

            if displayed.len() < total {
                println!("Showing {} of {total} operations", displayed.len());
            } else {
                println!("{total} operations");
            }
        }
    }

    Ok(())
}

/// Extract the input data from an operation — tries action.input, then inputText.
fn get_op_input(op: &Value) -> Option<&Value> {
    // Try action.input (ReactorOperation)
    if let Some(input) = op.pointer("/action/input").filter(|v| !v.is_null()) {
        return Some(input);
    }
    // Try inputText (legacy Operation) — it's a JSON string, but we return as-is
    if let Some(input_text) = op.get("inputText").filter(|v| !v.is_null()) {
        return Some(input_text);
    }
    None
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
