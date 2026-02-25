use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use dialoguer::{Confirm, Input};
use serde_json::Value;

use crate::cli::helpers::{self, resolve_drive_id, truncate};
use crate::output::{OutputFormat, print_json, print_table};

#[derive(Subcommand)]
pub enum DrivesCommand {
    /// List all drives
    List,
    /// Get drive details
    Get {
        /// Drive ID or slug
        id: String,
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
    /// Delete a drive
    Delete {
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
        DrivesCommand::Get { id } => get(&id, format, profile_name).await,
        DrivesCommand::Create {
            name,
            slug,
            id,
            icon,
            preferred_editor,
        } => create(name, slug, id, icon, preferred_editor, format, profile_name).await,
        DrivesCommand::Delete { id, yes } => delete(&id, yes, profile_name).await,
    }
}

async fn list(format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let data = client
        .query(
            "{ driveDocuments { id name slug documentType revision } }",
            None,
        )
        .await?;

    let drives = data
        .get("driveDocuments")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&Value::Array(drives)),
        OutputFormat::Table => {
            if drives.is_empty() {
                println!("No drives found.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = drives
                .iter()
                .map(|d| {
                    vec![
                        truncate(d["id"].as_str().unwrap_or("-"), 24),
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

async fn get(id: &str, format: OutputFormat, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    let query = format!(
        r#"{{
  driveDocument(idOrSlug: "{id}") {{
    id name slug revision documentType
    state {{
      name icon
      nodes {{
        ... on DocumentDrive_FileNode {{ id name kind documentType parentFolder }}
        ... on DocumentDrive_FolderNode {{ id name kind parentFolder }}
      }}
    }}
  }}
}}"#,
        id = id.replace('"', r#"\""#)
    );

    let data = client.query(&query, None).await?;
    let drive = &data["driveDocument"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(drive),
        OutputFormat::Table => {
            println!("ID:       {}", drive["id"].as_str().unwrap_or("-"));
            println!("Name:     {}", drive["name"].as_str().unwrap_or("-"));
            println!("Slug:     {}", drive["slug"].as_str().unwrap_or("-"));
            println!("Revision: {}", drive["revision"]);
            println!(
                "Type:     {}",
                drive["documentType"].as_str().unwrap_or("-")
            );

            // Show nodes summary
            if let Some(nodes) = drive.pointer("/state/nodes").and_then(|v| v.as_array()) {
                let files = nodes
                    .iter()
                    .filter(|n| n["kind"].as_str() == Some("file"))
                    .count();
                let folders = nodes
                    .iter()
                    .filter(|n| n["kind"].as_str() == Some("folder"))
                    .count();
                println!("\nContents: {files} files, {folders} folders");
            }
        }
    }

    Ok(())
}

async fn create(
    name: Option<String>,
    slug: Option<String>,
    id: Option<String>,
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

    let default_slug = name
        .to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>();

    let slug = match slug {
        Some(s) => s,
        None if interactive => Input::new()
            .with_prompt("Slug")
            .default(default_slug)
            .interact_text()?,
        None => default_slug,
    };

    let id = match id {
        Some(i) if !i.is_empty() => Some(i),
        Some(_) => None,
        None if interactive => {
            let i: String = Input::new()
                .with_prompt("Custom ID (optional, press Enter for auto-generated UUID)")
                .default(String::new())
                .interact_text()?;
            if i.is_empty() { None } else { Some(i) }
        }
        None => None,
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

    // Build mutation — only include non-empty optional fields
    let mut args = format!(r#"name: "{name}""#);
    if !slug.is_empty() {
        args.push_str(&format!(r#", slug: "{slug}""#));
    }
    if let Some(ref id) = id {
        args.push_str(&format!(r#", id: "{id}""#));
    }
    if let Some(ref icon) = icon {
        args.push_str(&format!(r#", icon: "{icon}""#));
    }
    if let Some(ref editor) = preferred_editor {
        args.push_str(&format!(r#", preferredEditor: "{editor}""#));
    }

    let mutation =
        format!(r#"mutation {{ addDrive({args}) {{ id slug name icon preferredEditor }} }}"#);

    let data = client.query(&mutation, None).await?;
    let drive = &data["addDrive"];

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(drive),
        OutputFormat::Table => {
            println!("{} Drive created", "✓".green());
            println!("  ID:   {}", drive["id"].as_str().unwrap_or("-"));
            println!("  Slug: {}", drive["slug"].as_str().unwrap_or("-"));
            println!("  Name: {}", drive["name"].as_str().unwrap_or("-"));
        }
    }

    Ok(())
}

async fn delete(id: &str, skip_confirm: bool, profile_name: Option<&str>) -> Result<()> {
    let (_name, _profile, client) = helpers::setup(profile_name)?;

    // Must resolve to UUID — deleteDrive silently fails with slugs
    let uuid = resolve_drive_id(&client, id).await?;

    if id != uuid {
        println!("Resolved slug \"{}\" → UUID {}", id, &uuid[..12]);
    }

    if !skip_confirm {
        let confirm = Confirm::new()
            .with_prompt(format!("Delete drive {uuid}?"))
            .default(false)
            .interact()?;
        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mutation = format!(r#"mutation {{ deleteDrive(id: "{uuid}") }}"#);
    client.query(&mutation, None).await?;

    println!("{} Drive deleted.", "✓".green());
    Ok(())
}
