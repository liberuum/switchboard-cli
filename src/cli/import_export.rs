use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use colored::Colorize;
use serde_json::Value;
use std::path::Path;

use crate::cli::helpers;
use crate::graphql::GraphQLClient;
use crate::output::OutputFormat;
use crate::phd::{self, PhdHeader, PhdOperations, PhdState};

/// Shared operation filter options for export commands.
#[derive(Args, Clone, Default)]
pub struct OpFilterArgs {
    /// Only include operations with these action types (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub action_types: Option<Vec<String>>,
    /// Only include operations since this revision index
    #[arg(long)]
    pub since_revision: Option<u64>,
    /// Only include operations from this timestamp (ISO-8601)
    #[arg(long)]
    pub from: Option<String>,
    /// Only include operations up to this timestamp (ISO-8601)
    #[arg(long)]
    pub to: Option<String>,
}

impl OpFilterArgs {
    fn has_filters(&self) -> bool {
        self.action_types.is_some()
            || self.since_revision.is_some()
            || self.from.is_some()
            || self.to.is_some()
    }

    fn build_filter_clause(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref types) = self.action_types {
            let quoted: Vec<String> = types.iter().map(|t| format!("\"{t}\"")).collect();
            parts.push(format!("actionTypes: [{}]", quoted.join(", ")));
        }
        if let Some(rev) = self.since_revision {
            parts.push(format!("sinceRevision: {rev}"));
        }
        if let Some(ref from) = self.from {
            parts.push(format!("timestampFrom: \"{from}\""));
        }
        if let Some(ref to) = self.to {
            parts.push(format!("timestampTo: \"{to}\""));
        }
        parts.join(", ")
    }
}

#[derive(Subcommand)]
pub enum ExportCommand {
    /// Export everything: all drives and their documents
    All {
        /// Output directory (defaults to ./switchboard-export/)
        #[arg(long, short)]
        out: Option<String>,
        /// Include operation history (for CLI archival/roundtrips via switchboard import)
        #[arg(long)]
        with_ops: bool,
        #[command(flatten)]
        filter: OpFilterArgs,
    },
    /// Export a single document as .phd file
    Doc {
        /// Document ID
        doc_id: String,
        /// Drive ID or slug
        #[arg(long)]
        drive: String,
        /// Output file path (defaults to <name>.phd)
        #[arg(long, short)]
        out: Option<String>,
        /// Include operation history (for CLI archival/roundtrips via switchboard import)
        #[arg(long)]
        with_ops: bool,
        #[command(flatten)]
        filter: OpFilterArgs,
    },
    /// Export all documents in a drive as .phd files
    Drive {
        /// Drive ID or slug
        drive: String,
        /// Output directory (defaults to ./<drive-name>/)
        #[arg(long, short)]
        out: Option<String>,
        /// Include operation history (for CLI archival/roundtrips via switchboard import)
        #[arg(long)]
        with_ops: bool,
        #[command(flatten)]
        filter: OpFilterArgs,
    },
}

pub async fn run_export(
    cmd: ExportCommand,
    _format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    match cmd {
        ExportCommand::All {
            out,
            with_ops,
            filter,
        } => export_all(out.as_deref(), with_ops, &filter, profile_name, quiet).await,
        ExportCommand::Doc {
            doc_id,
            drive,
            out,
            with_ops,
            filter,
        } => {
            export_doc(
                &doc_id,
                &drive,
                out.as_deref(),
                with_ops,
                &filter,
                profile_name,
                quiet,
            )
            .await
        }
        ExportCommand::Drive {
            drive,
            out,
            with_ops,
            filter,
        } => {
            export_drive(
                &drive,
                out.as_deref(),
                with_ops,
                &filter,
                profile_name,
                quiet,
            )
            .await
        }
    }
}

/// Build the proper PhdHeader matching the reference download-drive-documents.ts format.
/// Accepts operations to extract protocolVersions from the CREATE_DOCUMENT op.
fn build_header(doc: &Value, operations: &[Value]) -> PhdHeader {
    let doc_id = doc["id"].as_str().unwrap_or("").to_string();
    let doc_name = doc["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("document")
        .to_string();
    let doc_type = doc["documentType"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    // Build revision from revisionsList if available
    let revision = if let Some(arr) = doc["revisionsList"].as_array() {
        let mut rev_map = serde_json::Map::new();
        for entry in arr {
            if let (Some(scope), Some(rev)) = (entry["scope"].as_str(), entry["revision"].as_u64())
            {
                rev_map.insert(scope.to_string(), serde_json::json!(rev));
            }
        }
        if rev_map.is_empty() {
            serde_json::json!({ "global": 0 })
        } else {
            Value::Object(rev_map)
        }
    } else {
        serde_json::json!({ "global": 0 })
    };

    // Extract protocolVersions from the CREATE_DOCUMENT operation if present
    let protocol_versions = operations.iter().find_map(|op| {
        let action = op.get("action")?;
        if action.get("type")?.as_str()? == "CREATE_DOCUMENT" {
            action
                .get("input")
                .and_then(|i| i.get("protocolVersions"))
                .cloned()
        } else {
            None
        }
    });

    PhdHeader {
        id: doc_id.clone(),
        sig: serde_json::json!({ "publicKey": {}, "nonce": "" }),
        document_type: doc_type,
        created_at_utc_iso: doc["createdAtUtcIso"].as_str().map(|s| s.to_string()),
        slug: Some(doc_id),
        name: doc_name,
        branch: "main".to_string(),
        revision,
        last_modified_at_utc_iso: doc["lastModifiedAtUtcIso"].as_str().map(|s| s.to_string()),
        meta: Value::Object(serde_json::Map::new()),
        protocol_versions,
    }
}

/// Build PhdState from the API's state object.
/// The API returns { auth, document, global, local } which maps directly to PhdState fields.
fn build_current_state(state: &Value) -> PhdState {
    PhdState {
        auth: state
            .get("auth")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new())),
        document: state.get("document").cloned().unwrap_or_else(|| {
            serde_json::json!({ "version": 0, "hash": { "algorithm": "sha1", "encoding": "base64" } })
        }),
        global: state
            .get("global")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new())),
        local: state
            .get("local")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new())),
    }
}

async fn export_all(
    out_dir: Option<&str>,
    with_ops: bool,
    filter: &OpFilterArgs,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, _cache) = helpers::setup_with_cache(profile_name)?;

    // List all drives, filtering out soft-deleted ones
    let data = client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug state } totalCount } }"#,
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

    if drives.is_empty() {
        if !quiet {
            println!("No drives found.");
        }
        return Ok(());
    }

    let base_dir = out_dir.unwrap_or("./switchboard-export");
    let base_path = Path::new(base_dir);
    std::fs::create_dir_all(base_path)?;
    // Resolve to absolute path so the user sees exactly where files land
    let base_path = std::fs::canonicalize(base_path)?;

    if !quiet {
        println!(
            "Exporting {} drive(s) to {}/",
            drives.len(),
            base_path.display()
        );
    }

    let mut total_docs = 0;

    for (drive_idx, drive) in drives.iter().enumerate() {
        let drive_id = drive["id"].as_str().unwrap_or("");
        let drive_name = drive["name"].as_str().unwrap_or("drive");
        let drive_slug = drive["slug"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(drive_name);

        if !quiet {
            println!(
                "\n[{}/{}] Drive: {} ({})",
                drive_idx + 1,
                drives.len(),
                drive_name,
                drive_slug,
            );
        }

        // Get nodes for this drive via document() query
        let nodes = match fetch_drive_nodes(&client, drive_id).await {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  Failed to query drive {drive_slug}: {e}");
                continue;
            }
        };

        let files: Vec<&Value> = nodes
            .iter()
            .filter(|n| n["kind"].as_str() == Some("file"))
            .collect();

        if files.is_empty() {
            if !quiet {
                println!("  No documents, skipping.");
            }
            continue;
        }

        // Build folder lookup: id -> (name, parentFolder)
        let folder_map: std::collections::HashMap<&str, (&str, &str)> = nodes
            .iter()
            .filter(|n| n["kind"].as_str() == Some("folder"))
            .filter_map(|n| {
                let id = n["id"].as_str()?;
                let name = n["name"].as_str()?;
                let parent = n["parentFolder"].as_str().unwrap_or("");
                Some((id, (name, parent)))
            })
            .collect();

        // Build full relative path for a folder id by walking up the parent chain
        fn folder_path(
            id: &str,
            map: &std::collections::HashMap<&str, (&str, &str)>,
        ) -> std::path::PathBuf {
            let mut parts = vec![];
            let mut current = id;
            while let Some(&(name, parent)) = map.get(current) {
                parts.push(sanitize_filename(name));
                current = parent;
                if current.is_empty() {
                    break;
                }
            }
            parts.reverse();
            parts.iter().collect()
        }

        let drive_dir = base_path.join(sanitize_filename(drive_slug));
        std::fs::create_dir_all(&drive_dir)?;

        for (i, file_node) in files.iter().enumerate() {
            let file_id = file_node["id"].as_str().unwrap_or("");
            let file_name = file_node["name"].as_str().unwrap_or("document");
            let file_type = file_node["documentType"].as_str().unwrap_or("unknown");

            // Determine folder path for this file (supports arbitrary nesting depth)
            let mut file_dir = drive_dir.clone();
            if let Some(parent_id) = file_node["parentFolder"].as_str()
                && !parent_id.is_empty()
                && folder_map.contains_key(parent_id)
            {
                let rel = folder_path(parent_id, &folder_map);
                let folder_dir = drive_dir.join(rel);
                std::fs::create_dir_all(&folder_dir)?;
                file_dir = folder_dir;
            }

            match fetch_document(&client, file_id, filter).await {
                Ok((doc, operations)) => {
                    let header = build_header(&doc, &operations);
                    let state = extract_state(&doc);
                    let phd_ops = if with_ops {
                        split_ops_by_scope(&operations)
                    } else {
                        empty_ops()
                    };
                    let current_state = build_current_state(&state);
                    let initial_state = extract_initial_state(&operations, &current_state);

                    let safe_file = sanitize_filename(file_name);
                    let file_path = file_dir.join(format!("{safe_file}.phd"));

                    match phd::write_phd(
                        &file_path,
                        &header,
                        &initial_state,
                        &current_state,
                        &phd_ops,
                    ) {
                        Ok(()) => {
                            if !quiet {
                                let size =
                                    std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
                                println!(
                                    "  [{}/{}] {} ({}) → {} {}",
                                    i + 1,
                                    files.len(),
                                    file_name,
                                    file_type,
                                    format_bytes(size),
                                    "✓".green()
                                );
                            }
                            total_docs += 1;
                        }
                        Err(e) => {
                            println!(
                                "  [{}/{}] {} → {} {e}",
                                i + 1,
                                files.len(),
                                file_name,
                                "✗".red()
                            );
                        }
                    }
                }
                Err(e) => {
                    println!(
                        "  [{}/{}] {} → {} {e}",
                        i + 1,
                        files.len(),
                        file_name,
                        "✗".red()
                    );
                }
            }
        }
    }

    if !quiet {
        println!(
            "\n{} {total_docs} documents exported across {} drive(s) to {}/",
            "✓".green(),
            drives.len(),
            base_path.display()
        );
    }
    Ok(())
}

async fn export_doc(
    doc_id: &str,
    drive: &str,
    out_path: Option<&str>,
    with_ops: bool,
    filter: &OpFilterArgs,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, _cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve document: if drive is provided, use "drive/doc_id" format
    let identifier = format!("{drive}/{doc_id}");
    let resolved_id = helpers::resolve_doc(&client, &identifier).await?;

    let (doc, operations) = fetch_document(&client, &resolved_id, filter).await?;

    let header = build_header(&doc, &operations);
    let state = extract_state(&doc);
    let phd_ops = if with_ops {
        split_ops_by_scope(&operations)
    } else {
        empty_ops()
    };
    let current_state = build_current_state(&state);
    let initial_state = extract_initial_state(&operations, &current_state);

    // Determine output path
    let safe_name = sanitize_filename(&header.name);
    let default_path = format!("{safe_name}.phd");
    let out = out_path.unwrap_or(&default_path);
    let path = Path::new(out);

    phd::write_phd(path, &header, &initial_state, &current_state, &phd_ops)?;

    if !quiet {
        let abs_path = std::fs::canonicalize(path)?;
        let file_size = std::fs::metadata(&abs_path)?.len();
        println!(
            "{} Saved {} ({}, {} ops, {})",
            "✓".green(),
            abs_path.display(),
            header.document_type,
            operations.len(),
            format_bytes(file_size),
        );
    }

    Ok(())
}

async fn export_drive(
    drive: &str,
    out_dir: Option<&str>,
    with_ops: bool,
    filter: &OpFilterArgs,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, _cache) = helpers::setup_with_cache(profile_name)?;

    // Get drive info and node tree via document() query
    let nodes = fetch_drive_nodes(&client, drive).await?;

    // Also get the drive name
    let escaped = drive.replace('"', r#"\""#);
    let name_query =
        format!(r#"{{ document(identifier: "{escaped}") {{ document {{ name }} }} }}"#);
    let name_data = client.query(&name_query, None).await?;
    let drive_name = name_data
        .pointer("/document/document/name")
        .and_then(|v| v.as_str())
        .unwrap_or(drive);

    let files: Vec<&Value> = nodes
        .iter()
        .filter(|n| n["kind"].as_str() == Some("file"))
        .collect();

    if files.is_empty() {
        if !quiet {
            println!("No documents found in drive '{drive}'.");
        }
        return Ok(());
    }

    if !quiet {
        let folders = nodes
            .iter()
            .filter(|n| n["kind"].as_str() == Some("folder"))
            .count();
        println!(
            "  Name: {} ({} files, {} folders)",
            drive_name,
            files.len(),
            folders
        );
    }

    // Create output directory
    let safe_name = sanitize_filename(drive_name);
    let default_dir = format!("./{safe_name}");
    let dir_str = out_dir.unwrap_or(&default_dir);
    let dir = Path::new(dir_str);
    std::fs::create_dir_all(dir)?;
    let dir = std::fs::canonicalize(dir)?;

    if !quiet {
        println!("  Saving to {}/", dir.display());
        println!("  Downloading {} documents...", files.len());
    }

    let mut success = 0;
    let total = files.len();

    // Build folder ID → path map for preserving directory structure
    let folder_path_map = build_folder_paths(&nodes);

    for (i, file_node) in files.iter().enumerate() {
        let file_id = file_node["id"].as_str().unwrap_or("");
        let file_name = file_node["name"].as_str().unwrap_or("document");
        let file_type = file_node["documentType"].as_str().unwrap_or("unknown");

        match fetch_document(&client, file_id, filter).await {
            Ok((doc, operations)) => {
                let state = extract_state(&doc);

                let header = build_header(&doc, &operations);
                let phd_ops = if with_ops {
                    split_ops_by_scope(&operations)
                } else {
                    empty_ops()
                };
                let current_state = build_current_state(&state);
                let initial_state = extract_initial_state(&operations, &current_state);

                // Resolve the folder path for this file
                let safe_file = sanitize_filename(file_name);
                let sub_dir = file_node["parentFolder"]
                    .as_str()
                    .and_then(|pf| folder_path_map.get(pf))
                    .cloned()
                    .unwrap_or_default();
                let target_dir = if sub_dir.is_empty() {
                    dir.clone()
                } else {
                    let d = dir.join(&sub_dir);
                    std::fs::create_dir_all(&d).ok();
                    d
                };
                let file_path = target_dir.join(format!("{safe_file}.phd"));

                match phd::write_phd(
                    &file_path,
                    &header,
                    &initial_state,
                    &current_state,
                    &phd_ops,
                ) {
                    Ok(()) => {
                        if !quiet {
                            let size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
                            println!(
                                "  [{}/{}] {} ({}) → {} {}",
                                i + 1,
                                total,
                                file_name,
                                file_type,
                                format_bytes(size),
                                "✓".green()
                            );
                        }
                        success += 1;
                    }
                    Err(e) => {
                        println!("  [{}/{}] {} → {} {e}", i + 1, total, file_name, "✗".red());
                    }
                }
            }
            Err(e) => {
                println!("  [{}/{}] {} → {} {e}", i + 1, total, file_name, "✗".red());
            }
        }
    }

    if !quiet {
        println!(
            "{} {success} documents saved to {}/",
            "✓".green(),
            dir.display()
        );
    }
    Ok(())
}

/// Build a map of folder ID → relative filesystem path by walking the node tree.
/// e.g., folder "notes" inside "knowledge" at root → "knowledge/notes"
fn build_folder_paths(nodes: &[Value]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;

    // Build folder ID → (name, parentFolder) map
    let mut folder_info: HashMap<String, (String, Option<String>)> = HashMap::new();
    for node in nodes {
        if node["kind"].as_str() == Some("folder") {
            let id = node["id"].as_str().unwrap_or("").to_string();
            let name = node["name"].as_str().unwrap_or("").to_string();
            let parent = node["parentFolder"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            folder_info.insert(id, (name, parent));
        }
    }

    // Resolve each folder's full path by walking up the parent chain
    let mut result: HashMap<String, String> = HashMap::new();
    for id in folder_info.keys().cloned().collect::<Vec<_>>() {
        let mut parts = Vec::new();
        let mut current = Some(id.clone());
        // Walk up (with cycle guard)
        let mut depth = 0;
        while let Some(ref cur_id) = current {
            if depth > 20 {
                break;
            }
            if let Some((name, parent)) = folder_info.get(cur_id) {
                parts.push(sanitize_filename(name));
                current = parent.clone();
            } else {
                break;
            }
            depth += 1;
        }
        parts.reverse();
        result.insert(id, parts.join("/"));
    }
    result
}

const OP_BATCH_SIZE: usize = 500;
/// Delay between write operations (import only) to avoid overwhelming the server.
const WRITE_DELAY_MS: u64 = 100;

/// Fetch drive nodes via the document() query on the main GraphQL endpoint.
async fn fetch_drive_nodes(client: &GraphQLClient, drive_identifier: &str) -> Result<Vec<Value>> {
    let escaped = drive_identifier.replace('"', r#"\""#);
    let query = format!(r#"{{ document(identifier: "{escaped}") {{ document {{ state }} }} }}"#,);
    let data = client.query(&query, None).await?;
    // Nodes live at state.global.nodes in the unified document API
    Ok(data
        .pointer("/document/document/state/global/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Fetch a document's full data (metadata + state + operations) via the main GraphQL endpoint.
/// Uses document() for metadata/state and documentOperations() for ops with pagination.
async fn fetch_document(
    client: &GraphQLClient,
    doc_id: &str,
    filter: &OpFilterArgs,
) -> Result<(Value, Vec<Value>)> {
    let escaped = doc_id.replace('"', r#"\""#);

    // Fetch document metadata and state
    let doc_query = format!(
        r#"{{ document(identifier: "{escaped}") {{ document {{ id name documentType state revisionsList {{ scope revision }} createdAtUtcIso lastModifiedAtUtcIso }} }} }}"#,
    );
    let doc_data = client.query(&doc_query, None).await?;
    let doc = doc_data
        .pointer("/document/document")
        .filter(|v| !v.is_null())
        .ok_or_else(|| anyhow::anyhow!("Document '{doc_id}' not found"))?
        .clone();

    // Build operation filter clause
    let extra_filter = if filter.has_filters() {
        format!(", {}", filter.build_filter_clause())
    } else {
        String::new()
    };

    // Fetch operations with pagination (no delay — reads are safe to do at full speed)
    let mut all_ops: Vec<Value> = Vec::new();
    let mut total_count: Option<usize> = None;
    loop {
        let offset = all_ops.len();
        let ops_query = format!(
            r#"{{ documentOperations(filter: {{ documentId: "{escaped}"{extra_filter} }}, paging: {{ limit: {OP_BATCH_SIZE}, offset: {offset} }}) {{ items {{ id index action {{ id type input scope timestampUtcMs attachments {{ data mimeType hash extension fileName }} context {{ signer {{ user {{ address networkId chainId }} app {{ name key }} signatures }} }} }} timestampUtcMs hash skip error }} totalCount }} }}"#,
        );

        let ops_data = client.query(&ops_query, None).await?;

        // Capture totalCount on first batch to avoid an extra empty-fetch round trip
        if total_count.is_none() {
            total_count = ops_data
                .pointer("/documentOperations/totalCount")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
        }

        let batch = ops_data
            .pointer("/documentOperations/items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let batch_len = batch.len();
        all_ops.extend(batch);

        // Stop when we've collected everything or got a short page
        if batch_len < OP_BATCH_SIZE {
            break;
        }
        if let Some(total) = total_count
            && all_ops.len() >= total
        {
            break;
        }
    }

    // Sanity check: if the document's non-document-scope revisions indicate
    // history exists but `documentOperations` came back empty, surface the
    // discrepancy instead of writing a silently-empty operations.json. This
    // is the symptom in SWITCHBOARD_CLI_BUGS.md Bug 3 (revision > 0 but
    // exported ops empty) — knowing about it is the first step.
    if all_ops.is_empty() && !filter.has_filters() {
        let non_doc_revs = doc
            .pointer("/revisionsList")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|r| r["scope"].as_str() != Some("document"))
                    .filter_map(|r| r["revision"].as_u64())
                    .sum::<u64>()
            })
            .unwrap_or(0);
        if non_doc_revs > 0 {
            eprintln!(
                "  {} document '{}' reports {} non-document revision(s) but \
                 documentOperations returned 0 — exported operations.json will be empty",
                "⚠".yellow(),
                doc["name"].as_str().unwrap_or(doc_id),
                non_doc_revs,
            );
        }
    }

    // Strip context.signer from exported operations. Signatures are bound to
    // the original document ID — when imported into a new document, the reactor
    // would reject them with "signature verification returned false". Stripping
    // the signer lets the importing reactor re-sign with its own key.
    let cleaned_ops: Vec<Value> = all_ops
        .into_iter()
        .map(|mut op| {
            if let Some(action) = op.get_mut("action")
                && let Some(ctx) = action.get_mut("context")
                && let Some(obj) = ctx.as_object_mut()
            {
                obj.remove("signer");
            }
            op
        })
        .collect();

    Ok((doc, cleaned_ops))
}

/// Extract state from a document value. In the new API, state is a JSONObject directly.
/// Empty operations — matches Connect's export format (`operations.json: {}`)
fn empty_ops() -> PhdOperations {
    PhdOperations::default()
}

/// Split operations by scope for the .phd format.
/// Groups operations into their actual scope (global, document, local, custom, etc.)
/// rather than lumping non-document ops into global.
fn split_ops_by_scope(operations: &[Value]) -> PhdOperations {
    let mut map = std::collections::HashMap::<String, Vec<Value>>::new();
    for op in operations {
        let scope = op
            .pointer("/action/scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        map.entry(scope.to_string()).or_default().push(op.clone());
    }
    PhdOperations(map)
}

fn extract_state(doc: &Value) -> Value {
    doc.get("state")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()))
}

/// Extract the initial state from the UPGRADE_DOCUMENT operation in document-scope ops.
/// Looks for `action.input.initialState` first, then `action.input.state` as fallback.
/// If no UPGRADE_DOCUMENT op is found, falls back to cloning the current state.
fn extract_initial_state(operations: &[Value], current_state: &PhdState) -> PhdState {
    for op in operations {
        let action = match op.get("action") {
            Some(a) => a,
            None => continue,
        };
        if action.pointer("/scope").and_then(|v| v.as_str()) != Some("document") {
            continue;
        }
        if action.pointer("/type").and_then(|v| v.as_str()) != Some("UPGRADE_DOCUMENT") {
            continue;
        }
        let input = match action.get("input") {
            Some(i) => i,
            None => continue,
        };
        // Prefer initialState, fall back to state
        if let Some(state_val) = input.get("initialState").or_else(|| input.get("state")) {
            return build_current_state(state_val);
        }
    }
    current_state.clone()
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// --- Import ---

pub async fn run_import(
    files: Vec<String>,
    drive: String,
    strict: bool,
    id_mapping_path: Option<String>,
    _format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (pname, _profile, client, mut cache) = helpers::setup_with_cache(profile_name)?;
    let mut introspected = false;

    if files.is_empty() {
        bail!("No .phd files specified");
    }

    // Resolve the drive identifier
    let drive_id = helpers::resolve_doc(&client, &drive).await?;

    // Old → new document UUID map. Seeded from `--id-mapping <file>` if
    // provided, then extended automatically as documents are created so that
    // ops referencing earlier docs by their original UUID get rewritten
    // before being dispatched.
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(path) = id_mapping_path.as_deref() {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read --id-mapping file '{path}': {e}"))?;
        let parsed: std::collections::HashMap<String, String> = serde_json::from_str(&raw)
            .map_err(|e| {
                anyhow::anyhow!(
                    "--id-mapping file '{path}' is not a valid JSON object of old→new strings: {e}"
                )
            })?;
        if !quiet {
            eprintln!(
                "  {} Loaded {} entries from --id-mapping",
                "ℹ".cyan(),
                parsed.len()
            );
        }
        id_map.extend(parsed);
    }

    if !quiet {
        println!(
            "  Importing {} file(s) into drive '{drive}'{}...",
            files.len(),
            if strict { " (strict mode)" } else { "" }
        );
    }

    let mut success = 0;
    let mut total_ops_attempted = 0;
    let mut total_ops_failed = 0;

    for file_str in &files {
        let path = Path::new(file_str);
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file_str);

        if !quiet {
            println!("\n  ── {} ──", filename);
        }

        let contents = match phd::read_phd(path) {
            Ok(c) => c,
            Err(e) => {
                println!("  {} Failed to read: {e}", "✗".red());
                continue;
            }
        };

        let doc_type = &contents.header.document_type;
        let doc_name = &contents.header.name;
        let ops_count = contents.operations.domain_ops_count();

        if !quiet {
            println!("  Type: {doc_type}");
            println!("  Name: {doc_name}");
            println!("  Ops:  {ops_count}");
        }

        // Find the matching model. If the type is unknown, re-introspect once
        // (handles fresh profiles or reactor restarts that loaded new packages)
        // before giving up — same behavior as `docs create` and `docs mutate`.
        if cache.find_model(doc_type).is_none() && !introspected {
            if !quiet {
                eprintln!(
                    "  {} Type '{doc_type}' not in cache — re-introspecting...",
                    "ℹ".cyan()
                );
            }
            match crate::graphql::introspection::run_introspection(&client).await {
                Ok(new_cache) => {
                    let _ = crate::graphql::introspection::save_cache(&pname, &new_cache);
                    cache = new_cache;
                }
                Err(e) => {
                    eprintln!("  {} Re-introspection failed: {e}", "⚠".yellow());
                }
            }
            introspected = true;
        }
        let model = match cache.find_model(doc_type) {
            Some(m) => m,
            None => {
                println!(
                    "  {} No matching model found for type '{doc_type}' \
                     (try `switchboard introspect` to refresh the schema cache)",
                    "✗".red()
                );
                continue;
            }
        };

        // Step 1: Create the document via model-specific mutation
        let vars = serde_json::json!({
            "name": doc_name,
            "parentIdentifier": drive_id,
        });
        let mutation = if model.namespace.is_empty() {
            format!(
                "mutation($name: String!, $parentIdentifier: String) {{ {}(name: $name, parentIdentifier: $parentIdentifier) {{ id }} }}",
                model.create_mutation,
            )
        } else {
            format!(
                "mutation($name: String!, $parentIdentifier: String) {{ {} {{ createDocument(name: $name, parentIdentifier: $parentIdentifier) {{ id }} }} }}",
                model.namespace,
            )
        };

        let data = match client.query(&mutation, Some(&vars)).await {
            Ok(d) => d,
            Err(e) => {
                println!("  {} Failed to create document: {e}", "✗".red());
                continue;
            }
        };

        let create_result = if model.namespace.is_empty() {
            data.get(&model.create_mutation)
        } else {
            data.get(&model.namespace)
                .and_then(|ns| ns.get("createDocument"))
        };
        let new_doc_id = match create_result.and_then(|v| {
            v.as_str()
                .or_else(|| v.get("id").and_then(|id| id.as_str()))
        }) {
            Some(id) => {
                if !quiet {
                    println!("  Created: {id}");
                }
                id.to_string()
            }
            None => {
                println!("  {} Created but no document ID returned", "✗".red());
                continue;
            }
        };

        // Record the old → new ID mapping so subsequent ops within this
        // batch can rewrite cross-document references on the fly.
        if !contents.header.id.is_empty() {
            id_map.insert(contents.header.id.clone(), new_doc_id.clone());
        }

        // Step 2: Push operations via model-specific mutations
        let mut stats = OpStats::default();
        if ops_count > 0 {
            match push_operations_via_mutate(
                &client,
                &new_doc_id,
                &contents.operations,
                model,
                &id_map,
                quiet,
            )
            .await
            {
                Ok(s) => stats = s,
                Err(e) => {
                    println!("  {} Failed to push operations: {e}", "✗".red());
                    continue;
                }
            }
            if !quiet {
                if stats.failed == 0 {
                    println!("  Pushed: {} operations", stats.succeeded);
                } else {
                    println!(
                        "  Pushed: {}/{} operations ({} failed)",
                        stats.succeeded, stats.attempted, stats.failed
                    );
                }
            }
        } else if !quiet {
            println!("  No operations to push");
        }

        total_ops_attempted += stats.attempted;
        total_ops_failed += stats.failed;

        // Step 3: Verify state matches the .phd current-state.
        //
        // The verdict is qualified by whether ops actually applied: if any
        // op was rejected, we never claim "EXACT MATCH" even if the JSON
        // happens to match (it might match because both sides are empty —
        // which is the silent-corruption mode the bug report calls out).
        tokio::time::sleep(std::time::Duration::from_millis(WRITE_DELAY_MS)).await;
        let state_match = verify_state(&client, &new_doc_id, &contents.current_state.global).await;
        if !quiet {
            match (&state_match, stats.failed) {
                (Ok(true), 0) => println!("  State:  {} EXACT MATCH", "✓".green()),
                (Ok(true), _) => println!(
                    "  State:  {} states equal but {} op(s) failed — content may be missing",
                    "⚠".yellow(),
                    stats.failed
                ),
                (Ok(false), _) => {
                    println!("  State:  {} MISMATCH (see diffs above)", "✗".red())
                }
                (Err(e), _) => println!("  State:  {} Could not verify: {e}", "~".yellow()),
            }
        }

        // From the CLI's perspective, the import succeeded iff every op the
        // reactor accepted was the ones we tried to push. A state mismatch on
        // a volatile field (e.g. `lastModified` updated when ops are replayed)
        // is informational only — under --strict we surface it as a failure,
        // but normal mode treats the doc as successfully imported.
        let ops_failed = stats.failed > 0;
        let state_mismatched = matches!(state_match, Ok(false));
        let doc_failed = ops_failed || (strict && state_mismatched);
        if !quiet {
            if doc_failed {
                println!("  {} Imported with errors", "⚠".yellow());
            } else if state_mismatched {
                println!(
                    "  {} Imported (state has drift on volatile fields)",
                    "✓".green()
                );
            } else {
                println!("  {} Imported", "✓".green());
            }
        }
        if doc_failed && strict {
            bail!(
                "import aborted (--strict): document '{}' had {} failed op(s){}",
                doc_name,
                stats.failed,
                if state_mismatched {
                    " and a state mismatch"
                } else {
                    ""
                }
            );
        }
        if !doc_failed {
            success += 1;
        }
    }

    if !quiet {
        let icon = if total_ops_failed == 0 {
            "✓".green().to_string()
        } else {
            "⚠".yellow().to_string()
        };
        println!(
            "\n{icon} {success}/{} documents imported into drive '{drive}' \
             ({total_ops_attempted} ops attempted, {total_ops_failed} failed)",
            files.len(),
        );
    }
    if total_ops_failed > 0 && strict {
        bail!(
            "import finished with {} failed op(s) (--strict)",
            total_ops_failed
        );
    }
    if success < files.len() {
        bail!(
            "import finished with errors: only {success}/{} documents fully imported",
            files.len()
        );
    }
    Ok(())
}

/// Per-document op-application stats accumulated during import.
#[derive(Default)]
struct OpStats {
    attempted: usize,
    succeeded: usize,
    failed: usize,
}

/// Recursively rewrite UUID references in a JSON value using the old → new
/// ID map built during import. We walk every string and replace it whole if
/// it matches a key in the map. This catches cross-document references in
/// fields like `targetDocumentId`, `noteRef`, `childRef`, contributor lists,
/// `operatorId`, etc., without having to enumerate them by name.
fn rewrite_ids_in_value(value: &mut Value, id_map: &std::collections::HashMap<String, String>) {
    match value {
        Value::String(s) => {
            if let Some(new) = id_map.get(s.as_str()) {
                *s = new.clone();
            }
        }
        Value::Array(arr) => {
            for v in arr {
                rewrite_ids_in_value(v, id_map);
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                rewrite_ids_in_value(v, id_map);
            }
        }
        _ => {}
    }
}

/// Push operations via model-specific mutations (e.g. DocumentModel_setModelName).
/// Skips document-scope ops (CREATE_DOCUMENT, UPGRADE_DOCUMENT) since the doc
/// was already created. Converts SCREAMING_SNAKE op types to camelCase and
/// looks them up in the introspection cache for proper typed mutations.
///
/// Cross-document references in op inputs (any UUID in `id_map`) are rewritten
/// to the new local UUIDs so an imported drive's internal references stay
/// connected after the original IDs are reassigned.
async fn push_operations_via_mutate(
    client: &GraphQLClient,
    doc_id: &str,
    operations: &PhdOperations,
    model: &crate::graphql::introspection::DocumentModel,
    id_map: &std::collections::HashMap<String, String>,
    quiet: bool,
) -> Result<OpStats> {
    let mut stats = OpStats::default();

    // Iterate all non-document scopes (global, local, custom, etc.)
    for op in operations.domain_ops() {
        let (op_type, mut input) = if let Some(action) = op.get("action") {
            let t = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let i = action
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            (t.to_string(), i)
        } else {
            let t = op.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let input_text = op.get("inputText").and_then(|v| v.as_str()).unwrap_or("{}");
            let i: Value =
                serde_json::from_str(input_text).unwrap_or(Value::Object(serde_json::Map::new()));
            (t.to_string(), i)
        };

        stats.attempted += 1;

        // Rewrite cross-document references using the old → new ID map.
        // Done before camelCase lookup so the rewritten input is what we
        // actually send. No-op if id_map is empty.
        if !id_map.is_empty() {
            rewrite_ids_in_value(&mut input, id_map);
        }

        // Convert SCREAMING_SNAKE (e.g. SET_MODEL_NAME) to camelCase (e.g. setModelName)
        let camel_name = screaming_snake_to_camel(&op_type);

        // Find the matching operation in the model's introspection cache
        let model_op = match model.operations.iter().find(|o| o.operation == camel_name) {
            Some(op) => op,
            None => {
                if !quiet {
                    println!("    ⚠ {op_type}: no matching mutation found (tried {camel_name})");
                }
                stats.failed += 1;
                continue;
            }
        };

        // Build the mutation using the model's namespace-aware helper
        let has_input_arg = model_op.args.iter().any(|a| a.name == "input");
        let selection = "{ id }";

        let (mutation, vars) = if has_input_arg {
            let input_type = model_op
                .args
                .iter()
                .find(|a| a.name == "input")
                .map(|a| &a.type_name)
                .unwrap();
            let required = model_op
                .args
                .iter()
                .find(|a| a.name == "input")
                .is_some_and(|a| a.required);
            let bang = if required { "!" } else { "" };

            let args_str = "docId: $docId, input: $input";
            let body = model.mutation_body(&model_op.full_name, args_str, selection);
            let query = format!("mutation($docId: PHID!, $input: {input_type}{bang}) {{ {body} }}");
            let vars = serde_json::json!({
                "docId": doc_id,
                "input": input,
            });
            (query, vars)
        } else {
            let mut var_decls = vec!["$docId: PHID!".to_string()];
            let mut arg_refs = vec!["docId: $docId".to_string()];
            let mut vars_map = serde_json::Map::new();
            vars_map.insert("docId".into(), Value::String(doc_id.to_string()));

            if let Value::Object(map) = &input {
                for (key, val) in map {
                    let arg_type = model_op
                        .args
                        .iter()
                        .find(|a| a.name == *key)
                        .map(|a| a.type_name.as_str())
                        .unwrap_or("String");
                    let required = model_op
                        .args
                        .iter()
                        .find(|a| a.name == *key)
                        .is_some_and(|a| a.required);
                    let bang = if required { "!" } else { "" };

                    var_decls.push(format!("${key}: {arg_type}{bang}"));
                    arg_refs.push(format!("{key}: ${key}"));
                    vars_map.insert(key.clone(), val.clone());
                }
            }

            let args_str = arg_refs.join(", ");
            let body = model.mutation_body(&model_op.full_name, &args_str, selection);
            let query = format!(
                "mutation({decls}) {{ {body} }}",
                decls = var_decls.join(", "),
            );
            (query, Value::Object(vars_map))
        };

        if stats.succeeded > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(WRITE_DELAY_MS)).await;
        }

        match client.query(&mutation, Some(&vars)).await {
            Ok(_) => {
                stats.succeeded += 1;
            }
            Err(e) => {
                let err_str = format!("{e}");
                if !quiet {
                    println!("    {} {op_type}: {err_str}", "✗".red());
                }
                stats.failed += 1;
                // Continue with remaining ops
            }
        }
    }

    Ok(stats)
}

/// Convert SCREAMING_SNAKE_CASE to camelCase.
/// e.g. "SET_MODEL_NAME" → "setModelName"
fn screaming_snake_to_camel(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;
    for (i, c) in s.chars().enumerate() {
        if c == '_' {
            capitalize_next = true;
        } else if i == 0 || (!capitalize_next && result.is_empty()) {
            result.push(c.to_ascii_lowercase());
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c.to_ascii_lowercase());
        }
    }
    result
}

/// Verify the imported document's state matches the expected state from the .phd
async fn verify_state(
    client: &GraphQLClient,
    doc_id: &str,
    expected_global: &Value,
) -> Result<bool> {
    let escaped = doc_id.replace('"', r#"\""#);
    let query = format!(r#"{{ document(identifier: "{escaped}") {{ document {{ state }} }} }}"#,);
    let data = client.query(&query, None).await?;

    let actual = data
        .pointer("/document/document/state/global")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let expected_str = serde_json::to_string(expected_global)?;
    let actual_str = serde_json::to_string(&actual)?;

    if expected_str == actual_str {
        return Ok(true);
    }

    // Report differences
    if let (Some(expected_map), Some(actual_map)) =
        (expected_global.as_object(), actual.as_object())
    {
        let mut all_keys: Vec<&String> = expected_map.keys().chain(actual_map.keys()).collect();
        all_keys.sort();
        all_keys.dedup();

        let mut diffs = 0;
        for key in &all_keys {
            let ev = expected_map.get(*key);
            let av = actual_map.get(*key);
            if ev != av {
                diffs += 1;
                if diffs <= 5 {
                    let ev_str = ev
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                        .unwrap_or_else(|| "undefined".to_string());
                    let av_str = av
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                        .unwrap_or_else(|| "undefined".to_string());
                    let ev_short = if ev_str.len() > 60 {
                        &ev_str[..60]
                    } else {
                        &ev_str
                    };
                    let av_short = if av_str.len() > 60 {
                        &av_str[..60]
                    } else {
                        &av_str
                    };
                    println!("    DIFF {key}: expected={ev_short} actual={av_short}");
                }
            }
        }
        if diffs > 5 {
            println!("    ... and {} more differences", diffs - 5);
        }
    }

    Ok(false)
}
