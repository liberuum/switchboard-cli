use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Input, Select};
use serde_json::Value;

use crate::cli::helpers::{self, resolve_drive_id};
use crate::cli::mutate;
use crate::output::{self, OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum DocsCommand {
    /// List documents (all drives, or filtered by --drive)
    List {
        /// Drive ID or slug (omit to list all)
        #[arg(long)]
        drive: Option<String>,
        /// Filter by document type
        #[arg(long, short = 't')]
        r#type: Option<String>,
        /// Output file path (for svg/png/mermaid formats)
        #[arg(long)]
        out: Option<String>,
    },
    /// Get a document by ID or name (searches across all drives if --drive is omitted)
    Get {
        /// Document ID or name
        id: String,
        /// Drive ID or slug (narrows search to a single drive)
        #[arg(long)]
        drive: Option<String>,
        /// Include full document state in output
        #[arg(long)]
        state: bool,
    },
    /// Show hierarchical file tree of a drive
    Tree {
        /// Drive ID or slug (omit for interactive selection)
        #[arg(long)]
        drive: Option<String>,
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
    /// Delete one or more documents
    Delete {
        /// Document IDs or names
        ids: Vec<String>,
        /// Skip confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Mutate a document (apply operations)
    Mutate(mutate::MutateArgs),
}

pub async fn run(cmd: DocsCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        DocsCommand::List {
            drive,
            r#type,
            out,
        } => {
            list(
                drive.as_deref(),
                r#type.as_deref(),
                format,
                out.as_deref(),
                profile_name,
            )
            .await
        }
        DocsCommand::Get { id, drive, state } => {
            get(&id, drive.as_deref(), state, format, profile_name).await
        }
        DocsCommand::Tree { drive } => tree(drive, format, profile_name).await,
        DocsCommand::Create {
            r#type,
            name,
            drive,
        } => create(r#type, name, drive, format, profile_name).await,
        DocsCommand::Delete { ids, yes } => delete(&ids, yes, profile_name).await,
        DocsCommand::Mutate(args) => mutate::run(args, format, profile_name).await,
    }
}

async fn list(
    drive: Option<&str>,
    doc_type: Option<&str>,
    format: OutputFormat,
    out: Option<&str>,
    profile_name: Option<&str>,
) -> Result<()> {
    let (name, _profile, client) = helpers::setup(profile_name)?;

    // Collect drive IDs to query
    let drive_ids: Vec<(String, String)> = match drive {
        Some(d) => {
            // Single drive — resolve and use it
            let query = format!(
                r#"{{ driveDocument(idOrSlug: "{d}") {{ id name }} }}"#,
                d = d.replace('"', r#"\""#)
            );
            let data = client.query(&query, None).await?;
            let id = data
                .pointer("/driveDocument/id")
                .and_then(|v| v.as_str())
                .unwrap_or(d)
                .to_string();
            let name = data
                .pointer("/driveDocument/name")
                .and_then(|v| v.as_str())
                .unwrap_or(d)
                .to_string();
            vec![(id, name)]
        }
        None => {
            // All drives
            let data = client.query("{ driveDocuments { id name } }", None).await?;
            data.get("driveDocuments")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|d| {
                            let id = d["id"].as_str().unwrap_or("").to_string();
                            let name = d["name"].as_str().unwrap_or("").to_string();
                            (id, name)
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    };

    let mut all_files: Vec<Value> = Vec::new();
    let mut drive_with_nodes: Vec<(Value, Vec<Value>)> = Vec::new();
    let multiple_drives = drive_ids.len() > 1;

    for (drive_id, drive_name) in &drive_ids {
        let query = format!(
            r#"{{ driveDocument(idOrSlug: "{drive_id}") {{
                state {{
                    nodes {{
                        ... on DocumentDrive_FileNode {{ id name kind documentType parentFolder }}
                        ... on DocumentDrive_FolderNode {{ id name kind parentFolder }}
                    }}
                }}
            }} }}"#,
            drive_id = drive_id.replace('"', r#"\""#)
        );

        let data = client.query(&query, None).await?;
        let nodes = data
            .pointer("/driveDocument/state/nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Collect for visual formats
        if format.is_visual() {
            let drive_meta = serde_json::json!({
                "id": drive_id,
                "name": drive_name,
                "slug": drive_name,
                "documentType": "powerhouse/document-drive",
                "revision": 0
            });
            drive_with_nodes.push((drive_meta, nodes.clone()));
        }

        for node in nodes {
            if node["kind"].as_str() != Some("file") {
                continue;
            }
            if let Some(dt) = doc_type
                && node["documentType"].as_str() != Some(dt)
            {
                continue;
            }
            // Attach drive info for multi-drive display
            let mut file = node.clone();
            if multiple_drives {
                file["_driveName"] = Value::String(drive_name.clone());
            }
            all_files.push(file);
        }
    }

    // Handle visual formats
    if format.is_visual() {
        let revisions = std::collections::HashMap::new();
        let mut tree = output::build_drive_tree(&drive_with_nodes, &revisions);
        tree.url = Some(client.url.clone());
        tree.profile = Some(name.clone());
        let resolved_out = output::resolve_visual_output(out, format, "docs");
        let out_ref = resolved_out.as_deref();

        return match format {
            OutputFormat::Svg => {
                let svg = output::svg::render_svg(&tree);
                output::write_output(svg.as_bytes(), out_ref, false)
            }
            OutputFormat::Png => {
                let svg = output::svg::render_svg(&tree);
                let png = output::png::render_png(&svg)?;
                output::write_output(&png, out_ref, true)
            }
            OutputFormat::Mermaid => {
                let mmd = output::render_mermaid(&tree);
                output::write_output(mmd.as_bytes(), out_ref, false)
            }
            _ => unreachable!(),
        };
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::to_value(&all_files)?);
        }
        _ => {
            if all_files.is_empty() {
                if let Some(d) = drive {
                    println!("No documents found in drive '{d}'.");
                } else {
                    println!("No documents found.");
                }
                return Ok(());
            }

            if multiple_drives {
                let rows: Vec<Vec<String>> = all_files
                    .iter()
                    .map(|f| {
                        vec![
                            f["id"].as_str().unwrap_or("-").to_string(),
                            f["name"].as_str().unwrap_or("-").to_string(),
                            f["documentType"].as_str().unwrap_or("-").to_string(),
                            f["_driveName"].as_str().unwrap_or("-").to_string(),
                        ]
                    })
                    .collect();
                print_table(&["ID", "Name", "Type", "Drive"], &rows);
            } else {
                let rows: Vec<Vec<String>> = all_files
                    .iter()
                    .map(|f| {
                        vec![
                            f["id"].as_str().unwrap_or("-").to_string(),
                            f["name"].as_str().unwrap_or("-").to_string(),
                            f["documentType"].as_str().unwrap_or("-").to_string(),
                        ]
                    })
                    .collect();
                print_table(&["ID", "Name", "Type"], &rows);
            }
        }
    }

    Ok(())
}

async fn get(
    id: &str,
    drive: Option<&str>,
    include_state: bool,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve doc (accepts name or UUID) and drive.
    // When --drive is given, use "drive/doc" format so name resolution is scoped.
    let (doc_id, drive_id) = match drive {
        Some(d) => helpers::resolve_doc(&client, &format!("{d}/{id}")).await?,
        None => helpers::resolve_doc(&client, id).await?,
    };
    let id = &doc_id;

    // Build field list — only include stateJSON when --state is passed
    let fields = if include_state {
        "id name documentType revision stateJSON"
    } else {
        "id name documentType revision"
    };

    // Try each model namespace to find the document
    let mut doc: Option<Value> = None;

    for model in cache.models.values() {
        if !model.query_fields.iter().any(|f| f == "getDocument") {
            continue;
        }

        let query = format!(
            r#"{{ {prefix} {{ getDocument(docId: "{id}", driveId: "{drive_id}") {{ {fields} }} }} }}"#,
            prefix = model.prefix,
            id = id.replace('"', r#"\""#),
        );

        match client.query(&query, None).await {
            Ok(data) => {
                if let Some(d) = data
                    .get(&model.prefix)
                    .and_then(|v| v.get("getDocument"))
                    .filter(|d| !d.is_null())
                {
                    doc = Some(d.clone());
                    break;
                }
            }
            Err(_) => continue,
        }
    }

    let doc =
        doc.ok_or_else(|| anyhow::anyhow!("Document '{id}' not found in any model namespace"))?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&doc),
        _ => {
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

async fn tree(
    drive: Option<String>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let drive = match drive {
        Some(d) => d,
        None => {
            let (id, _slug, _name) = helpers::select_drive(&client).await?;
            id
        }
    };

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
        _ => {
            let drive_name = data
                .pointer("/driveDocument/name")
                .and_then(|v| v.as_str())
                .unwrap_or(&drive);

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
        None => Input::new().with_prompt("Document name").interact_text()?,
    };

    // Get drive — show a picker if not provided
    let drive_uuid = match drive {
        Some(d) => {
            let uuid = resolve_drive_id(&client, &d).await?;
            if d != uuid {
                println!("Resolved slug → UUID {}", &uuid[..12]);
            }
            uuid
        }
        None => {
            let (id, _slug, _name) = helpers::select_drive(&client).await?;
            id
        }
    };

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
        _ => {
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

async fn delete(ids: &[String], skip_confirm: bool, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    if !skip_confirm {
        let label = if ids.len() == 1 {
            format!("Delete document {}?", ids[0])
        } else {
            format!("Delete {} documents?", ids.len())
        };
        let confirm = dialoguer::Confirm::new()
            .with_prompt(label)
            .default(false)
            .interact()?;
        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mut failed = 0u32;
    for id_or_name in ids {
        // Resolve name to UUID if needed
        let doc_id = match helpers::resolve_doc(&client, id_or_name).await {
            Ok((id, _drive)) => id,
            Err(e) => {
                eprintln!("{} Could not resolve '{id_or_name}': {e}", "✗".red());
                failed += 1;
                continue;
            }
        };
        let mutation = format!(
            r#"mutation {{ deleteDocument(id: "{doc_id}") }}"#,
            doc_id = doc_id.replace('"', r#"\""#)
        );
        match client.query(&mutation, None).await {
            Ok(_) => println!("{} Deleted document {id_or_name}", "✓".green()),
            Err(e) => {
                eprintln!("{} Failed to delete {id_or_name}: {e}", "✗".red());
                failed += 1;
            }
        }
    }

    if failed > 0 {
        bail!("{failed} of {} deletes failed", ids.len());
    }
    Ok(())
}
