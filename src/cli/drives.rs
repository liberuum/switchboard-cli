use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Confirm, Input};
use serde_json::Value;

use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};

use crate::cli::helpers;
use crate::output::{self, OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum DrivesCommand {
    /// List all drives
    List,
    /// Get drive details
    Get {
        /// Drive ID or slug
        id: String,
        /// Output file path (for svg/png/mermaid formats)
        #[arg(long, short)]
        out: Option<String>,
    },
    /// Create a new drive
    Create {
        /// Drive name
        #[arg(long)]
        name: Option<String>,
        /// Icon URL
        #[arg(long)]
        icon: Option<String>,
        /// Preferred editor
        #[arg(long)]
        preferred_editor: Option<String>,
    },
    /// Delete one or more drives
    Delete {
        /// Drive IDs or slugs
        ids: Vec<String>,
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Check a drive for ghost nodes (orphan file references with no document)
    Check {
        /// Drive ID or slug
        id: String,
    },
    /// Fix a drive by removing ghost nodes
    Fix {
        /// Drive ID or slug
        id: String,
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

pub async fn run(
    cmd: DrivesCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    match cmd {
        DrivesCommand::List => list(format, profile_name).await,
        DrivesCommand::Get { id, out } => get(&id, format, out.as_deref(), profile_name).await,
        DrivesCommand::Create {
            name,
            icon,
            preferred_editor,
        } => create(name, icon, preferred_editor, format, profile_name).await,
        DrivesCommand::Delete { ids, yes } => delete(&ids, yes, profile_name).await,
        DrivesCommand::Check { id } => check(&id, format, profile_name).await,
        DrivesCommand::Fix { id, yes } => fix(&id, yes, format, profile_name).await,
    }
}

async fn list(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug documentType state } } }"#,
            None,
        )
        .await?;

    let drives: Vec<Value> = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| {
            d.pointer("/state/document/isDeleted")
                .and_then(|v| v.as_bool())
                != Some(true)
        })
        .collect();

    // Count documents (file nodes) per drive from the state we already fetched
    let doc_count = |d: &Value| -> usize {
        d.pointer("/state/global/nodes")
            .and_then(|v| v.as_array())
            .map(|nodes| {
                nodes
                    .iter()
                    .filter(|n| n["kind"].as_str() == Some("file"))
                    .count()
            })
            .unwrap_or(0)
    };

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let enriched: Vec<Value> = drives
                .iter()
                .map(|d| {
                    let mut e = d.clone();
                    if let Some(obj) = e.as_object_mut() {
                        obj.insert("documentCount".into(), Value::from(doc_count(d)));
                    }
                    e
                })
                .collect();
            print_json(&Value::Array(enriched))
        }
        _ => {
            if drives.is_empty() {
                println!("No drives found.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = drives
                .iter()
                .map(|d| {
                    vec![
                        d["id"].as_str().unwrap_or("-").to_string(),
                        d["name"].as_str().unwrap_or("-").to_string(),
                        d["slug"].as_str().unwrap_or("-").to_string(),
                        doc_count(d).to_string(),
                    ]
                })
                .collect();
            print_table(&["ID", "Name", "Slug", "Docs"], &rows);
        }
    }

    Ok(())
}

async fn get(
    id: &str,
    format: OutputFormat,
    out: Option<&str>,
    profile_name: Option<&str>,
) -> Result<()> {
    let (name, _profile, client) = helpers::setup(profile_name)?;

    // Resolve name/slug/UUID to a UUID the API understands
    let resolved = helpers::resolve_doc(&client, id).await?;
    let query = format!(
        r#"{{ document(identifier: "{resolved}") {{ document {{ id name slug documentType preferredEditor state revisionsList {{ scope revision }} }} childIds }} }}"#,
        resolved = resolved.replace('"', r#"\""#)
    );

    let data = client.query(&query, None).await?;
    let doc = &data["document"]["document"];

    // Handle visual formats (SVG/PNG/Mermaid)
    if format.is_visual() {
        let nodes = doc
            .pointer("/state/global/nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let drive_data = vec![(doc.clone(), nodes)];
        let revisions = std::collections::HashMap::new();
        let mut tree = output::build_drive_tree(&drive_data, &revisions);
        tree.url = Some(client.url.clone());
        tree.profile = Some(name.clone());

        let resolved_out = output::resolve_visual_output(out, format, "drive");
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
        OutputFormat::Json | OutputFormat::Raw => print_json(doc),
        _ => {
            println!("ID:       {}", doc["id"].as_str().unwrap_or("-"));
            println!("Name:     {}", doc["name"].as_str().unwrap_or("-"));
            println!("Slug:     {}", doc["slug"].as_str().unwrap_or("-"));
            // Show revision from revisionsList
            if let Some(revisions) = doc["revisionsList"].as_array() {
                let rev_str: Vec<String> = revisions
                    .iter()
                    .map(|r| {
                        format!(
                            "{}:{}",
                            r["scope"].as_str().unwrap_or("?"),
                            r["revision"].as_u64().unwrap_or(0)
                        )
                    })
                    .collect();
                println!("Revision: {}", rev_str.join(", "));
            }
            println!("Type:     {}", doc["documentType"].as_str().unwrap_or("-"));
            if let Some(editor) = doc["preferredEditor"].as_str().filter(|s| !s.is_empty()) {
                println!("Editor:   {editor}");
            }

            // Show contents as a tree with metadata from state.global.nodes
            if let Some(nodes) = doc
                .pointer("/state/global/nodes")
                .and_then(|v| v.as_array())
            {
                let files = nodes
                    .iter()
                    .filter(|n| n["kind"].as_str() == Some("file"))
                    .count();
                let folders = nodes
                    .iter()
                    .filter(|n| n["kind"].as_str() == Some("folder"))
                    .count();
                println!("\nContents: {files} files, {folders} folders");

                if files > 0 || folders > 0 {
                    println!();
                    print_drive_tree(nodes, None, "");
                }
            }
        }
    }

    Ok(())
}

async fn create(
    name: Option<String>,
    icon: Option<String>,
    preferred_editor: Option<String>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_pname, _profile, client) = helpers::setup(profile_name)?;

    // Determine if we're running interactively: if name is provided, skip prompts
    let interactive = name.is_none();

    let name = match name {
        Some(n) => n,
        None => Input::new().with_prompt("Drive name").interact_text()?,
    };

    let icon = match icon {
        Some(i) if !i.is_empty() => Some(i),
        Some(_) => None,
        None if interactive => {
            let i: String = Input::new()
                .with_prompt("Icon URL (optional, press Enter to skip)")
                .default(String::new())
                .interact_text()?;
            if i.is_empty() { None } else { Some(i) }
        }
        None => None,
    };

    let preferred_editor = match preferred_editor {
        Some(e) if !e.is_empty() => Some(e),
        Some(_) => None,
        None if interactive => {
            let e: String = Input::new()
                .with_prompt("Preferred editor (optional, press Enter to skip)")
                .default(String::new())
                .interact_text()?;
            if e.is_empty() { None } else { Some(e) }
        }
        None => None,
    };

    // Derive slug from name
    let slug: String = name
        .to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();

    // Use the DocumentDrive createDocument mutation so the API records a
    // CREATE_DOCUMENT operation (required for soft-delete to work later).
    let mut vars_map = serde_json::json!({
        "name": name,
        "slug": slug,
    });
    if let Some(ref editor) = preferred_editor {
        vars_map["preferredEditor"] = serde_json::json!(editor);
    }

    let create_mutation = "mutation($name: String!, $slug: String, $preferredEditor: String) { \
         DocumentDrive { createDocument(name: $name, slug: $slug, preferredEditor: $preferredEditor) \
         { id slug name } } }";

    let create_data = client.query(create_mutation, Some(&vars_map)).await?;
    let drive = &create_data["DocumentDrive"]["createDocument"];

    // Set the state-level drive name (state.global.name).
    // createDocument only sets the metadata name — the /d/<slug> endpoint
    // and Connect UI read from state.global.name which defaults to "".
    let doc_id = drive["id"].as_str().unwrap_or("");
    if !doc_id.is_empty() {
        let name_vars = serde_json::json!({
            "docId": doc_id,
            "input": { "name": name }
        });
        let name_mutation = "mutation($docId: PHID!, $input: DocumentDrive_SetDriveNameInput!) { DocumentDrive { setDriveName(docId: $docId, input: $input) { id } } }";
        let _ = client.query(name_mutation, Some(&name_vars)).await;
    }

    // Optionally set icon
    if let Some(ref icon_url) = icon {
        let doc_id = drive["id"].as_str().unwrap_or("");
        let icon_vars = serde_json::json!({
            "docId": doc_id,
            "input": { "icon": icon_url }
        });
        let icon_mutation = "mutation($docId: PHID!, $input: DocumentDrive_SetDriveIconInput!) { DocumentDrive { setDriveIcon(docId: $docId, input: $input) { id } } }";
        client.query(icon_mutation, Some(&icon_vars)).await?;
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(drive),
        _ => {
            let slug = drive["slug"].as_str().unwrap_or("-");
            let base = helpers::base_url_from(&client.url);
            println!("{} Drive created", "✓".green());
            println!("  ID:   {}", drive["id"].as_str().unwrap_or("-"));
            println!("  Slug: {}", slug);
            println!("  Name: {}", drive["name"].as_str().unwrap_or("-"));
            if let Some(ref editor) = preferred_editor {
                println!("  Editor: {}", editor);
            }
            println!("  URL:  {}/d/{}", base, slug);
        }
    }

    Ok(())
}

async fn delete(ids: &[String], skip_confirm: bool, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    if !skip_confirm {
        let label = if ids.len() == 1 {
            format!("Delete drive {}?", ids[0])
        } else {
            format!("Delete {} drives?", ids.len())
        };
        let confirm = Confirm::new()
            .with_prompt(label)
            .default(false)
            .interact()?;
        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mut failed = false;
    let mut resolved: Vec<(String, String)> = Vec::new(); // (display_id, uuid)
    for id in ids {
        let uuid = match helpers::resolve_doc(&client, id).await {
            Ok(u) if !is_soft_deleted(&client, &u).await => u,
            _ => match find_drive_uuid_by_name(&client, id).await {
                Some(u) => u,
                None => {
                    eprintln!("{} Drive '{id}' not found", "✗".red());
                    failed = true;
                    continue;
                }
            },
        };
        resolved.push((id.clone(), uuid));
    }

    if !resolved.is_empty() {
        let identifiers: Vec<&String> = resolved.iter().map(|(_, u)| u).collect();
        let mutation = "mutation($identifiers: [String!]!) { deleteDocuments(identifiers: $identifiers, propagate: CASCADE) }";
        let vars = serde_json::json!({ "identifiers": identifiers });
        match client.query(mutation, Some(&vars)).await {
            Ok(_) => {
                for (display_id, _) in &resolved {
                    println!("{} Deleted drive {display_id}", "✓".green());
                }
            }
            Err(e) => {
                eprintln!("{} Batch delete failed: {e}", "✗".red());
                failed = true;
            }
        }
    }

    if failed {
        bail!("One or more drives could not be deleted");
    }

    Ok(())
}

/// Returns true if the document with the given UUID is already soft-deleted.
async fn is_soft_deleted(client: &crate::graphql::GraphQLClient, uuid: &str) -> bool {
    let query = format!(
        r#"{{ document(identifier: "{uuid}") {{ document {{ state }} }} }}"#,
        uuid = uuid.replace('"', r#"\""#)
    );
    client
        .query(&query, None)
        .await
        .ok()
        .and_then(|d| {
            d.pointer("/document/document/state/document/isDeleted")
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(false)
}

/// Search all drives for one whose name matches `name` (case-insensitive) and return its UUID.
/// Skips soft-deleted drives (state.document.isDeleted == true).
async fn find_drive_uuid_by_name(
    client: &crate::graphql::GraphQLClient,
    name: &str,
) -> Option<String> {
    let data = client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name state } } }"#,
            None,
        )
        .await
        .ok()?;
    data.pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|d| {
                let deleted = d
                    .pointer("/state/document/isDeleted")
                    .and_then(|v| v.as_bool())
                    == Some(true);
                !deleted && d["name"].as_str().unwrap_or("").eq_ignore_ascii_case(name)
            })
        })
        .and_then(|d| d["id"].as_str())
        .map(String::from)
}

/// Print drive contents as a hybrid tree (folders) + table (documents) view.
/// Folders are rendered with tree connectors; documents inside each folder are
/// displayed as a formatted table indented under the folder.
fn print_drive_tree(nodes: &[Value], parent: Option<&str>, indent: &str) {
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

    let folders: Vec<&Value> = children
        .iter()
        .filter(|n| n["kind"].as_str() == Some("folder"))
        .copied()
        .collect();

    let files: Vec<&Value> = children
        .iter()
        .filter(|n| n["kind"].as_str() == Some("file"))
        .copied()
        .collect();

    // Render documents as an indented table
    if !files.is_empty() {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Disabled);
        table.set_header(["ID", "Name", "Type"]);
        for f in &files {
            table.add_row(vec![
                f["id"].as_str().unwrap_or("-"),
                f["name"].as_str().unwrap_or("-"),
                f["documentType"].as_str().unwrap_or("-"),
            ]);
        }
        for line in table.to_string().lines() {
            println!("{indent}{line}");
        }
    }

    // Render sub-folders as tree entries
    for (i, folder) in folders.iter().enumerate() {
        let is_last = i == folders.len() - 1;
        let connector = if is_last {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251c}\u{2500}\u{2500} "
        };
        let child_indent = if is_last { "    " } else { "\u{2502}   " };

        let name = folder["name"].as_str().unwrap_or("-");
        let id = folder["id"].as_str().unwrap_or("");

        println!("{indent}{connector}\u{1f4c1} {name}/");
        print_drive_tree(nodes, Some(id), &format!("{indent}{child_indent}"));
    }
}

async fn check(id: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let resolved = helpers::resolve_doc(&client, id)
        .await
        .unwrap_or_else(|_| id.to_string());

    eprintln!("Scanning drive for ghost nodes...");
    let ghosts = crate::cli::docs::find_ghost_nodes(&client, &resolved).await?;

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            let items: Vec<Value> = ghosts
                .iter()
                .map(|(id, name, dtype)| {
                    serde_json::json!({ "id": id, "name": name, "documentType": dtype })
                })
                .collect();
            crate::output::print_json(&Value::Array(items));
        }
        _ => {
            if ghosts.is_empty() {
                println!("{} No ghost nodes found — drive is clean", "✓".green());
            } else {
                println!("{} {} ghost node(s) found:", "⚠".yellow(), ghosts.len());
                for (ghost_id, name, dtype) in &ghosts {
                    println!("  {} {} ({}) [{}]", "✗".red(), name, dtype, ghost_id);
                }
                println!();
                println!(
                    "Run {} to remove orphan nodes",
                    format!("drives fix {id}").bold()
                );
            }
        }
    }

    Ok(())
}

async fn fix(
    id: &str,
    skip_confirm: bool,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let resolved = helpers::resolve_doc(&client, id)
        .await
        .unwrap_or_else(|_| id.to_string());

    eprintln!("Scanning drive for ghost nodes...");
    let ghosts = crate::cli::docs::find_ghost_nodes(&client, &resolved).await?;

    if ghosts.is_empty() {
        println!("{} No ghost nodes found — drive is clean", "✓".green());
        return Ok(());
    }

    println!("{} {} ghost node(s) found:", "⚠".yellow(), ghosts.len());
    for (ghost_id, name, dtype) in &ghosts {
        println!("  {} ({}) [{}]", name, dtype, ghost_id);
    }

    if !skip_confirm {
        let confirm = dialoguer::Confirm::new()
            .with_prompt(format!("Remove {} ghost node(s)?", ghosts.len()))
            .default(false)
            .interact()?;
        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Use mutateDocument with a raw DELETE_NODE action instead of the
    // model-specific deleteNode mutation. The model mutation goes through
    // the reactor job queue and may silently fail for ghost nodes (the
    // reactor tries to validate the referenced document which doesn't exist).
    // mutateDocument applies the action directly to the drive's state.
    let remove_mutation = "mutation($id: String!, $actions: [JSONObject!]!) { mutateDocument(documentIdentifier: $id, actions: $actions) { id name } }";

    let mut fixed = 0;
    let mut results = Vec::new();
    for (ghost_id, name, dtype) in &ghosts {
        let ts = crate::cli::docs::iso_now();
        let action_id = crate::cli::docs::gen_action_id();
        let vars = serde_json::json!({
            "id": resolved,
            "actions": [{
                "id": action_id,
                "type": "DELETE_NODE",
                "input": { "id": ghost_id },
                "scope": "global",
                "timestampUtcMs": ts
            }]
        });
        match client.query(remove_mutation, Some(&vars)).await {
            Ok(_) => {
                println!(
                    "{} Removed ghost node \"{}\" ({})",
                    "✓".green(),
                    name,
                    ghost_id
                );
                fixed += 1;
                results.push(serde_json::json!({
                    "id": ghost_id, "name": name, "documentType": dtype, "status": "removed"
                }));
            }
            Err(e) => {
                eprintln!("{} Failed to remove \"{}\": {e}", "✗".red(), name);
                results.push(serde_json::json!({
                    "id": ghost_id, "name": name, "documentType": dtype, "status": "failed"
                }));
            }
        }
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => {
            crate::output::print_json(&Value::Array(results));
        }
        _ => {
            println!();
            println!(
                "{} Fixed {}/{} ghost nodes",
                if fixed == ghosts.len() {
                    "✓".green()
                } else {
                    "⚠".yellow()
                },
                fixed,
                ghosts.len()
            );
        }
    }

    Ok(())
}
