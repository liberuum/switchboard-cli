use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use uuid::Uuid;

use crate::cli::helpers;
use crate::output::{OutputFormat, print_json};

#[derive(Subcommand)]
pub enum FoldersCommand {
    /// Create a new folder. The parent can be either a drive (folder goes
    /// at root) or another folder (folder is nested).
    Create {
        /// Folder name
        #[arg(long)]
        name: String,
        /// Parent — drive name/slug/UUID for root placement, or folder
        /// name/UUID for nested placement. Either `--parent` or `--drive`
        /// must be given.
        #[arg(long, alias = "folder")]
        parent: Option<String>,
        /// Drive ID or slug. Use this when you need to disambiguate (e.g.
        /// the same folder name exists in multiple drives) or when only
        /// creating at the drive root without a `--parent`.
        #[arg(long)]
        drive: Option<String>,
    },
    /// Delete a folder by ID. Children of the folder are not auto-removed —
    /// move or delete them first or they will be left orphaned in the tree.
    Delete {
        /// Folder ID
        id: String,
        /// Drive ID or slug containing the folder
        #[arg(long)]
        drive: String,
        /// Skip confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

pub async fn run(
    cmd: FoldersCommand,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    match cmd {
        FoldersCommand::Create {
            name,
            parent,
            drive,
        } => {
            create(
                &name,
                parent.as_deref(),
                drive.as_deref(),
                format,
                profile_name,
            )
            .await
        }
        FoldersCommand::Delete { id, drive, yes } => {
            delete(&id, &drive, yes, format, profile_name).await
        }
    }
}

/// Resolved parent target: which drive to add to, and (optionally) which
/// folder to nest under.
struct ParentTarget {
    drive_id: String,
    parent_folder_id: Option<String>,
}

async fn create(
    name: &str,
    parent: Option<&str>,
    drive: Option<&str>,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_pname, _profile, client) = helpers::setup(profile_name)?;
    let target = resolve_parent(&client, parent, drive).await?;
    let folder_id = Uuid::new_v4().to_string();

    let mut input = serde_json::json!({
        "id": folder_id,
        "name": name,
    });
    if let Some(ref p) = target.parent_folder_id {
        input["parentFolder"] = serde_json::json!(p);
    }

    let nested = helpers::is_nested_api(&client).await;
    let mutation = if nested {
        "mutation($docId: PHID!, $input: DocumentDrive_AddFolderInput!) { \
         DocumentDrive { addFolder(docId: $docId, input: $input) { id } } }"
    } else {
        "mutation($docId: PHID!, $input: DocumentDrive_AddFolderInput!) { \
         DocumentDrive_addFolder(docId: $docId, input: $input) { id } }"
    };
    let vars = serde_json::json!({ "docId": target.drive_id, "input": input });
    client.query(mutation, Some(&vars)).await?;

    let result = serde_json::json!({
        "id": folder_id,
        "name": name,
        "parentFolder": target.parent_folder_id,
        "drive": target.drive_id,
    });

    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&result),
        _ => {
            println!("{} Folder created", "✓".green());
            println!("  ID:     {folder_id}");
            println!("  Name:   {name}");
            match &target.parent_folder_id {
                Some(id) => println!("  Parent: {id} (folder)"),
                None => println!("  Parent: (drive root)"),
            }
            println!("  Drive:  {}", target.drive_id);
        }
    }

    Ok(())
}

/// Resolve a `--parent`/`--drive` pair into a drive_id + optional folder_id.
///
/// Rules:
/// - At least one of `--parent` or `--drive` must be given.
/// - `--parent` is universal: it can be a drive (then we place at root) or
///   a folder (then we nest under it). Drive resolution is tried first.
/// - When `--parent` is a folder, we search drives to find which one owns
///   it. If `--drive` was also passed, we use that drive as a constraint
///   instead of scanning everything.
/// - When `--drive` is given without `--parent`, the new folder lands at
///   the drive root.
async fn resolve_parent(
    client: &crate::graphql::GraphQLClient,
    parent: Option<&str>,
    drive: Option<&str>,
) -> Result<ParentTarget> {
    match (parent, drive) {
        (None, None) => bail!("Pass either --parent or --drive (--parent accepts both)"),

        // Only --drive: at the root of that drive.
        (None, Some(d)) => {
            let drive_id = helpers::resolve_doc(client, d).await?;
            Ok(ParentTarget {
                drive_id,
                parent_folder_id: None,
            })
        }

        // Only --parent: try drive first, fall back to folder lookup across drives.
        (Some(p), None) => {
            if let Ok(drive_id) = helpers::resolve_doc(client, p).await
                && is_drive(client, &drive_id).await
            {
                return Ok(ParentTarget {
                    drive_id,
                    parent_folder_id: None,
                });
            }
            // Not a drive — search all drives for a folder matching this id/name.
            let (drive_id, folder_id) = find_folder_across_drives(client, p, None).await?;
            Ok(ParentTarget {
                drive_id,
                parent_folder_id: Some(folder_id),
            })
        }

        // Both given: --drive scopes the search, --parent must resolve to a folder
        // within that drive (or be that drive itself, in which case we treat as root).
        (Some(p), Some(d)) => {
            let drive_id = helpers::resolve_doc(client, d).await?;
            // If --parent matches the drive itself, treat as root.
            if helpers::is_uuid(p) && p == drive_id {
                return Ok(ParentTarget {
                    drive_id,
                    parent_folder_id: None,
                });
            }
            let folder_id = resolve_folder_id_in_drive(client, &drive_id, p).await?;
            Ok(ParentTarget {
                drive_id,
                parent_folder_id: Some(folder_id),
            })
        }
    }
}

/// Returns true if the given UUID identifies a drive document.
async fn is_drive(client: &crate::graphql::GraphQLClient, doc_id: &str) -> bool {
    let escaped = doc_id.replace('"', r#"\""#);
    let query =
        format!(r#"{{ document(identifier: "{escaped}") {{ document {{ documentType }} }} }}"#);
    client
        .query(&query, None)
        .await
        .ok()
        .and_then(|d| {
            d.pointer("/document/document/documentType")
                .and_then(|v| v.as_str())
                .map(|t| t == "powerhouse/document-drive")
        })
        .unwrap_or(false)
}

/// Resolve a parent folder identifier within a specific drive. Accepts either
/// a UUID (used as-is, validated to exist in the drive) or a folder name
/// (looked up against the drive's nodes). Errors if missing or ambiguous.
async fn resolve_folder_id_in_drive(
    client: &crate::graphql::GraphQLClient,
    drive_id: &str,
    identifier: &str,
) -> Result<String> {
    let escaped = drive_id.replace('"', r#"\""#);
    let query = format!(r#"{{ document(identifier: "{escaped}") {{ document {{ state }} }} }}"#);
    let data = client.query(&query, None).await?;
    let nodes = data
        .pointer("/document/document/state/global/nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("Drive '{drive_id}' has no nodes to search"))?;

    if helpers::is_uuid(identifier) {
        let exists = nodes
            .iter()
            .any(|n| n["kind"].as_str() == Some("folder") && n["id"].as_str() == Some(identifier));
        if !exists {
            bail!("Folder '{identifier}' not found in drive {drive_id}");
        }
        return Ok(identifier.to_string());
    }

    let matches: Vec<&str> = nodes
        .iter()
        .filter(|n| n["kind"].as_str() == Some("folder") && n["name"].as_str() == Some(identifier))
        .filter_map(|n| n["id"].as_str())
        .collect();

    match matches.len() {
        0 => bail!("No folder named '{identifier}' found in drive {drive_id}"),
        1 => Ok(matches[0].to_string()),
        n => bail!(
            "Found {n} folders named '{identifier}' in drive {drive_id} — pass the UUID instead"
        ),
    }
}

/// Search every drive for a folder matching `identifier` (UUID or name).
/// Returns `(drive_id, folder_id)`. If `drive_filter` is set, only that
/// drive is searched. Errors on ambiguity.
async fn find_folder_across_drives(
    client: &crate::graphql::GraphQLClient,
    identifier: &str,
    drive_filter: Option<&str>,
) -> Result<(String, String)> {
    let drives_query = r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id state } } }"#;
    let data = client.query(drives_query, None).await?;
    let drives = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut matches: Vec<(String, String)> = Vec::new();
    for d in drives.iter() {
        let did = d["id"].as_str().unwrap_or("").to_string();
        if let Some(filter) = drive_filter
            && did != filter
        {
            continue;
        }
        let nodes = match d.pointer("/state/global/nodes").and_then(|v| v.as_array()) {
            Some(n) => n,
            None => continue,
        };
        for n in nodes {
            if n["kind"].as_str() != Some("folder") {
                continue;
            }
            let id = n["id"].as_str().unwrap_or("");
            let name = n["name"].as_str().unwrap_or("");
            let id_match = helpers::is_uuid(identifier) && id == identifier;
            let name_match = !helpers::is_uuid(identifier) && name == identifier;
            if id_match || name_match {
                matches.push((did.clone(), id.to_string()));
            }
        }
    }

    match matches.len() {
        0 => bail!("No folder named '{identifier}' found in any drive"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => bail!(
            "Found {n} folders matching '{identifier}' across drives — disambiguate with --drive <slug>"
        ),
    }
}

async fn delete(
    id: &str,
    drive: &str,
    skip_confirm: bool,
    format: OutputFormat,
    profile_name: Option<&str>,
) -> Result<()> {
    let (_pname, _profile, client) = helpers::setup(profile_name)?;
    let drive_id = helpers::resolve_doc(&client, drive).await?;

    if !skip_confirm {
        let confirm = dialoguer::Confirm::new()
            .with_prompt(format!("Delete folder {id}?"))
            .default(false)
            .interact()?;
        if !confirm {
            println!("Aborted.");
            return Ok(());
        }
    }

    let nested = helpers::is_nested_api(&client).await;
    let mutation = if nested {
        "mutation($docId: PHID!, $input: DocumentDrive_DeleteNodeInput!) { \
         DocumentDrive { deleteNode(docId: $docId, input: $input) { id } } }"
    } else {
        "mutation($docId: PHID!, $input: DocumentDrive_DeleteNodeInput!) { \
         DocumentDrive_deleteNode(docId: $docId, input: $input) { id } }"
    };
    let vars = serde_json::json!({ "docId": drive_id, "input": { "id": id } });
    if let Err(e) = client.query(mutation, Some(&vars)).await {
        bail!("Failed to delete folder {id}: {e}");
    }

    let result = serde_json::json!({ "deleted": id, "drive": drive_id });
    match format {
        OutputFormat::Json | OutputFormat::Raw => print_json(&result),
        _ => println!("{} Deleted folder {id}", "✓".green()),
    }
    Ok(())
}
