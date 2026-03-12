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
        /// Drive slug (human-readable URL identifier)
        #[arg(long)]
        slug: Option<String>,
        /// Custom drive ID (omit to let server auto-generate a UUID)
        #[arg(long)]
        id: Option<String>,
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
            slug,
            id,
            icon,
            preferred_editor,
        } => create(name, slug, id, icon, preferred_editor, format, profile_name).await,
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

    let drives = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

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

    let query = format!(
        r#"{{ document(identifier: "{id}") {{ document {{ id name slug documentType state revisionsList {{ scope revision }} }} childIds }} }}"#,
        id = id.replace('"', r#"\""#)
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
            println!(
                "Type:     {}",
                doc["documentType"].as_str().unwrap_or("-")
            );

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
    slug: Option<String>,
    _id: Option<String>,
    icon: Option<String>,
    _preferred_editor: Option<String>,
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

    let _slug = match slug {
        Some(s) => s,
        None if interactive => {
            let default_slug = name
                .to_lowercase()
                .replace(' ', "-")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>();
            Input::new()
                .with_prompt("Slug")
                .default(default_slug)
                .interact_text()?
        }
        None => name
            .to_lowercase()
            .replace(' ', "-")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>(),
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

    // Step 1: Create drive document
    let create_mutation = format!(
        r#"mutation {{ DocumentDrive_createDocument(name: "{name}") {{ id slug name }} }}"#,
        name = name.replace('"', r#"\""#)
    );
    let create_data = client.query(&create_mutation, None).await?;
    let drive = &create_data["DocumentDrive_createDocument"];
    let doc_id = drive["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No id returned from create"))?;

    // Step 2: Set drive name via DocumentDrive_setDriveName
    let set_name_mutation = format!(
        r#"mutation {{ DocumentDrive_setDriveName(docId: "{doc_id}", input: {{ name: "{name}" }}) {{ id name slug }} }}"#,
        doc_id = doc_id.replace('"', r#"\""#),
        name = name.replace('"', r#"\""#),
    );
    let name_data = client.query(&set_name_mutation, None).await?;
    let drive = &name_data["DocumentDrive_setDriveName"];

    // Step 3: Optionally set icon
    if let Some(ref icon_url) = icon {
        let icon_mutation = format!(
            r#"mutation {{ DocumentDrive_setDriveIcon(docId: "{doc_id}", input: {{ icon: "{icon}" }}) {{ id }} }}"#,
            doc_id = doc_id.replace('"', r#"\""#),
            icon = icon_url.replace('"', r#"\""#),
        );
        client.query(&icon_mutation, None).await?;
    }

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(drive),
        _ => {
            let slug = drive["slug"].as_str().unwrap_or("-");
            let base = helpers::base_url_from(&client.url);
            println!("{} Drive created", "✓".green());
            println!("  ID:   {}", drive["id"].as_str().unwrap_or(doc_id));
            println!("  Slug: {}", slug);
            println!("  Name: {}", drive["name"].as_str().unwrap_or("-"));
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

    // Use batch deleteDocuments API
    let id_list: String = ids
        .iter()
        .map(|id| format!("\"{}\"", id.replace('"', r#"\""#)))
        .collect::<Vec<_>>()
        .join(", ");
    let mutation = format!(
        r#"mutation {{ deleteDocuments(identifiers: [{id_list}], propagate: CASCADE) }}"#
    );

    match client.query(&mutation, None).await {
        Ok(_) => {
            for id in ids {
                println!("{} Deleted drive {id}", "✓".green());
            }
        }
        Err(e) => {
            bail!("Failed to delete drives: {e}");
        }
    }

    Ok(())
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
