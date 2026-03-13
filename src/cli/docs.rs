use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Input, Select};
use serde_json::Value;

use crate::cli::helpers;
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
        /// Output file path (for svg/png/mermaid formats)
        #[arg(long, short)]
        out: Option<String>,
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
    /// Rename a document
    Rename {
        /// Document ID or slug
        id: String,
        /// New name
        name: String,
    },
    /// Show parent documents (reverse tree traversal)
    Parents {
        /// Document ID or slug
        id: String,
    },
    /// Add documents as children of a parent
    #[command(name = "add-to")]
    AddTo {
        /// Parent document/drive ID or slug
        parent: String,
        /// Document IDs to add
        ids: Vec<String>,
    },
    /// Remove documents from a parent
    #[command(name = "remove-from")]
    RemoveFrom {
        /// Parent document/drive ID or slug
        parent: String,
        /// Document IDs to remove
        ids: Vec<String>,
    },
    /// Move documents between parents
    Move {
        /// Document IDs to move
        ids: Vec<String>,
        /// Source parent ID or slug
        #[arg(long)]
        from: String,
        /// Destination parent ID or slug
        #[arg(long)]
        to: String,
    },
    /// Mutate a document (apply operations)
    Mutate(mutate::MutateArgs),
}

pub async fn run(cmd: DocsCommand, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    match cmd {
        DocsCommand::List { drive, r#type, out } => {
            list(
                drive.as_deref(),
                r#type.as_deref(),
                format,
                out.as_deref(),
                profile_name,
            )
            .await
        }
        DocsCommand::Get {
            id,
            drive,
            state,
            out,
        } => {
            get(
                &id,
                drive.as_deref(),
                state,
                format,
                out.as_deref(),
                profile_name,
            )
            .await
        }
        DocsCommand::Tree { drive } => tree(drive, format, profile_name).await,
        DocsCommand::Create {
            r#type,
            name,
            drive,
        } => create(r#type, name, drive, format, profile_name).await,
        DocsCommand::Delete { ids, yes } => delete(&ids, yes, profile_name).await,
        DocsCommand::Rename { id, name } => rename(&id, &name, format, profile_name).await,
        DocsCommand::Parents { id } => parents(&id, format, profile_name).await,
        DocsCommand::AddTo { parent, ids } => add_to(&parent, &ids, format, profile_name).await,
        DocsCommand::RemoveFrom { parent, ids } => {
            remove_from(&parent, &ids, format, profile_name).await
        }
        DocsCommand::Move { ids, from, to } => {
            move_docs(&ids, &from, &to, format, profile_name).await
        }
        DocsCommand::Mutate(args) => mutate::run(args, format, profile_name).await,
    }
}

/// Fetch drive document and return (drive_id, drive_name, nodes from state.global.nodes)
async fn fetch_drive_nodes(
    client: &crate::graphql::GraphQLClient,
    drive_identifier: &str,
) -> Result<(String, String, Vec<Value>)> {
    let escaped = drive_identifier.replace('"', r#"\""#);
    let query =
        format!(r#"{{ document(identifier: "{escaped}") {{ document {{ id name state }} }} }}"#);
    let data = client.query(&query, None).await?;
    let doc = data
        .pointer("/document/document")
        .ok_or_else(|| anyhow::anyhow!("Drive '{drive_identifier}' not found"))?;

    let id = doc["id"].as_str().unwrap_or(drive_identifier).to_string();
    let name = doc["name"].as_str().unwrap_or("").to_string();
    let nodes = doc
        .pointer("/state/global/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok((id, name, nodes))
}

async fn list(
    drive: Option<&str>,
    doc_type: Option<&str>,
    format: OutputFormat,
    out: Option<&str>,
    profile_name: Option<&str>,
) -> Result<()> {
    let (profile_display, _profile, client) = helpers::setup(profile_name)?;

    // Collect drives to query — same logic as old CLI
    let drive_ids: Vec<(String, String)> = match drive {
        Some(d) => {
            let escaped = d.replace('"', r#"\""#);
            let query =
                format!(r#"{{ document(identifier: "{escaped}") {{ document {{ id name }} }} }}"#);
            let data = client.query(&query, None).await?;
            let id = data
                .pointer("/document/document/id")
                .and_then(|v| v.as_str())
                .unwrap_or(d)
                .to_string();
            let name = data
                .pointer("/document/document/name")
                .and_then(|v| v.as_str())
                .unwrap_or(d)
                .to_string();
            vec![(id, name)]
        }
        None => {
            // All drives
            let data = client
                .query(
                    r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name } } }"#,
                    None,
                )
                .await?;
            data.pointer("/findDocuments/items")
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
        let (_, _, nodes) = fetch_drive_nodes(&client, drive_id).await?;

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

        for node in &nodes {
            if node["kind"].as_str() != Some("file") {
                continue;
            }
            if let Some(dt) = doc_type
                && node["documentType"].as_str() != Some(dt)
            {
                continue;
            }
            let mut file = node.clone();
            if multiple_drives {
                file["_driveName"] = Value::String(drive_name.clone());
            }
            all_files.push(file);
        }

        // If drive state has no nodes, also try documentChildren as fallback
        if nodes.is_empty() {
            let escaped = drive_id.replace('"', r#"\""#);
            let children_query = format!(
                r#"{{ documentChildren(parentIdentifier: "{escaped}") {{ items {{ id slug name documentType }} }} }}"#
            );
            if let Ok(data) = client.query(&children_query, None).await
                && let Some(items) = data
                    .pointer("/documentChildren/items")
                    .and_then(|v| v.as_array())
            {
                for item in items {
                    if let Some(dt) = doc_type
                        && item["documentType"].as_str() != Some(dt)
                    {
                        continue;
                    }
                    let mut file = item.clone();
                    // Add kind so the visual/table logic works
                    file["kind"] = Value::String("file".to_string());
                    if multiple_drives {
                        file["_driveName"] = Value::String(drive_name.clone());
                    }
                    all_files.push(file);
                }
            }
        }
    }

    // Handle visual formats
    if format.is_visual() {
        let revisions = std::collections::HashMap::new();
        let mut tree = output::build_drive_tree(&drive_with_nodes, &revisions);
        tree.url = Some(client.url.clone());
        tree.profile = Some(profile_display.clone());
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

/// Resolve a document name to its ID by searching drive nodes.
/// If `drive` is given, only searches that drive; otherwise searches all drives.
async fn resolve_doc_by_name(
    client: &crate::graphql::GraphQLClient,
    name: &str,
    drive: Option<&str>,
) -> Result<String> {
    let drive_ids: Vec<String> = match drive {
        Some(d) => vec![d.to_string()],
        None => {
            let data = client
                .query(
                    r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id } } }"#,
                    None,
                )
                .await?;
            data.pointer("/findDocuments/items")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| d["id"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        }
    };

    let name_lower = name.to_lowercase();
    let mut matches: Vec<(String, String)> = Vec::new(); // (id, name)

    for drive_id in &drive_ids {
        // Check drive state nodes
        if let Ok((_, _, nodes)) = fetch_drive_nodes(client, drive_id).await {
            for node in &nodes {
                if node["kind"].as_str() != Some("file") {
                    continue;
                }
                let node_name = node["name"].as_str().unwrap_or("");
                if node_name.eq_ignore_ascii_case(&name_lower)
                    && let Some(id) = node["id"].as_str()
                {
                    return Ok(id.to_string());
                }
                // Also partial/contains match as secondary
                if node_name.to_lowercase().contains(&name_lower)
                    && let Some(id) = node["id"].as_str()
                {
                    matches.push((id.to_string(), node_name.to_string()));
                }
            }
        }

        // Also check documentChildren as fallback
        let escaped = drive_id.replace('"', r#"\""#);
        let children_query = format!(
            r#"{{ documentChildren(parentIdentifier: "{escaped}") {{ items {{ id slug name }} }} }}"#
        );
        if let Ok(data) = client.query(&children_query, None).await
            && let Some(items) = data
                .pointer("/documentChildren/items")
                .and_then(|v| v.as_array())
        {
            for item in items {
                let item_name = item["name"].as_str().unwrap_or("");
                if item_name.eq_ignore_ascii_case(&name_lower)
                    && let Some(id) = item["id"].as_str()
                {
                    return Ok(id.to_string());
                }
                if item_name.to_lowercase().contains(&name_lower)
                    && let Some(id) = item["id"].as_str()
                {
                    matches.push((id.to_string(), item_name.to_string()));
                }
            }
        }
    }

    // If we have exactly one partial match, use it
    if matches.len() == 1 {
        return Ok(matches[0].0.clone());
    }

    if matches.len() > 1 {
        let list = matches
            .iter()
            .map(|(id, n)| format!("  - {n} ({id})"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!("Multiple documents match '{name}':\n{list}\nUse the document ID instead.");
    }

    bail!("Document '{name}' not found");
}

async fn get(
    id: &str,
    drive: Option<&str>,
    include_state: bool,
    format: OutputFormat,
    out: Option<&str>,
    profile_name: Option<&str>,
) -> Result<()> {
    let (name, _profile, client) = helpers::setup(profile_name)?;

    // Build the identifier: if --drive is given, use "drive/doc" format
    let identifier = match drive {
        Some(d) => format!("{d}/{id}"),
        None => id.to_string(),
    };

    // Visual formats always need state
    let need_state = include_state || format.is_visual();

    let state_field = if need_state { "state" } else { "" };

    // Try direct lookup by ID/slug first
    let (data, resolved_id) = {
        let escaped = identifier.replace('"', r#"\""#);
        let query = format!(
            r#"{{ document(identifier: "{escaped}") {{ document {{ id slug name documentType {state_field} revisionsList {{ scope revision }} createdAtUtcIso lastModifiedAtUtcIso }} childIds }} }}"#
        );
        let result = client.query(&query, None).await;
        let found = result
            .as_ref()
            .ok()
            .and_then(|d| d.pointer("/document/document"))
            .is_some_and(|d| !d.is_null());

        if found {
            (result.unwrap(), identifier.clone())
        } else {
            // Fallback: search by name across drives (or within --drive)
            let resolved = resolve_doc_by_name(&client, id, drive).await?;
            let escaped = resolved.replace('"', r#"\""#);
            let query = format!(
                r#"{{ document(identifier: "{escaped}") {{ document {{ id slug name documentType {state_field} revisionsList {{ scope revision }} createdAtUtcIso lastModifiedAtUtcIso }} childIds }} }}"#
            );
            (client.query(&query, None).await?, resolved)
        }
    };

    let doc = data
        .pointer("/document/document")
        .filter(|d| !d.is_null())
        .ok_or_else(|| anyhow::anyhow!("Document '{id}' not found"))?;
    let child_ids = data
        .pointer("/document/childIds")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let _ = resolved_id; // used above for the query

    // Visual formats: render document state as themed SVG/PNG
    if format.is_visual() {
        let state = doc.get("state").filter(|v| !v.is_null()).cloned();

        let doc_name = doc["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                state
                    .as_ref()
                    .and_then(|s| s.get("name"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("-");

        let doc_id = doc["id"].as_str().unwrap_or("-");

        let file_name = if id != doc_id {
            Some(id.to_string())
        } else {
            None
        };

        let revision = doc
            .get("revisionsList")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .map(|r| r["revision"].as_u64().unwrap_or(0))
                    .max()
            })
            .unwrap_or(0);

        let drive_id = drive.unwrap_or("-").to_string();

        let view = output::DocStateView {
            url: Some(client.url.clone()),
            profile: Some(name.clone()),
            drive: Some(drive_id),
            id: doc_id.into(),
            name: doc_name.into(),
            file_name,
            document_type: doc["documentType"].as_str().unwrap_or("-").into(),
            revision,
            state,
        };

        let resolved_out = output::resolve_visual_output(out, format, "doc");
        let out_ref = resolved_out.as_deref();

        return match format {
            OutputFormat::Svg => {
                let svg = output::svg::render_doc_state_svg(&view);
                output::write_output(svg.as_bytes(), out_ref, false)
            }
            OutputFormat::Png => {
                let svg = output::svg::render_doc_state_svg(&view);
                let png = output::png::render_png(&svg)?;
                output::write_output(&png, out_ref, true)
            }
            OutputFormat::Mermaid => {
                let mmd = format!("graph TD\n    doc[\"{}\"]\n", view.name);
                output::write_output(mmd.as_bytes(), out_ref, false)
            }
            _ => unreachable!(),
        };
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let mut output = doc.clone();
            if !child_ids.is_empty() {
                output["childIds"] = Value::Array(child_ids);
            }
            print_json(&output);
        }
        _ => {
            println!("ID:       {}", doc["id"].as_str().unwrap_or("-"));
            println!("Slug:     {}", doc["slug"].as_str().unwrap_or("-"));
            println!("Name:     {}", doc["name"].as_str().unwrap_or("-"));
            println!("Type:     {}", doc["documentType"].as_str().unwrap_or("-"));

            if let Some(revisions) = doc.get("revisionsList").and_then(|v| v.as_array()) {
                for rev in revisions {
                    let scope = rev["scope"].as_str().unwrap_or("-");
                    let revision = rev["revision"].as_u64().unwrap_or(0);
                    println!("Revision: {scope} = {revision}");
                }
            }

            if let Some(created) = doc.get("createdAtUtcIso").and_then(|v| v.as_str()) {
                println!("Created:  {created}");
            }
            if let Some(modified) = doc.get("lastModifiedAtUtcIso").and_then(|v| v.as_str()) {
                println!("Modified: {modified}");
            }

            if !child_ids.is_empty() {
                println!("Children: {}", child_ids.len());
            }

            if let Some(state) = doc.get("state").filter(|v| !v.is_null()) {
                println!("\nState:");
                print_json(state);
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

    let (_, drive_name, nodes) = fetch_drive_nodes(&client, &drive).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let escaped = drive.replace('"', r#"\""#);
            let query = format!(
                r#"{{ document(identifier: "{escaped}") {{ document {{ id slug name state }} childIds }} }}"#
            );
            let data = client.query(&query, None).await?;
            let doc = data.pointer("/document").cloned().unwrap_or_default();
            print_json(&doc);
        }
        _ => {
            let display_name = if drive_name.is_empty() {
                &drive
            } else {
                &drive_name
            };

            if !nodes.is_empty() {
                // Use drive state nodes (same as old CLI — has folder/file hierarchy)
                println!("{display_name}/");
                print_tree(&nodes, None, "");
            } else {
                // Fallback: use documentChildren for flat listing
                let escaped = drive.replace('"', r#"\""#);
                let children_query = format!(
                    r#"{{ documentChildren(parentIdentifier: "{escaped}") {{ items {{ id name documentType }} }} }}"#
                );
                let data = client.query(&children_query, None).await?;
                let items = data
                    .pointer("/documentChildren/items")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                println!("{display_name}/");
                for (i, item) in items.iter().enumerate() {
                    let is_last = i == items.len() - 1;
                    let connector = if is_last { "└── " } else { "├── " };
                    let item_name = item["name"].as_str().unwrap_or("-");
                    let doc_type = item["documentType"].as_str().unwrap_or("");
                    if doc_type.is_empty() {
                        println!("{connector}{item_name}");
                    } else {
                        println!("{connector}{item_name} ({doc_type})");
                    }
                }
            }
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
    let drive_identifier = match drive {
        Some(d) => d,
        None => {
            let (id, _slug, _name) = helpers::select_drive(&client).await?;
            id
        }
    };

    let escaped_name = name.replace('"', r#"\""#);
    let escaped_drive = drive_identifier.replace('"', r#"\""#);

    // Use the model's typed create mutation
    let mutation = format!(
        r#"mutation {{ {create_mutation}(name: "{escaped_name}", parentIdentifier: "{escaped_drive}") {{ id }} }}"#,
        create_mutation = model.create_mutation,
    );

    let data = client.query(&mutation, None).await?;
    let doc_id = data.get(&model.create_mutation).and_then(|v| {
        v.as_str()
            .or_else(|| v.get("id").and_then(|id| id.as_str()))
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

    // Use batch deleteDocuments API
    let id_list: String = ids
        .iter()
        .map(|id| format!("\"{}\"", id.replace('"', r#"\""#)))
        .collect::<Vec<_>>()
        .join(", ");
    let mutation =
        format!(r#"mutation {{ deleteDocuments(identifiers: [{id_list}], propagate: CASCADE) }}"#);

    match client.query(&mutation, None).await {
        Ok(_) => {
            for id in ids {
                println!("{} Deleted document {id}", "✓".green());
            }
        }
        Err(e) => {
            bail!("Failed to delete documents: {e}");
        }
    }

    Ok(())
}

async fn rename(
    id: &str,
    name: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_pname, _profile, client) = helpers::setup(profile_name)?;

    let escaped_id = id.replace('"', r#"\""#);
    let escaped_name = name.replace('"', r#"\""#);
    let mutation = format!(
        r#"mutation {{ renameDocument(documentIdentifier: "{escaped_id}", name: "{escaped_name}") {{ id name slug }} }}"#
    );

    let data = client.query(&mutation, None).await?;
    let doc = &data["renameDocument"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(doc),
        _ => {
            println!(
                "{} Renamed to \"{}\"",
                "✓".green(),
                doc["name"].as_str().unwrap_or(name)
            );
        }
    }

    Ok(())
}

async fn parents(id: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let escaped = id.replace('"', r#"\""#);
    let query = format!(
        r#"{{ documentParents(childIdentifier: "{escaped}") {{ items {{ id name slug documentType }} totalCount }} }}"#
    );

    let data = client.query(&query, None).await?;
    let items = data
        .pointer("/documentParents/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(items)),
        _ => {
            if items.is_empty() {
                println!("No parent documents found for '{id}'.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = items
                .iter()
                .map(|p| {
                    vec![
                        p["id"].as_str().unwrap_or("-").to_string(),
                        p["name"].as_str().unwrap_or("-").to_string(),
                        p["documentType"].as_str().unwrap_or("-").to_string(),
                    ]
                })
                .collect();
            print_table(&["ID", "Name", "Type"], &rows);
        }
    }

    Ok(())
}

async fn add_to(
    parent: &str,
    ids: &[String],
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let escaped_parent = parent.replace('"', r#"\""#);
    let id_list: String = ids
        .iter()
        .map(|id| format!("\"{}\"", id.replace('"', r#"\""#)))
        .collect::<Vec<_>>()
        .join(", ");

    let mutation = format!(
        r#"mutation {{ addChildren(parentIdentifier: "{escaped_parent}", documentIdentifiers: [{id_list}]) {{ id name }} }}"#
    );

    let data = client.query(&mutation, None).await?;
    let doc = &data["addChildren"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(doc),
        _ => {
            println!(
                "{} Added {} document(s) to {}",
                "✓".green(),
                ids.len(),
                doc["name"].as_str().unwrap_or(parent)
            );
        }
    }

    Ok(())
}

async fn remove_from(
    parent: &str,
    ids: &[String],
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let escaped_parent = parent.replace('"', r#"\""#);
    let id_list: String = ids
        .iter()
        .map(|id| format!("\"{}\"", id.replace('"', r#"\""#)))
        .collect::<Vec<_>>()
        .join(", ");

    let mutation = format!(
        r#"mutation {{ removeChildren(parentIdentifier: "{escaped_parent}", documentIdentifiers: [{id_list}]) {{ id name }} }}"#
    );

    let data = client.query(&mutation, None).await?;
    let doc = &data["removeChildren"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(doc),
        _ => {
            println!(
                "{} Removed {} document(s) from {}",
                "✓".green(),
                ids.len(),
                doc["name"].as_str().unwrap_or(parent)
            );
        }
    }

    Ok(())
}

async fn move_docs(
    ids: &[String],
    from: &str,
    to: &str,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let escaped_from = from.replace('"', r#"\""#);
    let escaped_to = to.replace('"', r#"\""#);
    let id_list: String = ids
        .iter()
        .map(|id| format!("\"{}\"", id.replace('"', r#"\""#)))
        .collect::<Vec<_>>()
        .join(", ");

    let mutation = format!(
        r#"mutation {{ moveChildren(sourceParentIdentifier: "{escaped_from}", targetParentIdentifier: "{escaped_to}", documentIdentifiers: [{id_list}]) {{ source {{ id name }} target {{ id name }} }} }}"#
    );

    let data = client.query(&mutation, None).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data["moveChildren"]),
        _ => {
            let src = data
                .pointer("/moveChildren/source/name")
                .and_then(|v| v.as_str())
                .unwrap_or(from);
            let dst = data
                .pointer("/moveChildren/target/name")
                .and_then(|v| v.as_str())
                .unwrap_or(to);
            println!(
                "{} Moved {} document(s) from {} to {}",
                "✓".green(),
                ids.len(),
                src,
                dst
            );
        }
    }

    Ok(())
}
