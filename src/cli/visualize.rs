use std::collections::HashMap;

use anyhow::Result;
use colored::Colorize;
use serde_json::Value;

use crate::cli::helpers;
use crate::graphql::GraphQLClient;
use crate::output::{
    self, build_drive_tree, render_mermaid, DriveTree, OutputFormat, TreeEntry,
};

pub async fn run(
    format: OutputFormat,
    out: Option<&str>,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    if !quiet {
        eprintln!(
            "{} Fetching drives from profile '{name}'...",
            "→".cyan().bold()
        );
    }

    // Step 1: Fetch all drives
    let drives_data = client
        .query(
            r#"{ driveDocuments { id name slug documentType revision meta { preferredEditor } } }"#,
            None,
        )
        .await?;

    let drives: Vec<Value> = drives_data
        .get("driveDocuments")
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

    // Step 2: Fetch nodes for each drive in parallel
    let node_futs: Vec<_> = drives
        .iter()
        .map(|d| {
            let id = d["id"].as_str().unwrap_or("").to_string();
            let client = client.clone();
            async move { fetch_drive_nodes(&client, &id).await }
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

    // Step 3: Enrich file metadata with revisions from model namespaces
    if !quiet {
        eprintln!(
            "{} Enriching document metadata...",
            "→".cyan().bold()
        );
    }

    let revisions = fetch_revisions(&client, &cache, &drive_with_nodes).await;

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
    let query = format!(
        r#"{{ driveDocument(idOrSlug: "{drive_id}") {{
            state {{
                nodes {{
                    ... on DocumentDrive_FileNode {{ id name kind documentType parentFolder }}
                    ... on DocumentDrive_FolderNode {{ id name kind parentFolder }}
                }}
            }}
        }} }}"#,
    );
    let data = client.query(&query, None).await?;
    Ok(data
        .pointer("/driveDocument/state/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Fetch document revisions by querying model namespaces in parallel.
/// For each model with `getDocuments` × each drive, fires a parallel query.
async fn fetch_revisions(
    client: &GraphQLClient,
    cache: &crate::graphql::IntrospectionCache,
    drive_with_nodes: &[(Value, Vec<Value>)],
) -> HashMap<String, u64> {
    let mut futs = Vec::new();

    for model in cache.models.values() {
        if !model.query_fields.iter().any(|f| f == "getDocuments") {
            continue;
        }

        for (drive, _) in drive_with_nodes {
            let drive_id = drive["id"].as_str().unwrap_or("").to_string();
            let prefix = model.prefix.clone();
            let client = client.clone();

            futs.push(async move {
                let query = format!(
                    r#"{{ {prefix} {{ getDocuments(driveId: "{drive_id}") {{ id revision }} }} }}"#,
                );
                match client.query(&query, None).await {
                    Ok(data) => {
                        let docs = data
                            .get(&prefix)
                            .and_then(|v| v.get("getDocuments"))
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        docs.into_iter()
                            .filter_map(|d| {
                                let id = d["id"].as_str()?.to_string();
                                let rev = d["revision"].as_u64()?;
                                Some((id, rev))
                            })
                            .collect::<Vec<_>>()
                    }
                    Err(_) => Vec::new(),
                }
            });
        }
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

        println!(
            "{} {}",
            "Drive:".bold(),
            drive.name.bold()
        );
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
            let rev_str = f
                .revision
                .map(|r| format!(" rev:{r}"))
                .unwrap_or_default();
            println!(
                "{indent}  📄 {} ({}{})",
                f.name, f.document_type, rev_str
            );
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
