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
    /// Show hierarchical file tree of drives (all drives if no argument given)
    Tree {
        /// Drive ID or slug (omit to show all drives)
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
        /// Parent folder ID (place document inside a folder)
        #[arg(long)]
        parent_folder: Option<String>,
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
    /// Interactive field-by-field editor (use --op to skip operation picker, --input for scripting)
    Mutate(mutate::MutateArgs),
    /// Apply raw actions to a document (async, returns job ID)
    Apply {
        /// Document ID or slug
        id: String,
        /// JSON array of actions (or use --file)
        #[arg(long)]
        actions: Option<String>,
        /// Read actions JSON from a file (- for stdin)
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        /// Wait for the job to complete
        #[arg(long)]
        wait: bool,
        /// Permit string fields that contain a literal backslash-escape (`\n` as two
        /// characters). Off by default: that is almost always a shell-quoting bug that
        /// would store `\n` in a note body instead of line breaks.
        #[arg(long)]
        allow_literal_escapes: bool,
    },
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
            parent_folder,
        } => create(r#type, name, drive, parent_folder, format, profile_name).await,
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
        DocsCommand::Apply {
            id,
            actions,
            file,
            wait,
            allow_literal_escapes,
        } => {
            apply(
                &id,
                actions,
                file,
                wait,
                allow_literal_escapes,
                format,
                profile_name,
            )
            .await
        }
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
                    r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name state } } }"#,
                    None,
                )
                .await?;
            data.pointer("/findDocuments/items")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|d| {
                            d.pointer("/state/document/isDeleted")
                                .and_then(|v| v.as_bool())
                                != Some(true)
                        })
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

        // If drive state has no nodes, also try the relationship index as fallback
        if nodes.is_empty() {
            let escaped = drive_id.replace('"', r#"\""#);
            let children_query = format!(
                r#"{{ documentOutgoingRelationships(sourceIdentifier: "{escaped}", relationshipType: "child") {{ items {{ id slug name documentType state }} }} }}"#
            );
            if let Ok(data) = client.query(&children_query, None).await
                && let Some(items) = data
                    .pointer("/documentOutgoingRelationships/items")
                    .and_then(|v| v.as_array())
            {
                for item in items {
                    // Skip soft-deleted documents
                    if item
                        .pointer("/state/document/isDeleted")
                        .and_then(|v| v.as_bool())
                        == Some(true)
                    {
                        continue;
                    }
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
                    r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id state } } }"#,
                    None,
                )
                .await?;
            data.pointer("/findDocuments/items")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|d| {
                            d.pointer("/state/document/isDeleted")
                                .and_then(|v| v.as_bool())
                                != Some(true)
                        })
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

        // Also check the relationship index as fallback
        let escaped = drive_id.replace('"', r#"\""#);
        let children_query = format!(
            r#"{{ documentOutgoingRelationships(sourceIdentifier: "{escaped}", relationshipType: "child") {{ items {{ id slug name }} }} }}"#
        );
        if let Ok(data) = client.query(&children_query, None).await
            && let Some(items) = data
                .pointer("/documentOutgoingRelationships/items")
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

    // Always resolve by the document's own id/slug. (The old "drive/doc"
    // compound identifier is not understood by the reactor's document
    // resolver, which made every `docs get --drive` lookup fail and fall
    // back to a name search that can't match a UUID.) --drive instead
    // scopes the result: direct hits are membership-checked against the
    // drive, and the name-search fallback stays drive-scoped.
    let identifier = id.to_string();

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
            let data = result.unwrap();
            // --drive narrows the search: verify the direct hit is actually
            // a member of that drive (or the drive document itself).
            if let Some(d) = drive {
                let (drive_id, _drive_name, nodes) = fetch_drive_nodes(&client, d).await?;
                let doc_id = data
                    .pointer("/document/document/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&identifier)
                    .to_string();
                let is_member = doc_id == drive_id
                    || nodes
                        .iter()
                        .any(|n| n["id"].as_str() == Some(doc_id.as_str()));
                if !is_member {
                    anyhow::bail!(
                        "Document '{id}' exists but is not in drive '{d}' (omit --drive to fetch it)"
                    );
                }
            }
            (data, identifier.clone())
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

            // Show parent drive info. Scans drive node trees (source of
            // truth) rather than the relationship index, which only records
            // the creation-time parent — see find_containing_drives.
            let doc_id = doc["id"].as_str().unwrap_or("");
            if !doc_id.is_empty() {
                for parent in find_containing_drives(&client, doc_id).await {
                    println!(
                        "Drive:    {} ({}) [{}]",
                        parent.name, parent.slug, parent.id
                    );
                }
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

    // Collect the drives to render
    let drive_ids: Vec<String> = match drive {
        Some(d) => vec![d],
        None => {
            let data = client
                .query(
                    r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id state } } }"#,
                    None,
                )
                .await?;
            data.pointer("/findDocuments/items")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|d| {
                            d.pointer("/state/document/isDeleted")
                                .and_then(|v| v.as_bool())
                                != Some(true)
                        })
                        .filter_map(|d| d["id"].as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        }
    };

    if drive_ids.is_empty() {
        println!("No drives found.");
        return Ok(());
    }

    if matches!(format, OutputFormat::Json | OutputFormat::Raw) {
        // JSON: flat node list per drive with id, name, kind, documentType, parentFolder
        let mut results = Vec::new();
        for id in &drive_ids {
            let (drive_id, drive_name, nodes) = fetch_drive_nodes(&client, id).await?;
            let clean_nodes: Vec<Value> = nodes
                .iter()
                .map(|n| {
                    serde_json::json!({
                        "id": n["id"],
                        "name": n["name"],
                        "kind": n["kind"],
                        "documentType": n.get("documentType").unwrap_or(&Value::Null),
                        "parentFolder": n.get("parentFolder").unwrap_or(&Value::Null),
                    })
                })
                .collect();
            results.push(serde_json::json!({
                "id": drive_id,
                "name": drive_name,
                "nodes": clean_nodes,
            }));
        }
        if results.len() == 1 {
            print_json(&results[0]);
        } else {
            print_json(&Value::Array(results));
        }
        return Ok(());
    }

    // Text tree output — render each drive in sequence
    for (i, id) in drive_ids.iter().enumerate() {
        let (_, drive_name, nodes) = fetch_drive_nodes(&client, id).await?;
        let display_name = if drive_name.is_empty() {
            id
        } else {
            &drive_name
        };

        if i > 0 {
            println!();
        }

        if !nodes.is_empty() {
            println!("{display_name}/");
            print_tree(&nodes, None, "");
        } else {
            // Fallback: relationship index. Older/different servers may not
            // expose this field at all — treat any failure as "drive is empty"
            // rather than aborting the whole tree render.
            let escaped = id.replace('"', r#"\""#);
            let children_query = format!(
                r#"{{ documentOutgoingRelationships(sourceIdentifier: "{escaped}", relationshipType: "child") {{ items {{ id name documentType }} }} }}"#
            );
            let items: Vec<Value> = client
                .query(&children_query, None)
                .await
                .ok()
                .and_then(|data| {
                    data.pointer("/documentOutgoingRelationships/items")
                        .and_then(|v| v.as_array())
                        .cloned()
                })
                .unwrap_or_default();

            println!("{display_name}/");
            if items.is_empty() {
                println!("└── (empty)");
            } else {
                for (j, item) in items.iter().enumerate() {
                    let is_last = j == items.len() - 1;
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
    parent_folder: Option<String>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (pname, _profile, client, mut cache) = helpers::setup_with_cache(profile_name)?;

    // Auto-introspect if cache is empty (stale cache after reactor restart)
    if cache.models.is_empty() {
        eprintln!("No models in cache — re-introspecting...");
        cache = crate::graphql::introspection::run_introspection(&client).await?;
        crate::graphql::introspection::save_cache(&pname, &cache)?;
        if cache.models.is_empty() {
            bail!("No document models found even after re-introspection.");
        }
    }

    // Select document type
    let doc_type = match doc_type {
        Some(t) => t,
        None => {
            // Exclude document-drive — drives have their own `drives create` command
            let types: Vec<String> = cache
                .models
                .keys()
                .filter(|k| k.as_str() != "powerhouse/document-drive")
                .cloned()
                .collect();
            let selection = Select::new()
                .with_prompt("Select document type")
                .items(&types)
                .interact()?;
            types[selection].clone()
        }
    };

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

    // Use the model-specific createDocument(parentIdentifier) mutation.
    // This goes through the reactor's proper creation pipeline (createDocumentInDrive
    // equivalent) which ensures Connect sync works — the reactor creates the document,
    // adds it to the drive, and establishes the relationship edge atomically.
    let drive_id = helpers::resolve_doc(&client, &drive_identifier).await?;

    // Look up the model namespace from the introspection cache — required for the
    // namespaced createDocument mutation that atomically adds the doc to the drive.
    let namespace = cache.find_model(&doc_type).map(|m| m.namespace.clone());

    let mut vars = serde_json::json!({
        "name": name,
        "parentIdentifier": drive_id,
    });
    if let Some(ref folder_id) = parent_folder {
        vars["slug"] = serde_json::json!(folder_id); // parentFolder not directly supported
    }

    let ns = match &namespace {
        Some(ns) if !ns.is_empty() => ns.clone(),
        _ => bail!(
            "No namespace found for document type \"{doc_type}\". \
             Run `switchboard introspect` to refresh the schema cache."
        ),
    };

    let mutation = format!(
        "mutation($name: String!, $parentIdentifier: String) {{ {} {{ createDocument(name: $name, parentIdentifier: $parentIdentifier) {{ id }} }} }}",
        ns,
    );

    let create_data = client.query(&mutation, Some(&vars)).await?;

    let doc_id = create_data
        .get(ns.as_str())
        .and_then(|ns_val| ns_val.get("createDocument"))
        .and_then(|v| v.get("id").and_then(|id| id.as_str()))
        .ok_or_else(|| anyhow::anyhow!("createDocument returned no ID"))?
        .to_string();

    // Move into folder if --parent-folder was specified.
    if let Some(ref folder_id) = parent_folder {
        let move_mutation = "mutation($docId: PHID!, $input: DocumentDrive_MoveNodeInput!) { DocumentDrive { moveNode(docId: $docId, input: $input) { id } } }";
        let move_vars = serde_json::json!({
            "docId": drive_id,
            "input": {
                "srcFolder": doc_id,
                "targetParentFolder": folder_id,
            }
        });
        let _ = client.query(move_mutation, Some(&move_vars)).await;
    }

    let data = serde_json::json!({ "id": doc_id });

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&data),
        _ => {
            println!("{} Document created", "✓".green());
            println!("  ID: {doc_id}");
            println!("  Type: {doc_type}");
            println!("  Name: {name}");
            if let Some(folder) = &parent_folder {
                println!("  Folder: {folder}");
            }
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

    // Resolve every input id to a UUID and remember which drives carry a
    // `state.global.nodes` entry for it — both must be cleaned up server-side.
    //
    // The legacy singular `deleteDocument(identifier)` mutation still validates
    // against `drive.state.global.nodes` and rejects with "Node not found" when
    // the entry is missing, so we use the batched `deleteDocuments(propagate:
    // ORPHAN)` API. ORPHAN tears down the document and its relationship index
    // entries but does NOT touch `state.global.nodes` — verified empirically —
    // hence the explicit DELETE_NODE pre-step below. (CASCADE would also
    // delete child docs, which is wrong for a single-doc operation.)
    let remove_node_mutation = "mutation($id: String!, $actions: [JSONObject!]!) { mutateDocument(documentIdentifier: $id, actions: $actions) { id name } }";
    let mut failed = false;
    let mut to_delete: Vec<(String, String)> = Vec::new(); // (input_id, uuid)
    let mut node_cleanups: Vec<(String, String)> = Vec::new(); // (drive_id, uuid)

    for id in ids {
        let uuid = match helpers::resolve_doc(&client, id).await {
            Ok(u) => u,
            Err(_) => match resolve_doc_by_name(&client, id, None).await {
                Ok(u) => u,
                Err(_) => {
                    eprintln!("{} Document '{id}' not found", "✗".red());
                    failed = true;
                    continue;
                }
            },
        };
        for drive_id in find_parent_drives(&client, &uuid).await {
            node_cleanups.push((drive_id, uuid.clone()));
        }
        to_delete.push((id.clone(), uuid));
    }

    // Strip the drive's tree-node entries first so docs vanish from `docs list`
    // and Connect immediately. These calls are best-effort: a missing node is
    // already the desired post-condition.
    for (drive_id, uuid) in &node_cleanups {
        let ts = iso_now();
        let node_vars = serde_json::json!({
            "id": drive_id,
            "actions": [{
                "id": gen_action_id(),
                "type": "DELETE_NODE",
                "input": { "id": uuid },
                "scope": "global",
                "timestampUtcMs": ts
            }]
        });
        let _ = client.query(remove_node_mutation, Some(&node_vars)).await;
    }

    // Batch-delete the docs themselves.
    if !to_delete.is_empty() {
        let uuids: Vec<String> = to_delete.iter().map(|(_, u)| u.clone()).collect();
        let delete_mutation =
            "mutation($ids: [String!]!) { deleteDocuments(identifiers: $ids, propagate: ORPHAN) }";
        let vars = serde_json::json!({ "ids": uuids });
        match client.query(delete_mutation, Some(&vars)).await {
            Ok(_) => {
                for (input_id, _) in &to_delete {
                    println!("{} Deleted document {input_id}", "✓".green());
                }
            }
            Err(e) => {
                eprintln!("{} Failed to delete document(s): {e}", "✗".red());
                failed = true;
            }
        }
    }

    if failed {
        bail!("One or more documents could not be deleted");
    }

    Ok(())
}

/// A drive that contains a document (has a file node for it in
/// `state.global.nodes`), plus the folder the node sits in (None = drive root).
pub struct ContainingDrive {
    pub id: String,
    pub name: String,
    pub slug: String,
    /// (folder_id, folder_name) when the file node lives inside a folder.
    pub folder: Option<(String, String)>,
}

/// Enumerate ALL drives whose node tree contains a file entry for `doc_id`.
///
/// Do NOT rely on `documentIncomingRelationships` for this: the relationship
/// index only records the creation-time parent. `DocumentDrive.addFile` (what
/// `docs add-to` dispatches) adds a `state.global.nodes` entry without
/// updating the index, so a doc living in two drives is reported with a
/// single parent there. Drive state is the source of truth — `docs list` and
/// Connect both read it — so scan every drive's node tree.
pub async fn find_containing_drives(
    client: &crate::graphql::GraphQLClient,
    doc_id: &str,
) -> Vec<ContainingDrive> {
    let data = match client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug state } } }"#,
            None,
        )
        .await
    {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let drives = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut result = Vec::new();
    for drive in &drives {
        // Skip soft-deleted drives
        if drive
            .pointer("/state/document/isDeleted")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            continue;
        }
        let nodes = match drive
            .pointer("/state/global/nodes")
            .and_then(|v| v.as_array())
        {
            Some(n) => n,
            None => continue,
        };
        let node = nodes
            .iter()
            .find(|n| n["kind"].as_str() == Some("file") && n["id"].as_str() == Some(doc_id));
        let node = match node {
            Some(n) => n,
            None => continue,
        };
        let folder = node["parentFolder"]
            .as_str()
            .filter(|f| !f.is_empty())
            .map(|folder_id| {
                let folder_name = nodes
                    .iter()
                    .find(|n| {
                        n["kind"].as_str() == Some("folder") && n["id"].as_str() == Some(folder_id)
                    })
                    .and_then(|n| n["name"].as_str())
                    .unwrap_or("?")
                    .to_string();
                (folder_id.to_string(), folder_name)
            });
        result.push(ContainingDrive {
            id: drive["id"].as_str().unwrap_or("").to_string(),
            name: drive["name"].as_str().unwrap_or("").to_string(),
            slug: drive["slug"].as_str().unwrap_or("").to_string(),
            folder,
        });
    }
    result
}

/// Find parent drives of a document (returns their UUIDs).
///
/// Scans every drive's `state.global.nodes` (source of truth), so drives are
/// found regardless of whether the doc was created in the drive or added
/// later via ADD_FILE. Callers (delete's DELETE_NODE pre-step, rename's
/// updateFile sync) only care about drives that actually hold a file node —
/// relationship-only parents have no node to clean up or update.
async fn find_parent_drives(client: &crate::graphql::GraphQLClient, doc_id: &str) -> Vec<String> {
    find_containing_drives(client, doc_id)
        .await
        .into_iter()
        .map(|d| d.id)
        .collect()
}

/// Scan a drive's node tree and return file nodes whose documents don't exist.
pub async fn find_ghost_nodes(
    client: &crate::graphql::GraphQLClient,
    drive_id: &str,
) -> Result<Vec<(String, String, String)>> {
    let escaped = drive_id.replace('"', r#"\""#);
    let query =
        format!(r#"{{ document(identifier: "{escaped}") {{ document {{ id name state }} }} }}"#);
    let data = client.query(&query, None).await?;
    let doc = data
        .pointer("/document/document")
        .ok_or_else(|| anyhow::anyhow!("Drive '{drive_id}' not found"))?;

    let nodes = doc
        .pointer("/state/global/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut ghosts = Vec::new();
    for node in &nodes {
        if node["kind"].as_str() != Some("file") {
            continue;
        }
        let node_id = node["id"].as_str().unwrap_or("");
        if node_id.is_empty() {
            continue;
        }
        let node_name = node["name"].as_str().unwrap_or("?").to_string();
        let node_type = node["documentType"].as_str().unwrap_or("?").to_string();

        // Try to fetch the document — if it fails, it's a ghost
        let check_escaped = node_id.replace('"', r#"\""#);
        let check_query =
            format!(r#"{{ document(identifier: "{check_escaped}") {{ document {{ id }} }} }}"#);
        if client.query(&check_query, None).await.is_err() {
            ghosts.push((node_id.to_string(), node_name, node_type));
        }
    }

    Ok(ghosts)
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
    let doc_uuid = doc["id"].as_str().unwrap_or(id).to_string();

    // Sync each parent drive's tree-node entry. `renameDocument` updates the
    // doc's own name + records SET_NAME, but `state.global.nodes[].name` is a
    // separate cached field; without this follow-up `docs list` and Connect
    // continue to show the stale pre-rename name.
    let parent_drives = find_parent_drives(&client, &doc_uuid).await;
    let update_file_mutation = "mutation($docId: PHID!, $input: DocumentDrive_UpdateFileInput!) { DocumentDrive { updateFile(docId: $docId, input: $input) { id } } }";
    for drive_id in &parent_drives {
        let vars = serde_json::json!({
            "docId": drive_id,
            "input": { "id": doc_uuid, "name": name }
        });
        let _ = client.query(update_file_mutation, Some(&vars)).await;
    }

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

    // Resolve slugs/names to the UUID used in drive node trees.
    let uuid = helpers::resolve_doc(&client, id)
        .await
        .unwrap_or_else(|_| id.to_string());

    // Source of truth: every drive whose node tree holds a file entry for the
    // doc. The relationship index alone under-reports — it only records the
    // creation-time parent, not drives the doc was added to via ADD_FILE.
    let containing = find_containing_drives(&client, &uuid).await;
    let mut items: Vec<Value> = containing
        .iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id,
                "name": d.name,
                "slug": d.slug,
                "documentType": "powerhouse/document-drive",
                "folder": d.folder.as_ref().map(|(fid, fname)| {
                    serde_json::json!({ "id": fid, "name": fname })
                }),
            })
        })
        .collect();

    // Merge relationship-index parents on top (covers non-drive parent
    // documents and drives whose node entry was removed but whose
    // relationship survives).
    let escaped = uuid.replace('"', r#"\""#);
    let query = format!(
        r#"{{ documentIncomingRelationships(targetIdentifier: "{escaped}", relationshipType: "child") {{ items {{ id name slug documentType }} totalCount }} }}"#
    );
    if let Ok(data) = client.query(&query, None).await
        && let Some(rel_items) = data
            .pointer("/documentIncomingRelationships/items")
            .and_then(|v| v.as_array())
    {
        for p in rel_items {
            let pid = p["id"].as_str().unwrap_or("");
            if !items.iter().any(|existing| existing["id"] == pid) {
                let mut entry = p.clone();
                entry["folder"] = Value::Null;
                items.push(entry);
            }
        }
    }

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
                    let folder = p
                        .pointer("/folder/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("/")
                        .to_string();
                    vec![
                        p["id"].as_str().unwrap_or("-").to_string(),
                        p["name"].as_str().unwrap_or("-").to_string(),
                        p["documentType"].as_str().unwrap_or("-").to_string(),
                        folder,
                    ]
                })
                .collect();
            print_table(&["ID", "Name", "Type", "Folder"], &rows);
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

    let (drive_id, parent_folder) = helpers::resolve_drive_and_parent(&client, parent).await?;
    let mut added = Vec::new();

    for id in ids {
        // Fetch doc info (name + documentType) so we can register it in the drive
        let resolved = helpers::resolve_doc(&client, id)
            .await
            .unwrap_or_else(|_| id.clone());
        let escaped = resolved.replace('"', r#"\""#);
        let info_query = format!(
            r#"{{ document(identifier: "{escaped}") {{ document {{ id name documentType }} }} }}"#
        );
        let info = client.query(&info_query, None).await?;
        let doc = info
            .pointer("/document/document")
            .ok_or_else(|| anyhow::anyhow!("Document '{id}' not found"))?;
        let doc_name = doc["name"].as_str().unwrap_or("unknown");
        let doc_type = doc["documentType"].as_str().unwrap_or("unknown");
        let doc_id = doc["id"].as_str().unwrap_or(&resolved);

        let mut input = serde_json::json!({
            "id": doc_id,
            "name": doc_name,
            "documentType": doc_type,
        });
        if let Some(folder) = &parent_folder {
            input["parentFolder"] = serde_json::Value::String(folder.clone());
        }
        let vars = serde_json::json!({
            "docId": drive_id,
            "input": input,
        });
        let add_file_mutation = "mutation($docId: PHID!, $input: DocumentDrive_AddFileInput!) { DocumentDrive { addFile(docId: $docId, input: $input) { id name } } }";
        client.query(add_file_mutation, Some(&vars)).await?;
        added.push(doc_name.to_string());
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::json!({ "added": added, "parent": drive_id }));
        }
        _ => {
            for name in &added {
                println!("{} Added {} to {}", "✓".green(), name, parent);
            }
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

    // `parent` may be a drive or a folder. `deleteNode` only needs the drive
    // ID + file ID (the file's location in the tree is irrelevant), so we
    // discard the folder context after resolving.
    let (drive_id, _folder) = helpers::resolve_drive_and_parent(&client, parent).await?;
    let mut removed = Vec::new();

    for id in ids {
        let resolved = helpers::resolve_doc(&client, id)
            .await
            .unwrap_or_else(|_| id.clone());
        let vars = serde_json::json!({
            "docId": drive_id,
            "input": { "id": resolved }
        });
        let del_node_mutation = "mutation($docId: PHID!, $input: DocumentDrive_DeleteNodeInput!) { DocumentDrive { deleteNode(docId: $docId, input: $input) { id } } }";
        client.query(del_node_mutation, Some(&vars)).await?;
        removed.push(resolved);
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::json!({ "removed": removed, "parent": drive_id }));
        }
        _ => {
            println!(
                "{} Removed {} document(s) from {}",
                "✓".green(),
                removed.len(),
                parent
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

    let (from_drive, _from_folder) = helpers::resolve_drive_and_parent(&client, from).await?;
    let (to_drive, to_folder) = helpers::resolve_drive_and_parent(&client, to).await?;
    let mut moved = Vec::new();

    for id in ids {
        let resolved = helpers::resolve_doc(&client, id)
            .await
            .unwrap_or_else(|_| id.clone());

        // Fetch doc info — needed for the cross-drive add-back path; cheap so
        // we always run it and let the intra-drive branch ignore name/type.
        let escaped = resolved.replace('"', r#"\""#);
        let info_query = format!(
            r#"{{ document(identifier: "{escaped}") {{ document {{ id name documentType }} }} }}"#
        );
        let info = client.query(&info_query, None).await?;
        let doc = info
            .pointer("/document/document")
            .ok_or_else(|| anyhow::anyhow!("Document '{id}' not found"))?;
        let doc_name = doc["name"].as_str().unwrap_or("unknown");
        let doc_type = doc["documentType"].as_str().unwrap_or("unknown");
        let doc_id = doc["id"].as_str().unwrap_or(&resolved);

        if from_drive == to_drive {
            // Intra-drive move: use the dedicated MoveNode mutation. Updates
            // the existing tree entry in place (one round-trip instead of two,
            // and the entry retains its identity in the operations log).
            //
            // Note: the input field is named `srcFolder` in the schema but
            // accepts ANY node id (file or folder), confirmed empirically.
            let mut move_input = serde_json::json!({ "srcFolder": doc_id });
            if let Some(folder) = &to_folder {
                move_input["targetParentFolder"] = serde_json::Value::String(folder.clone());
            }
            let move_vars = serde_json::json!({
                "docId": from_drive,
                "input": move_input,
            });
            let move_mutation = "mutation($docId: PHID!, $input: DocumentDrive_MoveNodeInput!) { DocumentDrive { moveNode(docId: $docId, input: $input) { id } } }";
            client.query(move_mutation, Some(&move_vars)).await?;
        } else {
            // Cross-drive move: MoveNode is scoped to a single drive, so fall
            // back to deleteNode + addFile.
            let del_vars = serde_json::json!({
                "docId": from_drive,
                "input": { "id": doc_id }
            });
            let del_node_mutation = "mutation($docId: PHID!, $input: DocumentDrive_DeleteNodeInput!) { DocumentDrive { deleteNode(docId: $docId, input: $input) { id } } }";
            client.query(del_node_mutation, Some(&del_vars)).await?;

            let mut add_input = serde_json::json!({
                "id": doc_id,
                "name": doc_name,
                "documentType": doc_type,
            });
            if let Some(folder) = &to_folder {
                add_input["parentFolder"] = serde_json::Value::String(folder.clone());
            }
            let add_vars = serde_json::json!({
                "docId": to_drive,
                "input": add_input,
            });
            let add_file_mutation = "mutation($docId: PHID!, $input: DocumentDrive_AddFileInput!) { DocumentDrive { addFile(docId: $docId, input: $input) { id } } }";
            client.query(add_file_mutation, Some(&add_vars)).await?;
        }

        moved.push(doc_name.to_string());
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::json!({ "moved": moved, "from": from_drive, "to": to_drive }));
        }
        _ => {
            for name in &moved {
                println!("{} Moved {} from {} to {}", "✓".green(), name, from, to);
            }
        }
    }

    Ok(())
}

/// Apply raw actions to a document via mutateDocumentAsync.
/// Returns a job ID. With --wait, blocks until the job completes.
async fn apply(
    id: &str,
    actions_arg: Option<String>,
    file_arg: Option<String>,
    wait: bool,
    allow_literal_escapes: bool,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    // Read actions JSON from --actions, --file, or stdin
    let actions_json = match (actions_arg, file_arg) {
        (Some(json), _) => json,
        (_, Some(path)) if path == "-" => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow::anyhow!("Failed to read stdin: {e}"))?;
            buf
        }
        (_, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read file '{path}': {e}"))?,
        (None, None) => bail!("Provide actions via --actions or --file (use - for stdin)"),
    };

    let actions: Value =
        serde_json::from_str(&actions_json).map_err(|e| anyhow::anyhow!("Invalid JSON: {e}"))?;

    if !actions.is_array() {
        bail!("Actions must be a JSON array");
    }

    // Refuse payloads whose strings carry a literal `\n` — a shell-quoting bug
    // the reactor would happily persist as broken note content.
    helpers::reject_literal_escapes(&actions, allow_literal_escapes)?;

    // Auto-populate timestampUtcMs on each action if missing.
    // The reactor's operation store requires this field but the generic
    // mutateDocument resolver doesn't inject it (unlike model-specific resolvers).
    let actions = stamp_actions(actions);

    let resolved_id = helpers::resolve_doc(&client, id)
        .await
        .unwrap_or_else(|_| id.to_string());

    let vars = serde_json::json!({
        "documentIdentifier": resolved_id,
        "actions": actions,
    });

    // Use async variant so we get a job ID
    let data = client
        .query(
            "mutation($documentIdentifier: String!, $actions: [JSONObject!]!) { \
             mutateDocumentAsync(documentIdentifier: $documentIdentifier, actions: $actions) }",
            Some(&vars),
        )
        .await?;

    let job_id = data["mutateDocumentAsync"]
        .as_str()
        .unwrap_or("")
        .to_string();

    if job_id.is_empty() {
        bail!("No job ID returned from mutateDocumentAsync");
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            print_json(&serde_json::json!({ "jobId": job_id }));
        }
        _ => {
            println!("Job: {job_id}");
        }
    }

    if wait {
        eprintln!("Waiting for job to complete...");
        crate::cli::jobs::run(
            crate::cli::jobs::JobsCommand::Wait {
                job_id,
                timeout: 300,
            },
            format,
            profile_name,
            false,
        )
        .await?;
    }

    Ok(())
}

/// Generate an ISO-8601 timestamp string (e.g. "2026-03-22T22:06:53.528Z").
pub fn iso_now() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days_since_epoch);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

/// Generate a random action ID (hex string matching the reactor's format).
pub fn gen_action_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;
    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let h1 = hasher.finish();
    // Second hash with different seed for more entropy
    h1.hash(&mut hasher);
    let h2 = hasher.finish();
    format!("{h1:016x}{h2:016x}")
}

/// Inject `timestampUtcMs` and `id` into each action object that doesn't already have them.
/// The `id` field is required by Connect's sync — actions without it cause
/// "Cannot return null for non-nullable field Action.id" errors.
fn stamp_actions(actions: Value) -> Value {
    let Value::Array(mut arr) = actions else {
        return actions;
    };

    let now_iso = iso_now();

    for action in &mut arr {
        if let Value::Object(map) = action {
            if !map.contains_key("timestampUtcMs") {
                map.insert("timestampUtcMs".to_string(), Value::String(now_iso.clone()));
            }
            if !map.contains_key("id") {
                map.insert("id".to_string(), Value::String(gen_action_id()));
            }
        }
    }

    Value::Array(arr)
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
