use anyhow::Result;
use clap::Subcommand;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum ModelsCommand {
    /// List all discovered document models
    List,
    /// Show details and operations for a specific model
    Get {
        /// Document type (e.g., powerhouse/invoice) or prefix (e.g., Invoice)
        r#type: String,
    },
}

pub async fn run(
    cmd: ModelsCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    match cmd {
        ModelsCommand::List => list(format, profile_name),
        ModelsCommand::Get { r#type } => get(&r#type, format, profile_name),
    }
}

fn list(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, _client, cache) = helpers::setup_with_cache(profile_name)?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let models: Vec<_> = cache
                .models
                .iter()
                .map(|(doc_type, model)| {
                    serde_json::json!({
                        "type": doc_type,
                        "prefix": model.prefix,
                        "operations": model.operations.len() - 1, // exclude createDocument
                    })
                })
                .collect();
            print_json(&serde_json::Value::Array(models));
        }
        _ => {
            if cache.models.is_empty() {
                println!("No document models found. Run `switchboard introspect`.");
                return Ok(());
            }

            let rows: Vec<Vec<String>> = cache
                .models
                .iter()
                .map(|(doc_type, model)| {
                    let op_count = model.operations.len().saturating_sub(1);
                    vec![doc_type.clone(), model.prefix.clone(), op_count.to_string()]
                })
                .collect();
            print_table(&["Type", "Prefix", "Operations"], &rows);
        }
    }

    Ok(())
}

fn get(type_or_prefix: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, _client, cache) = helpers::setup_with_cache(profile_name)?;

    let model = cache
        .find_model(type_or_prefix)
        .ok_or_else(|| anyhow::anyhow!("Unknown model: {type_or_prefix}"))?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::to_value(model)?);
        }
        _ => {
            println!("Type:   {}", model.document_type);
            println!("Prefix: {}", model.prefix);
            println!();
            println!("Mutations:");
            for op in &model.operations {
                let args: Vec<String> = op
                    .args
                    .iter()
                    .map(|a| {
                        let req = if a.required { "!" } else { "" };
                        format!("{}: {}{}", a.name, a.type_name, req)
                    })
                    .collect();
                println!("  {}({})", op.full_name, args.join(", "));
            }

            if !model.query_fields.is_empty() {
                println!();
                println!("Queries:");
                for field in &model.query_fields {
                    println!("  {}.{}", model.prefix, field);
                }
            }
        }
    }

    Ok(())
}
