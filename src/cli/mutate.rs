use anyhow::{Result, bail};
use clap::Args;
use colored::Colorize;
use dialoguer::Select;
use serde_json::Value;

use crate::cli::field_editor;
use crate::cli::helpers;
use crate::graphql::GraphQLClient;
use crate::output::{OutputFormat, print_json};

#[derive(Args)]
pub struct MutateArgs {
    /// Document ID or name
    pub doc_id: String,

    /// Operation name (e.g., editInvoice). Omit for interactive selection.
    pub operation: Option<String>,

    /// Input JSON for the operation
    #[arg(long)]
    pub input: Option<String>,

    /// Drive ID or slug (omit for interactive selection)
    #[arg(long)]
    pub drive: Option<String>,

    /// Interactive operation selection
    #[arg(long, short)]
    pub interactive: bool,
}

pub async fn run(args: MutateArgs, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve doc (name or UUID) and drive.
    // When --drive is given, use "drive/doc" format so name resolution is scoped.
    let (resolved_doc_id, drive_id) = match &args.drive {
        Some(d) => helpers::resolve_doc(&client, &format!("{d}/{}", args.doc_id)).await?,
        None => helpers::resolve_doc(&client, &args.doc_id).await?,
    };

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
            doc_id = resolved_doc_id.replace('"', r#"\""#),
        );

        if let Ok(data) = client.query(&query, None).await
            && let Some(dt) = data
                .get(&model.prefix)
                .and_then(|v| v.get("getDocument"))
                .and_then(|v| v.get("documentType"))
                .and_then(|v| v.as_str())
        {
            doc_type = Some(dt.to_string());
            break;
        }
    }

    let doc_type = doc_type.ok_or_else(|| {
        anyhow::anyhow!("Could not determine document type for {}", resolved_doc_id)
    })?;

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
    let operation = if let Some(ref op_name) = args.operation.filter(|_| !args.interactive) {
        operations
            .iter()
            .find(|op| op.operation == *op_name)
            .ok_or_else(|| anyhow::anyhow!("Unknown operation '{op_name}' for type {doc_type}"))?
    } else {
        let op_names: Vec<&str> = operations.iter().map(|op| op.operation.as_str()).collect();
        let selection = Select::new()
            .with_prompt("Select operation")
            .items(&op_names)
            .interact()?;
        &operations[selection]
    };

    // Get input JSON + optional schema for enum-aware serialization
    let (input_value, input_schema): (Value, Option<Vec<field_editor::InputField>>) =
        match args.input {
            Some(ref input) => {
                let v = serde_json::from_str(input)
                    .map_err(|e| anyhow::anyhow!("Invalid input JSON: {e}"))?;
                (v, None)
            }
            None => {
                let input_args: Vec<_> = operation
                    .args
                    .iter()
                    .filter(|a| a.name != "docId" && a.name != "driveId")
                    .collect();

                if input_args.is_empty() {
                    (Value::Object(serde_json::Map::new()), None)
                } else {
                    // Try field-by-field editor for the "input" arg
                    let input_arg = input_args.iter().find(|a| a.name == "input");
                    match try_field_editor(
                        &client,
                        input_arg,
                        model.prefix.as_str(),
                        &resolved_doc_id,
                        &drive_id,
                    )
                    .await
                    {
                        Ok(Some((val, schema))) => (val, Some(schema)),
                        Ok(None) => {
                            // User cancelled or no changes
                            println!("No changes. Aborted.");
                            return Ok(());
                        }
                        Err(_) => {
                            // Fallback to raw JSON input
                            eprintln!(
                                "{}",
                                "Could not introspect input type — falling back to raw JSON input."
                                    .yellow()
                            );
                            println!("Expected input fields:");
                            for arg in &input_args {
                                let req = if arg.required { " (required)" } else { "" };
                                println!("  {} : {}{}", arg.name, arg.type_name, req);
                            }
                            let input_json: String = dialoguer::Input::new()
                                .with_prompt("Input JSON")
                                .interact_text()?;
                            let v = serde_json::from_str(&input_json)
                                .map_err(|e| anyhow::anyhow!("Invalid input JSON: {e}"))?;
                            (v, None)
                        }
                    }
                }
            }
        };

    // Build mutation
    // Check if this operation takes `input` as an argument or uses direct args
    let has_input_arg = operation.args.iter().any(|a| a.name == "input");

    // Convert JSON to GraphQL literal — use schema-aware version when available
    // so enum values are sent unquoted
    let gql_input = match &input_schema {
        Some(schema) => field_editor::json_to_graphql_with_schema(&input_value, schema),
        None => helpers::json_to_graphql(&input_value),
    };

    let mutation = if has_input_arg {
        format!(
            r#"mutation {{ {full_name}(docId: "{doc_id}", input: {gql_input}) }}"#,
            full_name = operation.full_name,
            doc_id = resolved_doc_id.replace('"', r#"\""#),
        )
    } else {
        // Spread the input object as direct arguments
        let args_str = if let Value::Object(map) = &input_value {
            map.iter()
                .map(|(k, v)| format!("{k}: {}", helpers::json_to_graphql(v)))
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
            doc_id = resolved_doc_id.replace('"', r#"\""#),
        )
    };

    println!(
        "Running: {}",
        format!(
            "{}(docId: \"{}\")",
            operation.full_name,
            &resolved_doc_id[..resolved_doc_id.len().min(12)]
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

/// Attempt to use the field-by-field editor for an operation's input type.
/// Returns Ok(Some((value, schema))) if the user provided input, Ok(None) if no changes.
async fn try_field_editor(
    client: &GraphQLClient,
    input_arg: Option<&&crate::graphql::introspection::OperationArg>,
    prefix: &str,
    doc_id: &str,
    drive_id: &str,
) -> Result<Option<(Value, Vec<field_editor::InputField>)>> {
    let input_arg = input_arg.ok_or_else(|| anyhow::anyhow!("No input arg found"))?;
    let type_name = &input_arg.type_name;

    // Fetch the input type schema via __type introspection
    let fields = field_editor::fetch_input_type_schema(client, type_name).await?;
    if fields.is_empty() {
        bail!("No fields found for input type {type_name}");
    }

    // Fetch current document state for pre-population
    let state = match field_editor::fetch_document_state(client, prefix, doc_id, drive_id).await {
        Ok(s) => s,
        Err(_) => {
            eprintln!(
                "{}",
                "Could not fetch current state — prompting without defaults.".yellow()
            );
            Value::Object(serde_json::Map::new())
        }
    };

    // Show fields with current state, let user pick which to edit, then prompt
    let input = field_editor::select_and_prompt_fields(&fields, &state)?;

    if input.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return Ok(None);
    }

    // Confirm before executing
    if !field_editor::confirm_input(&input)? {
        return Ok(None);
    }

    Ok(Some((input, fields)))
}
