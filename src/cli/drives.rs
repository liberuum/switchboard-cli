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

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(drives)),
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
                    ]
                })
                .collect();
            print_table(&["ID", "Name", "Slug"], &rows);
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
    let nested = helpers::is_nested_api(&client).await;

    let mut vars_map = serde_json::json!({
        "name": name,
        "slug": slug,
    });
    if let Some(ref editor) = preferred_editor {
        vars_map["preferredEditor"] = serde_json::json!(editor);
    }

    let create_mutation = if nested {
        "mutation($name: String!, $slug: String, $preferredEditor: String) { \
         DocumentDrive { createDocument(name: $name, slug: $slug, preferredEditor: $preferredEditor) \
         { id slug name } } }"
    } else {
        "mutation($name: String!, $slug: String, $preferredEditor: String) { \
         DocumentDrive_createDocument(name: $name, slug: $slug, preferredEditor: $preferredEditor) \
         { id slug name } }"
    };

    let create_data = client.query(create_mutation, Some(&vars_map)).await?;
    let drive = if nested {
        &create_data["DocumentDrive"]["createDocument"]
    } else {
        &create_data["DocumentDrive_createDocument"]
    };

    // Optionally set icon
    if let Some(ref icon_url) = icon {
        let doc_id = drive["id"].as_str().unwrap_or("");
        let icon_vars = serde_json::json!({
            "docId": doc_id,
            "input": { "icon": icon_url }
        });
        let icon_mutation = if nested {
            "mutation($docId: PHID!, $input: DocumentDrive_SetDriveIconInput!) { DocumentDrive { setDriveIcon(docId: $docId, input: $input) { id } } }"
        } else {
            "mutation($docId: PHID!, $input: DocumentDrive_SetDriveIconInput!) { DocumentDrive_setDriveIcon(docId: $docId, input: $input) { id } }"
        };
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

    // The API's rebuild-before-delete path requires UUID identifiers, not slugs.
    // Resolve each identifier to its UUID — required by the API's delete path.
    // Try document() lookup first, then fall back to findDocuments name search.
    let mutation = "mutation($identifier: String!) { deleteDocument(identifier: $identifier, propagate: CASCADE) }";
    let mut failed = false;
    for id in ids {
        // Resolve to UUID, then verify the result isn't already soft-deleted.
        // The API's document(identifier) can match deleted docs by name, so we
        // skip those and fall through to the name-search which filters them out.
        let uuid = match helpers::resolve_doc(&client, id).await {
            Ok(u) if !is_soft_deleted(&client, &u).await => u,
            _ => {
                // Either resolution failed or the resolved doc is already deleted —
                // search by name among non-deleted drives.
                match find_drive_uuid_by_name(&client, id).await {
                    Some(u) => u,
                    None => {
                        eprintln!("{} Drive '{id}' not found", "✗".red());
                        failed = true;
                        continue;
                    }
                }
            }
        };
        let vars = serde_json::json!({ "identifier": uuid });
        match client.query(mutation, Some(&vars)).await {
            Ok(_) => println!("{} Deleted drive {id}", "✓".green()),
            Err(e) => {
                eprintln!("{} Failed to delete drive {id}: {e}", "✗".red());
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
