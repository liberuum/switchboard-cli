use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Input, Select};
use serde_json::Value;

use crate::cli::helpers::{self, resolve_drive_id, truncate};
use crate::cli::mutate;
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum DocsCommand {
    /// List documents in a drive
    List {
        /// Drive ID or slug
        #[arg(long)]
        drive: String,
        /// Filter by document type
        #[arg(long, short = 't')]
        r#type: Option<String>,
    },
    /// Get a document by ID
    Get {
        /// Document ID
        id: String,
        /// Drive ID or slug (required for model-specific query)
        #[arg(long)]
        drive: String,
    },
    /// Show hierarchical file tree of a drive
    Tree {
        /// Drive ID or slug
        #[arg(long)]
        drive: String,
    },
    /// Create a new document (interactive)
    Create {
        /// Document type (e.g., powerhouse/invoice)
        #[arg(long, short = 't')]
        r#type: Option<String>,
        /// Document name
        #[arg(long)]
        name: Option<String>,
        /// Drive ID or slug
        #[arg(long)]
        drive: Option<String>,
    },
    /// Delete a document
    Delete {
        /// Document ID
        id: String,
        /// Skip confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Mutate a document (apply operations)
    Mutate(mutate::MutateArgs),
}

pub async fn run(
    cmd: DocsCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    match cmd {
        DocsCommand::List { drive, r#type } => list(&drive, r#type.as_deref(), format, profile_name).await,
        DocsCommand::Get { id, drive } => get(&id, &drive, format, profile_name).await,
        DocsCommand::Tree { drive } => tree(&drive, format, profile_name).await,
        DocsCommand::Create {
            r#type,
            name,
            drive,
        } => create(r#type, name, drive, format, profile_name).await,
        DocsCommand::Delete { id, yes } => delete(&id, yes, profile_name).await,
        DocsCommand::Mutate(args) => mutate::run(args, format, profile_name).await,
    }
}

async fn list(
    drive: &str,
    doc_type: Option<&str>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    // Get the drive's node tree to list all documents
    let drive_query = format!(
        r#"{{
  driveDocument(idOrSlug: "{drive}") {{
    id name
    state {{
      nodes {{
        ... on DocumentDrive_FileNode {{ id name kind documentType parentFolder }}
        ... on DocumentDrive_FolderNode {{ id name kind parentFolder }}
      }}
    }}
  }}
}}"#,
        drive = drive.replace('"', r#"\""#)
    );

    let data = client.query(&drive_query, None).await?;
    let nodes = data
        .pointer("/driveDocument/state/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Filter to files (optionally by type)
    let files: Vec<&Value> = nodes
        .iter()
        .filter(|n| n["kind"].as_str() == Some("file"))
        .filter(|n| {
            if let Some(dt) = doc_type {
                n["documentType"].as_str() == Some(dt)
            } else {
                true
            }
        })
        .collect();

    let folders: Vec<&Value> = nodes
        .iter()
        .filter(|n| n["kind"].as_str() == Some("folder"))
        .collect();

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let all: Vec<&Value> = nodes.iter().collect();
            print_json(&serde_json::to_value(all)?);
        }
        OutputFormat::Table => {
            if files.is_empty() && folders.is_empty() {
                println!("No documents found in drive '{drive}'.");
                return Ok(());
            }

            if !files.is_empty() {
                let rows: Vec<Vec<String>> = files
                    .iter()
                    .map(|f| {
                        vec![
                            truncate(f["id"].as_str().unwrap_or("-"), 24),
                            f["name"].as_str().unwrap_or("-").to_string(),
                            f["documentType"].as_str().unwrap_or("-").to_string(),
                        ]
                    })
                    .collect();
                print_table(&["ID", "Name", "Type"], &rows);
            }

            if !folders.is_empty() {
                println!("\nFolders:");
                for folder in &folders {
                    println!(
                        "  {} {}/",
                        "\u{1F4C1}",
                        folder["name"].as_str().unwrap_or("-")
                    );
                }
            }
        }
    }

    Ok(())
}

async fn get(
    id: &str,
    drive: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve slug to UUID — model-specific queries require the actual drive UUID
    let drive_id = resolve_drive_id(&client, drive).await?;

    // Try each model namespace to find the document
    // We need to try model-specific queries since that's how the API works
    let mut doc: Option<Value> = None;

    for model in cache.models.values() {
        if !model.query_fields.iter().any(|f| f == "getDocument") {
            continue;
        }

        let query = format!(
            r#"{{ {prefix} {{ getDocument(docId: "{id}", driveId: "{drive_id}") {{ id name documentType revision stateJSON }} }} }}"#,
            prefix = model.prefix,
            id = id.replace('"', r#"\""#),
        );

        match client.query(&query, None).await {
            Ok(data) => {
                if let Some(d) = data
                    .get(&model.prefix)
                    .and_then(|v| v.get("getDocument"))
                {
                    if !d.is_null() {
                        doc = Some(d.clone());
                        break;
                    }
                }
            }
            Err(_) => continue,
        }
    }

    let doc = doc.ok_or_else(|| anyhow::anyhow!("Document '{id}' not found in any model namespace"))?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&doc),
        OutputFormat::Table => {
            println!("ID:       {}", doc["id"].as_str().unwrap_or("-"));
            println!("Name:     {}", doc["name"].as_str().unwrap_or("-"));
            println!("Type:     {}", doc["documentType"].as_str().unwrap_or("-"));
            println!("Revision: {}", doc["revision"]);

            if let Some(state_json) = doc.get("stateJSON") {
                // stateJSON is a JSON string — parse and pretty-print it
                match state_json.as_str() {
                    Some(s) => {
                        if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                            println!("\nState:");
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&parsed).unwrap_or_default()
                            );
                        } else {
                            println!("\nState (raw): {s}");
                        }
                    }
                    None => {
                        println!("\nState:");
                        print_json(state_json);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn tree(drive: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let query = format!(
        r#"{{
  driveDocument(idOrSlug: "{drive}") {{
    name
    state {{
      nodes {{
        ... on DocumentDrive_FileNode {{ id name kind documentType parentFolder }}
        ... on DocumentDrive_FolderNode {{ id name kind parentFolder }}
      }}
    }}
  }}
}}"#,
        drive = drive.replace('"', r#"\""#)
    );

    let data = client.query(&query, None).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&data["driveDocument"]);
        }
        OutputFormat::Table => {
            let drive_name = data
                .pointer("/driveDocument/name")
                .and_then(|v| v.as_str())
                .unwrap_or(drive);

            let nodes = data
                .pointer("/driveDocument/state/nodes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            println!("{}/", drive_name);
            print_tree(&nodes, None, "");
        }
    }

    Ok(())
}

pub fn print_tree(nodes: &[Value], parent: Option<&str>, indent: &str) {
    let children: Vec<&Value> = nodes
        .iter()
        .filter(|n| {
            let pf = n["parentFolder"].as_str();
            match parent {
                None => pf.is_none() || pf == Some(""),
                Some(p) => pf == Some(p),
            }
        })
        .collect();

    for (i, child) in children.iter().enumerate() {
        let is_last = i == children.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_indent = if is_last { "    " } else { "│   " };

        let name = child["name"].as_str().unwrap_or("-");
        let kind = child["kind"].as_str().unwrap_or("file");
        let id = child["id"].as_str().unwrap_or("");

        if kind == "folder" {
            println!("{indent}{connector}\u{1F4C1} {name}/");
            print_tree(nodes, Some(id), &format!("{indent}{child_indent}"));
        } else {
            let doc_type = child["documentType"].as_str().unwrap_or("");
            if doc_type.is_empty() {
                println!("{indent}{connector}{name}");
            } else {
                println!("{indent}{connector}{name} ({doc_type})");
            }
        }
    }
}

async fn create(
    doc_type: Option<String>,
    name: Option<String>,
    drive: Option<String>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_pname, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    if cache.models.is_empty() {
        bail!("No document models found. Run `switchboard introspect` first.");
    }

    // Select document type
    let doc_type = match doc_type {
        Some(t) => t,
        None => {
            let types: Vec<String> = cache.models.keys().cloned().collect();
            let selection = Select::new()
                .with_prompt("Select document type")
                .items(&types)
                .interact()?;
            types[selection].clone()
        }
    };

    let model = cache
        .find_model(&doc_type)
        .ok_or_else(|| anyhow::anyhow!("Unknown document type: {doc_type}"))?;

    // Get document name
    let name = match name {
        Some(n) => n,
        None => Input::new()
            .with_prompt("Document name")
            .interact_text()?,
    };

    // Get drive
    let drive = match drive {
        Some(d) => d,
        None => Input::new()
            .with_prompt("Drive (slug or ID)")
            .interact_text()?,
    };

    // Resolve drive slug to UUID
    let drive_uuid = resolve_drive_id(&client, &drive).await?;
    if drive != drive_uuid {
        println!("Resolved slug → UUID {}", &drive_uuid[..12]);
    }

    // Build create mutation
    let mutation = format!(
        r#"mutation {{ {create_mutation}(name: "{name}", driveId: "{drive_uuid}") }}"#,
        create_mutation = model.create_mutation,
        name = name.replace('"', r#"\""#),
    );

    let data = client.query(&mutation, None).await?;
    let doc_id = data
        .get(&model.create_mutation)
        .and_then(|v| v.as_str())
        .or_else(|| {
            // Some mutations return an object
            data.get(&model.create_mutation)
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
        });

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        OutputFormat::Table => {
            println!("{} Document created", "✓".green());
            if let Some(id) = doc_id {
                println!("  ID: {id}");
            }
            println!("  Type: {doc_type}");
            println!("  Name: {name}");
        }
    }

    Ok(())
}

async fn delete(id: &str, skip_confirm: bool, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    if !skip_confirm {
        let confirm = dialoguer::Confirm::new()
            .with_prompt(format!("Delete document {id}?"))
            .default(false)
            .interact()?;
        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mutation = format!(
        r#"mutation {{ deleteDocument(id: "{id}") }}"#,
        id = id.replace('"', r#"\""#)
    );
    client.query(&mutation, None).await?;

    println!("{} Document deleted.", "✓".green());
    Ok(())
}
