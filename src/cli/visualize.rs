use std::collections::HashMap;

use anyhow::Result;
use colored::Colorize;
use serde_json::Value;

use crate::cli::helpers;
use crate::graphql::GraphQLClient;
use crate::output::{self, DriveTree, OutputFormat, TreeEntry, build_drive_tree, render_mermaid};

pub async fn run(
    format: OutputFormat,
    out: Option<&str>,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (name, _profile, client, _cache) = helpers::setup_with_cache(profile_name)?;

    if !quiet {
        eprintln!(
            "{} Fetching drives from profile '{name}'...",
            "→".cyan().bold()
        );
    }

    // Step 1: Fetch all drives
    let drives_data = client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug documentType state } totalCount } }"#,
            None,
        )
        .await?;

    let drives: Vec<Value> = drives_data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if drives.is_empty() {
        eprintln!("No drives found.");
        return Ok(());
    }

    if !quiet {
        eprintln!(
            "{} Found {} drives. Fetching contents...",
            "→".cyan().bold(),
            drives.len()
        );
    }

    // Step 2: Extract nodes from each drive's state (already fetched inline)
    // and also fetch via document() for childIds if state.nodes is missing
    let node_futs: Vec<_> = drives
        .iter()
        .map(|d| {
            let id = d["id"].as_str().unwrap_or("").to_string();
            let state_nodes = d
                .pointer("/state/nodes")
                .and_then(|v| v.as_array())
                .cloned();
            let client = client.clone();
            async move {
                // If state.nodes was returned inline, use it directly
                if let Some(nodes) = state_nodes {
                    return Ok(nodes);
                }
                // Otherwise fetch via document() query
                fetch_drive_nodes(&client, &id).await
            }
        })
        .collect();

    let node_results = futures_util::future::join_all(node_futs).await;

    let mut drive_with_nodes: Vec<(Value, Vec<Value>)> = Vec::new();
    for (drive, result) in drives.into_iter().zip(node_results) {
        let nodes = match result {
            Ok(n) => n,
            Err(e) => {
                let name = drive["name"].as_str().unwrap_or("?");
                eprintln!(
                    "{} Failed to fetch nodes for drive '{name}': {e}",
                    "⚠".yellow()
                );
                Vec::new()
            }
        };
        drive_with_nodes.push((drive, nodes));
    }

    // Step 3: Enrich file metadata with revisions
    if !quiet {
        eprintln!("{} Enriching document metadata...", "→".cyan().bold());
    }

    let revisions = fetch_revisions(&client, &drive_with_nodes).await;

    // Step 4: Build the unified tree
    let mut tree = build_drive_tree(&drive_with_nodes, &revisions);
    tree.url = Some(client.url.clone());
    tree.profile = Some(name.clone());

    // Step 5: Render based on format
    let resolved_out = output::resolve_visual_output(out, format, "visualize");
    let out_ref = resolved_out.as_deref();

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&tree)?;
            output::write_output(json.as_bytes(), out, false)?;
        }
        OutputFormat::Svg => {
            let svg = output::svg::render_svg(&tree);
            output::write_output(svg.as_bytes(), out_ref, false)?;
        }
        OutputFormat::Png => {
            let svg = output::svg::render_svg(&tree);
            let png_bytes = output::png::render_png(&svg)?;
            output::write_output(&png_bytes, out_ref, true)?;
        }
        OutputFormat::Mermaid => {
            let mmd = render_mermaid(&tree);
            output::write_output(mmd.as_bytes(), out_ref, false)?;
        }
        _ => {
            // Table / Raw — terminal tree output
            print_all_drives(&tree);
        }
    }

    Ok(())
}

async fn fetch_drive_nodes(client: &GraphQLClient, drive_id: &str) -> Result<Vec<Value>> {
    let escaped = drive_id.replace('"', r#"\""#);
    let query = format!(
        r#"{{ document(identifier: "{escaped}") {{
            document {{ state }}
            childIds
        }} }}"#,
    );
    let data = client.query(&query, None).await?;
    Ok(data
        .pointer("/document/document/state/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Fetch document revisions by querying children of each drive in parallel.
async fn fetch_revisions(
    client: &GraphQLClient,
    drive_with_nodes: &[(Value, Vec<Value>)],
) -> HashMap<String, u64> {
    let mut futs = Vec::new();

    for (drive, _) in drive_with_nodes {
        let drive_id = drive["id"].as_str().unwrap_or("").to_string();
        let client = client.clone();

        futs.push(async move {
            let escaped = drive_id.replace('"', r#"\""#);
            let query = format!(
                r#"{{ findDocuments(search: {{ parentId: "{escaped}" }}) {{ items {{ id revisionsList {{ scope revision }} }} }} }}"#,
            );
            match client.query(&query, None).await {
                Ok(data) => {
                    let docs = data
                        .pointer("/findDocuments/items")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    docs.into_iter()
                        .filter_map(|d| {
                            let id = d["id"].as_str()?.to_string();
                            // Find the "global" revision from revisionsList
                            let rev = d["revisionsList"]
                                .as_array()
                                .and_then(|arr| {
                                    arr.iter().find_map(|r| {
                                        if r["scope"].as_str() == Some("global") {
                                            r["revision"].as_u64()
                                        } else {
                                            None
                                        }
                                    })
                                })
                                // Fallback: take the first revision entry
                                .or_else(|| {
                                    d["revisionsList"]
                                        .as_array()
                                        .and_then(|arr| arr.first())
                                        .and_then(|r| r["revision"].as_u64())
                                })?;
                            Some((id, rev))
                        })
                        .collect::<Vec<_>>()
                }
                Err(_) => Vec::new(),
            }
        });
    }

    let results = futures_util::future::join_all(futs).await;
    let mut revisions = HashMap::new();
    for pairs in results {
        for (id, rev) in pairs {
            revisions.insert(id, rev);
        }
    }
    revisions
}

/// Print all drives as terminal tree output (default table format)
fn print_all_drives(tree: &DriveTree) {
    for (i, drive) in tree.drives.iter().enumerate() {
        if i > 0 {
            println!();
        }

        println!("{} {}", "Drive:".bold(), drive.name.bold());
        println!("  ID:       {}", drive.id);
        println!("  Slug:     {}", drive.slug);
        println!("  Revision: {}", drive.revision);
        println!("  Type:     {}", drive.document_type);
        if let Some(ref editor) = drive.editor {
            println!("  Editor:   {editor}");
        }
        println!(
            "  Contents: {} files, {} folders",
            drive.file_count, drive.folder_count
        );

        if !drive.children.is_empty() {
            println!();
            print_tree_entries(&drive.children, "");
        }
    }
}

fn print_tree_entries(entries: &[TreeEntry], indent: &str) {
    // Separate folders and files
    let folders: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e, TreeEntry::Folder(_)))
        .collect();
    let files: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e, TreeEntry::File(_)))
        .collect();

    // Print files as a simple list
    for file in &files {
        if let TreeEntry::File(f) = file {
            let rev_str = f.revision.map(|r| format!(" rev:{r}")).unwrap_or_default();
            println!("{indent}  📄 {} ({}{})", f.name, f.document_type, rev_str);
        }
    }

    // Print folders with tree connectors
    for (i, entry) in folders.iter().enumerate() {
        if let TreeEntry::Folder(folder) = entry {
            let is_last = i == folders.len() - 1;
            let connector = if is_last { "└── " } else { "├── " };
            let child_indent = if is_last { "    " } else { "│   " };

            println!("{indent}{connector}📁 {}/", folder.name);
            print_tree_entries(&folder.children, &format!("{indent}{child_indent}"));
        }
    }
}
