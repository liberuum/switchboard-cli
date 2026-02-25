use anyhow::{Result, bail};
use clap::Args;
use colored::Colorize;
use dialoguer::Select;
use serde_json::Value;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json};

#[derive(Args)]
pub struct MutateArgs {
    /// Document ID
    pub doc_id: String,

    /// Operation name (e.g., editInvoice). Omit for interactive selection.
    pub operation: Option<String>,

    /// Input JSON for the operation
    #[arg(long)]
    pub input: Option<String>,

    /// Drive ID or slug (used to look up the document's type)
    #[arg(long)]
    pub drive: String,

    /// Interactive operation selection
    #[arg(long, short)]
    pub interactive: bool,
}

pub async fn run(
    args: MutateArgs,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve slug to UUID — model-specific queries require the actual drive UUID
    let drive_id = helpers::resolve_drive_id(&client, &args.drive).await?;

    // We need to figure out the document's type to know which mutations to offer.
    // Try each model's getDocument to find the doc.
    let mut doc_type: Option<String> = None;

    for model in cache.models.values() {
        if !model.query_fields.iter().any(|f| f == "getDocument") {
            continue;
        }

        let query = format!(
            r#"{{ {prefix} {{ getDocument(docId: "{doc_id}", driveId: "{drive_id}") {{ documentType }} }} }}"#,
            prefix = model.prefix,
            doc_id = args.doc_id.replace('"', r#"\""#),
        );

        if let Ok(data) = client.query(&query, None).await {
            if let Some(dt) = data
                .get(&model.prefix)
                .and_then(|v| v.get("getDocument"))
                .and_then(|v| v.get("documentType"))
                .and_then(|v| v.as_str())
            {
                doc_type = Some(dt.to_string());
                break;
            }
        }
    }

    let doc_type =
        doc_type.ok_or_else(|| anyhow::anyhow!("Could not determine document type for {}", args.doc_id))?;

    let model = cache.find_model(&doc_type).ok_or_else(|| {
        anyhow::anyhow!("No model found for type {doc_type}. Run `switchboard introspect`.")
    })?;

    // Get available operations (exclude createDocument)
    let operations: Vec<_> = model
        .operations
        .iter()
        .filter(|op| op.operation != "createDocument")
        .collect();

    if operations.is_empty() {
        bail!("No mutations available for type {doc_type}");
    }

    // Select operation
    let operation = if args.interactive || args.operation.is_none() {
        let op_names: Vec<&str> = operations.iter().map(|op| op.operation.as_str()).collect();
        let selection = Select::new()
            .with_prompt("Select operation")
            .items(&op_names)
            .interact()?;
        &operations[selection]
    } else {
        let op_name = args.operation.as_ref().unwrap();
        operations
            .iter()
            .find(|op| op.operation == *op_name)
            .ok_or_else(|| anyhow::anyhow!("Unknown operation '{op_name}' for type {doc_type}"))?
    };

    // Get input JSON
    let input_json = match args.input {
        Some(ref input) => input.clone(),
        None => {
            // Show what args are expected
            let input_args: Vec<_> = operation
                .args
                .iter()
                .filter(|a| a.name != "docId" && a.name != "driveId")
                .collect();

            if input_args.is_empty() {
                "{}".to_string()
            } else {
                println!("Expected input fields:");
                for arg in &input_args {
                    let req = if arg.required { " (required)" } else { "" };
                    println!("  {} : {}{}", arg.name, arg.type_name, req);
                }

                dialoguer::Input::new()
                    .with_prompt("Input JSON")
                    .interact_text()?
            }
        }
    };

    // Validate input is valid JSON
    let input_value: Value = serde_json::from_str(&input_json)
        .map_err(|e| anyhow::anyhow!("Invalid input JSON: {e}"))?;

    // Build mutation
    // Check if this operation takes `input` as an argument or uses direct args
    let has_input_arg = operation.args.iter().any(|a| a.name == "input");

    // Convert JSON to GraphQL literal (unquoted keys)
    let gql_input = json_to_graphql(&input_value);

    let mutation = if has_input_arg {
        format!(
            r#"mutation {{ {full_name}(docId: "{doc_id}", input: {gql_input}) }}"#,
            full_name = operation.full_name,
            doc_id = args.doc_id.replace('"', r#"\""#),
        )
    } else {
        // Spread the input object as direct arguments
        let args_str = if let Value::Object(map) = &input_value {
            map.iter()
                .map(|(k, v)| format!("{k}: {}", json_to_graphql(v)))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            String::new()
        };

        let extra = if args_str.is_empty() {
            String::new()
        } else {
            format!(", {args_str}")
        };

        format!(
            r#"mutation {{ {full_name}(docId: "{doc_id}"{extra}) }}"#,
            full_name = operation.full_name,
            doc_id = args.doc_id.replace('"', r#"\""#),
        )
    };

    println!(
        "Running: {}",
        format!(
            "{}(docId: \"{}\")",
            operation.full_name,
            &args.doc_id[..args.doc_id.len().min(12)]
        )
        .dimmed()
    );

    let data = client.query(&mutation, None).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!("{} Mutation applied.", "✓".green());
        }
    }

    Ok(())
}

/// Convert a serde_json Value to GraphQL literal syntax.
/// GraphQL uses unquoted keys in objects: {name: "value", count: 42}
/// vs JSON which quotes keys: {"name": "value", "count": 42}
fn json_to_graphql(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_graphql).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", json_to_graphql(v)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
    }
}
