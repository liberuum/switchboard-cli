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

    /// Read input JSON from a file (avoids shell escaping issues)
    #[arg(long, value_name = "FILE")]
    pub input_file: Option<String>,

    /// Drive ID or slug (omit for interactive selection)
    #[arg(long)]
    pub drive: Option<String>,

    /// Interactive operation selection
    #[arg(long, short)]
    pub interactive: bool,
}

pub async fn run(args: MutateArgs, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve doc identifier (name or UUID).
    // When --drive is given, use "drive/doc" format so name resolution is scoped.
    let doc_identifier = match &args.drive {
        Some(d) => helpers::resolve_doc(&client, &format!("{d}/{}", args.doc_id)).await?,
        None => helpers::resolve_doc(&client, &args.doc_id).await?,
    };

    // Query the document directly to get its type and actual PHID
    let doc_query = format!(
        r#"{{ document(identifier: "{id}") {{ document {{ id documentType }} }} }}"#,
        id = doc_identifier.replace('"', r#"\""#)
    );
    let doc_data = client.query(&doc_query, None).await?;

    let doc_type = doc_data
        .pointer("/document/document/documentType")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!("Could not determine document type for {}", doc_identifier)
        })?
        .to_string();

    // Get the actual PHID (UUID) — mutations use docId, not identifier/slug
    let resolved_doc_id = doc_data
        .pointer("/document/document/id")
        .and_then(|v| v.as_str())
        .unwrap_or(&doc_identifier)
        .to_string();

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

    // Get input JSON — prefer --input-file over --input to avoid shell escaping issues
    let input_from_file = match &args.input_file {
        Some(path) if path == "-" => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow::anyhow!("Failed to read stdin: {e}"))?;
            Some(buf)
        }
        Some(path) => {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("Failed to read input file '{path}': {e}"))?;
            Some(contents)
        }
        None => None,
    };
    let effective_input = input_from_file.as_deref().or(args.input.as_deref());

    let input_value: Value = match effective_input {
            Some(input) => {
                serde_json::from_str(input)
                    .map_err(|e| anyhow::anyhow!("Invalid input JSON: {e}"))?
            }
            None => {
                let input_args: Vec<_> = operation
                    .args
                    .iter()
                    .filter(|a| a.name != "docId" && a.name != "driveId")
                    .collect();

                if input_args.is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    // Try field-by-field editor for the "input" arg
                    let input_arg = input_args.iter().find(|a| a.name == "input");
                    match try_field_editor(&client, input_arg, &doc_identifier).await {
                        Ok(Some((val, _schema))) => val,
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
                            serde_json::from_str(&input_json)
                                .map_err(|e| anyhow::anyhow!("Invalid input JSON: {e}"))?
                        }
                    }
                }
            }
        };

    // Build mutation using GraphQL variables to avoid string-interpolation issues
    // (newlines, special chars in values get properly serialized by serde)
    let has_input_arg = operation.args.iter().any(|a| a.name == "input");

    // All typed mutations return *MutationResult which requires a selection set
    let selection = "{ id name }";

    let (mutation, variables) = if has_input_arg {
        // Find the input type name from the operation args
        let input_type = operation
            .args
            .iter()
            .find(|a| a.name == "input")
            .map(|a| &a.type_name)
            .unwrap();
        let required = operation
            .args
            .iter()
            .find(|a| a.name == "input")
            .is_some_and(|a| a.required);
        let bang = if required { "!" } else { "" };

        let query = format!(
            "mutation($docId: PHID!, $input: {input_type}{bang}) {{ {name}(docId: $docId, input: $input) {selection} }}",
            name = operation.full_name,
        );
        let vars = serde_json::json!({
            "docId": resolved_doc_id,
            "input": input_value,
        });
        (query, vars)
    } else {
        // Direct args — build variable declarations and references dynamically
        let mut var_decls = vec!["$docId: PHID!".to_string()];
        let mut arg_refs = vec!["docId: $docId".to_string()];
        let mut vars = serde_json::Map::new();
        vars.insert("docId".into(), Value::String(resolved_doc_id.clone()));

        if let Value::Object(map) = &input_value {
            for (key, val) in map {
                // Find the arg type from the operation definition
                let arg_type = operation
                    .args
                    .iter()
                    .find(|a| a.name == *key)
                    .map(|a| a.type_name.as_str())
                    .unwrap_or("String");
                let required = operation
                    .args
                    .iter()
                    .find(|a| a.name == *key)
                    .is_some_and(|a| a.required);
                let bang = if required { "!" } else { "" };

                var_decls.push(format!("${key}: {arg_type}{bang}"));
                arg_refs.push(format!("{key}: ${key}"));
                vars.insert(key.clone(), val.clone());
            }
        }

        let query = format!(
            "mutation({decls}) {{ {name}({args}) {selection} }}",
            decls = var_decls.join(", "),
            name = operation.full_name,
            args = arg_refs.join(", "),
        );
        (query, Value::Object(vars))
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

    let data = client.query(&mutation, Some(&variables)).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        _ => {
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
    doc_identifier: &str,
) -> Result<Option<(Value, Vec<field_editor::InputField>)>> {
    let input_arg = input_arg.ok_or_else(|| anyhow::anyhow!("No input arg found"))?;
    let type_name = &input_arg.type_name;

    // Fetch the input type schema via __type introspection
    let fields = field_editor::fetch_input_type_schema(client, type_name).await?;
    if fields.is_empty() {
        bail!("No fields found for input type {type_name}");
    }

    // Fetch current document state for pre-population
    let state = match field_editor::fetch_document_state(client, doc_identifier).await {
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
