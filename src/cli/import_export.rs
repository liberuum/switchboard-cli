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

/// Wait for an async job (returned by `mutateDocumentAsync`) to reach a
/// terminal status. Polls `jobStatus` rather than opening a subscription —
/// `jobStatus` returns `FAILED` for unknown ids and a status string for real
/// ones, so the poll never produces server log noise. Returns Ok on terminal
/// success states (COMPLETED, READ_READY) and Err on terminal failure states
/// (FAILED, CANCELLED) or timeout.
pub(crate) async fn wait_for_job(
    client: &GraphQLClient,
    job_id: &str,
    timeout_ms: u64,
) -> Result<()> {
    let escaped = job_id.replace('"', r#"\""#);
    let query = format!(r#"{{ jobStatus(jobId: "{escaped}") {{ id status error }} }}"#);
    let start = std::time::Instant::now();
    let budget = std::time::Duration::from_millis(timeout_ms);
    loop {
        let data = client.query(&query, None).await?;
        let job = &data["jobStatus"];
        let status = job["status"].as_str().unwrap_or("UNKNOWN");
        match status {
            "COMPLETED" | "READ_READY" => return Ok(()),
            "FAILED" | "CANCELLED" => {
                let err = job["error"].as_str().unwrap_or("(no error message)");
                bail!("job {job_id} ended with status {status}: {err}");
            }
            _ => {}
        }
        if start.elapsed() >= budget {
            bail!("job {job_id} did not complete within {timeout_ms}ms (last status: {status})");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Fetch drive nodes via the document() query on the main GraphQL endpoint.
pub(crate) async fn fetch_drive_nodes(
    client: &GraphQLClient,
    drive_identifier: &str,
) -> Result<Vec<Value>> {
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
pub(crate) async fn fetch_document(
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

    // Expand any directory args into individual .phd files, capturing the
    // sub-path components so we can reproduce the source's folder hierarchy
    // on the destination drive. Plain file args land at drive root.
    let entries = expand_phd_inputs(&files);
    if entries.is_empty() {
        bail!("No .phd files found in the supplied paths");
    }
    let total_inputs = entries.len();

    // Pre-cache existing folders so repeat imports reuse folders instead of
    // creating duplicates with the same name/parent.
    let mut folder_cache = build_existing_folder_cache(&client, &drive_id).await;

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
    // Ops with forward UUID references queue here during the per-doc loop
    // and drain after every doc has been created (so the id_map is final).
    let mut deferred_ops: Vec<DeferredOp> = Vec::new();

    for entry in &entries {
        let path = entry.path.as_path();
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("?"));

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
            if !entry.folder_chain.is_empty() {
                println!("  Path: {}", entry.folder_chain.join("/"));
            }
        }

        // Ensure the destination folder hierarchy exists, creating folders
        // as needed. We do this before doc creation so the createDocument
        // mutation can drop the new doc straight into the correct folder
        // via parentIdentifier (skipping a follow-up moveNode round trip).
        let parent_folder_id =
            match ensure_folder_chain(&client, &drive_id, &entry.folder_chain, &mut folder_cache)
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    println!("  {} Failed to create folder chain: {e}", "✗".red());
                    continue;
                }
            };

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

        // moveNode is intentionally deferred until *after* op replay below.
        // push_operations_via_mutate uses mutateDocumentAsync internally and
        // waits for the resulting job to complete, which doubles as our
        // "doc is committed" gate. Firing moveNode before that gate would
        // race the commit and produce "Document not found" log noise.

        // Record the old → new ID mapping so subsequent ops within this
        // batch can rewrite cross-document references on the fly.
        if !contents.header.id.is_empty() {
            id_map.insert(contents.header.id.clone(), new_doc_id.clone());
        }

        // Step 2: Push operations as a single batched mutateDocumentAsync.
        // The job returned by the server doubles as our visibility gate —
        // by the time `wait_for_job` returns, the new doc is committed and
        // every action has been applied (or rejected) atomically.
        let mut stats = OpStats::default();
        if ops_count > 0 {
            match push_operations_via_mutate(
                &client,
                &new_doc_id,
                doc_name,
                &contents.operations,
                &id_map,
                &mut deferred_ops,
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
                let mut parts = Vec::new();
                parts.push(format!("{} pushed", stats.succeeded));
                if stats.failed > 0 {
                    parts.push(format!("{} failed", stats.failed));
                }
                if stats.deferred > 0 {
                    parts.push(format!("{} deferred (forward refs)", stats.deferred));
                }
                println!("  Ops:    {} of {}", parts.join(", "), stats.attempted);
            }
        } else if !quiet {
            println!("  No operations to push");
        }

        total_ops_attempted += stats.attempted;
        total_ops_failed += stats.failed;

        // Step 2b: Place the doc in its target folder. Deferred until after
        // op replay so the async-job gate (inside push_operations_via_mutate)
        // has confirmed the new doc is committed — moveNode against an
        // uncommitted doc would log a server-side "Document not found".
        if let Some(folder_id) = &parent_folder_id {
            let move_mutation = "mutation($docId: PHID!, $input: DocumentDrive_MoveNodeInput!) { \
                 DocumentDrive { moveNode(docId: $docId, input: $input) { id } } }";
            let move_vars = serde_json::json!({
                "docId": drive_id,
                "input": {
                    "srcFolder": new_doc_id,
                    "targetParentFolder": folder_id,
                }
            });
            if let Err(e) = client.query(move_mutation, Some(&move_vars)).await {
                eprintln!("  {} Could not move doc into folder: {e}", "⚠".yellow());
            }
        }

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

    // ── Drain deferred forward-ref ops ───────────────────────────────────
    //
    // Ops queued during the per-doc loop because their inputs referenced
    // a UUID we hadn't yet seen the new ID for. Now that every doc is
    // created and id_map is complete, rewrite their inputs and dispatch.
    let (deferred_succeeded, deferred_failed) = if !deferred_ops.is_empty() {
        if !quiet {
            println!(
                "\n  ── Drain: {} deferred forward-ref op(s) ──",
                deferred_ops.len()
            );
        }
        let (s, f) = drain_deferred_ops(&client, &deferred_ops, &cache, &id_map, quiet).await;
        total_ops_attempted += deferred_ops.len();
        total_ops_failed += f;
        if !quiet {
            let icon = if f == 0 {
                "✓".green().to_string()
            } else {
                "⚠".yellow().to_string()
            };
            println!("  {icon} Drained: {s} resolved, {f} failed");
        }
        (s, f)
    } else {
        (0, 0)
    };
    let _ = deferred_succeeded; // recorded for future use; not surfaced in the final line

    if !quiet {
        let icon = if total_ops_failed == 0 {
            "✓".green().to_string()
        } else {
            "⚠".yellow().to_string()
        };
        println!(
            "\n{icon} {success}/{total_inputs} documents imported into drive '{drive}' \
             ({total_ops_attempted} ops attempted, {total_ops_failed} failed)",
        );
    }
    if total_ops_failed > 0 && strict {
        bail!(
            "import finished with {} failed op(s) (--strict)",
            total_ops_failed
        );
    }
    if success < total_inputs || deferred_failed > 0 && strict {
        bail!(
            "import finished with errors: only {success}/{total_inputs} documents fully imported",
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
    /// Ops whose input references a UUID we haven't seen the new ID for yet
    /// (forward refs to a doc later in the import). These are queued and
    /// drained after every doc has been created so the now-complete id_map
    /// can rewrite cross-document references correctly.
    deferred: usize,
}

/// An op that was queued during the per-doc pass because at least one
/// UUID-shaped string in its input wasn't yet in the id_map. Drained at the
/// end of `run_import` once the map is final.
struct DeferredOp {
    /// New (local) doc UUID this op will apply to.
    doc_id: String,
    /// Human-readable doc name for error reporting.
    doc_name: String,
    /// Original op JSON (action.type / action.input live underneath).
    op: Value,
}

/// One file slated for import, paired with the folder hierarchy it should land
/// in inside the destination drive. The `folder_chain` is empty for files
/// passed directly on the command line; for files discovered by walking a
/// directory argument it's the path components from that directory down to
/// the file's parent.
struct ImportEntry {
    path: std::path::PathBuf,
    folder_chain: Vec<String>,
}

/// Expand each input arg: directory args are walked recursively (their
/// internal structure becomes folder_chain), file args land at drive root.
fn expand_phd_inputs(paths: &[String]) -> Vec<ImportEntry> {
    use std::path::Path;
    let mut entries = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            walk_phd_dir(path, path, &mut entries);
        } else if path.is_file() {
            entries.push(ImportEntry {
                path: path.to_path_buf(),
                folder_chain: vec![],
            });
        } else {
            // Doesn't exist — keep the path as-is so the read step reports a
            // meaningful "Failed to read" error rather than silently dropping it.
            entries.push(ImportEntry {
                path: path.to_path_buf(),
                folder_chain: vec![],
            });
        }
    }
    entries
}

fn walk_phd_dir(base: &std::path::Path, current: &std::path::Path, out: &mut Vec<ImportEntry>) {
    let Ok(read) = std::fs::read_dir(current) else {
        return;
    };
    for entry in read.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_phd_dir(base, &p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("phd") {
            let rel = p.strip_prefix(base).unwrap_or(&p);
            let folder_chain: Vec<String> = rel
                .parent()
                .map(|d| {
                    d.components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            out.push(ImportEntry {
                path: p,
                folder_chain,
            });
        }
    }
}

/// Pre-populate a folder-path → UUID cache from a drive's existing nodes so
/// repeat imports against the same drive reuse folders instead of duplicating
/// them. The cache key is the full path from drive root, so "Billing/Aug" and
/// "Other/Aug" are distinct entries.
async fn build_existing_folder_cache(
    client: &GraphQLClient,
    drive_id: &str,
) -> std::collections::HashMap<Vec<String>, String> {
    let mut out = std::collections::HashMap::new();
    let escaped = drive_id.replace('"', r#"\""#);
    let q = format!(r#"{{ document(identifier: "{escaped}") {{ document {{ state }} }} }}"#);
    let Ok(data) = client.query(&q, None).await else {
        return out;
    };
    let Some(nodes) = data
        .pointer("/document/document/state/global/nodes")
        .and_then(|v| v.as_array())
    else {
        return out;
    };

    // Build id → (name, parent_id) for folder nodes only.
    let mut folder_info: std::collections::HashMap<String, (String, Option<String>)> =
        std::collections::HashMap::new();
    for n in nodes {
        if n["kind"].as_str() != Some("folder") {
            continue;
        }
        let id = n["id"].as_str().unwrap_or("").to_string();
        let name = n["name"].as_str().unwrap_or("").to_string();
        let parent = n["parentFolder"].as_str().map(String::from);
        if !id.is_empty() && !name.is_empty() {
            folder_info.insert(id, (name, parent));
        }
    }

    // For each folder, walk its parent chain to construct the path key.
    for (id, _) in folder_info.iter() {
        let mut chain: Vec<String> = Vec::new();
        let mut cur = Some(id.clone());
        while let Some(c) = cur {
            let Some((name, parent)) = folder_info.get(&c) else {
                break;
            };
            chain.push(name.clone());
            cur = parent.clone();
        }
        chain.reverse();
        if !chain.is_empty() {
            out.insert(chain, id.clone());
        }
    }
    out
}

/// Walk `chain` from drive root, creating each folder if it isn't already in
/// the cache. Returns the leaf folder's UUID (or None when chain is empty).
async fn ensure_folder_chain(
    client: &GraphQLClient,
    drive_id: &str,
    chain: &[String],
    cache: &mut std::collections::HashMap<Vec<String>, String>,
) -> Result<Option<String>> {
    if chain.is_empty() {
        return Ok(None);
    }
    let mut current_parent: Option<String> = None;
    for i in 1..=chain.len() {
        let prefix: Vec<String> = chain[..i].to_vec();
        if let Some(id) = cache.get(&prefix) {
            current_parent = Some(id.clone());
            continue;
        }
        let folder_name = &prefix[prefix.len() - 1];
        let new_id = uuid::Uuid::new_v4().to_string();
        let mut input = serde_json::json!({ "id": new_id, "name": folder_name });
        if let Some(p) = &current_parent {
            input["parentFolder"] = serde_json::json!(p);
        }
        let mutation = "mutation($docId: PHID!, $input: DocumentDrive_AddFolderInput!) { \
             DocumentDrive { addFolder(docId: $docId, input: $input) { id } } }";
        let vars = serde_json::json!({ "docId": drive_id, "input": input });
        client.query(mutation, Some(&vars)).await?;
        cache.insert(prefix, new_id.clone());
        current_parent = Some(new_id);
    }
    Ok(current_parent)
}

/// Returns true if `value` contains any UUID-shaped string that is not in
/// the id_map. Used to decide whether an op references a doc we haven't
/// imported yet (forward reference) — such ops get deferred so the second
/// pass can rewrite them with the now-complete map.
///
/// "UUID-shaped" matches `helpers::is_uuid` (8-4-4-4-12 hex). Strings that
/// aren't UUIDs are ignored, so plain text content with uuid-like substrings
/// won't trigger deferral.
fn has_forward_ref(value: &Value, id_map: &std::collections::HashMap<String, String>) -> bool {
    match value {
        Value::String(s) => helpers::is_uuid(s) && !id_map.contains_key(s.as_str()),
        Value::Array(arr) => arr.iter().any(|v| has_forward_ref(v, id_map)),
        Value::Object(map) => map.values().any(|v| has_forward_ref(v, id_map)),
        _ => false,
    }
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
///
/// When an op's input references a UUID that's not yet in `id_map` (a forward
/// reference to a doc later in the import), the op is queued in
/// `deferred_ops` instead of dispatched. After every doc has been created,
/// `run_import` drains the queue with the now-complete map. This keeps
/// bidirectional links between docs intact across a single-pass import.
#[allow(clippy::too_many_arguments)]
async fn push_operations_via_mutate(
    client: &GraphQLClient,
    doc_id: &str,
    doc_name: &str,
    operations: &PhdOperations,
    id_map: &std::collections::HashMap<String, String>,
    deferred_ops: &mut Vec<DeferredOp>,
    quiet: bool,
) -> Result<OpStats> {
    let mut stats = OpStats::default();

    // Build a single batched action list. mutateDocumentAsync queues these
    // server-side and returns a job id; `wait_for_job` blocks until the job
    // reaches a terminal state. Doing it as one call (rather than per-op
    // sync mutations) is what makes this race-free: the job processor
    // serialises the doc's commit and the actions, so we never need a
    // CLI-side delay or visibility probe (both of which would generate
    // "Document not found" log noise on the reactor).
    let mut actions: Vec<Value> = Vec::new();

    for op in operations.domain_ops() {
        let (op_type, mut input, scope) = if let Some(action) = op.get("action") {
            let t = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let s = action
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("global");
            let i = action
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            (t.to_string(), i, s.to_string())
        } else {
            let t = op.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let s = op.get("scope").and_then(|v| v.as_str()).unwrap_or("global");
            let input_text = op.get("inputText").and_then(|v| v.as_str()).unwrap_or("{}");
            let i: Value =
                serde_json::from_str(input_text).unwrap_or(Value::Object(serde_json::Map::new()));
            (t.to_string(), i, s.to_string())
        };

        stats.attempted += 1;

        // Forward references to docs not yet imported go to the drain phase
        // with the same logic as before — when the drain runs, id_map is
        // complete and we can rewrite + dispatch them then.
        if has_forward_ref(&input, id_map) {
            stats.deferred += 1;
            deferred_ops.push(DeferredOp {
                doc_id: doc_id.to_string(),
                doc_name: doc_name.to_string(),
                op: op.clone(),
            });
            continue;
        }

        if !id_map.is_empty() {
            rewrite_ids_in_value(&mut input, id_map);
        }

        let action_id = op
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let timestamp = op
            .get("timestampUtcMs")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(crate::cli::docs::iso_now);

        actions.push(serde_json::json!({
            "id": action_id,
            "type": op_type,
            "input": input,
            "scope": scope,
            "timestampUtcMs": timestamp,
        }));
    }

    if actions.is_empty() {
        return Ok(stats);
    }

    let mutation = "mutation($di: String!, $acts: [JSONObject!]!) { mutateDocumentAsync(documentIdentifier: $di, actions: $acts) }";
    let vars = serde_json::json!({
        "di": doc_id,
        "acts": actions,
    });
    let resp = client.query(mutation, Some(&vars)).await?;
    let job_id = resp["mutateDocumentAsync"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("mutateDocumentAsync did not return a job id"))?;

    // 30s budget per doc covers worst-case reactor commit + replay latency.
    match wait_for_job(client, job_id, 30_000).await {
        Ok(()) => stats.succeeded = stats.attempted - stats.deferred,
        Err(e) => {
            if !quiet {
                println!("    {} batch failed: {e}", "✗".red());
            }
            stats.failed = stats.attempted - stats.deferred;
        }
    }

    Ok(stats)
}

/// Re-attempt deferred ops after every doc has been created and id_map is
/// final. Each op's input is re-rewritten with the now-complete map, then
/// dispatched. Returns (succeeded, failed) counts.
async fn drain_deferred_ops(
    client: &GraphQLClient,
    deferred: &[DeferredOp],
    _cache: &crate::graphql::IntrospectionCache,
    id_map: &std::collections::HashMap<String, String>,
    quiet: bool,
) -> (usize, usize) {
    // Group ops by destination doc, then send each group as a single
    // mutateDocumentAsync + wait — same race-free pattern as
    // push_operations_via_mutate.
    use std::collections::HashMap;
    let mut grouped: HashMap<String, (String, Vec<Value>)> = HashMap::new(); // doc_id -> (doc_name, actions)
    let mut failed = 0;

    for d in deferred {
        let (op_type, mut input, scope) = if let Some(action) = d.op.get("action") {
            let t = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let s = action
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("global");
            let i = action
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            (t.to_string(), i, s.to_string())
        } else {
            let t = d.op.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let s =
                d.op.get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("global");
            let input_text =
                d.op.get("inputText")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
            let i: Value =
                serde_json::from_str(input_text).unwrap_or(Value::Object(serde_json::Map::new()));
            (t.to_string(), i, s.to_string())
        };

        if !id_map.is_empty() {
            rewrite_ids_in_value(&mut input, id_map);
        }

        let action_id =
            d.op.get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let timestamp =
            d.op.get("timestampUtcMs")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(crate::cli::docs::iso_now);

        grouped
            .entry(d.doc_id.clone())
            .or_insert_with(|| (d.doc_name.clone(), Vec::new()))
            .1
            .push(serde_json::json!({
                "id": action_id,
                "type": op_type,
                "input": input,
                "scope": scope,
                "timestampUtcMs": timestamp,
            }));
    }

    let mutation = "mutation($di: String!, $acts: [JSONObject!]!) { mutateDocumentAsync(documentIdentifier: $di, actions: $acts) }";
    let mut succeeded = 0;
    for (doc_id, (doc_name, actions)) in grouped {
        let count = actions.len();
        let vars = serde_json::json!({ "di": doc_id, "acts": actions });
        let resp = match client.query(mutation, Some(&vars)).await {
            Ok(r) => r,
            Err(e) => {
                if !quiet {
                    println!("    {} {doc_name}: drain submit failed: {e}", "✗".red());
                }
                failed += count;
                continue;
            }
        };
        let job_id = match resp["mutateDocumentAsync"].as_str() {
            Some(j) => j,
            None => {
                if !quiet {
                    println!("    {} {doc_name}: no job id from drain", "✗".red());
                }
                failed += count;
                continue;
            }
        };
        match wait_for_job(client, job_id, 30_000).await {
            Ok(()) => succeeded += count,
            Err(e) => {
                if !quiet {
                    println!("    {} {doc_name}: drain job failed: {e}", "✗".red());
                }
                failed += count;
            }
        }
    }
    (succeeded, failed)
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

#[cfg(test)]
mod tests {
    use super::{has_forward_ref, rewrite_ids_in_value};
    use serde_json::json;
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

    #[test]
    fn no_uuids_means_no_forward_ref() {
        let m = map(&[]);
        assert!(!has_forward_ref(&json!({ "title": "hello" }), &m));
        assert!(!has_forward_ref(&json!({ "amount": 42 }), &m));
        assert!(!has_forward_ref(&json!([1, 2, 3]), &m));
    }

    #[test]
    fn known_uuid_is_not_a_forward_ref() {
        let m = map(&[(A, "new-a")]);
        assert!(!has_forward_ref(&json!({ "targetDocumentId": A }), &m));
    }

    #[test]
    fn unknown_uuid_is_a_forward_ref() {
        let m = map(&[(A, "new-a")]);
        // B is not in the map → forward (or external) ref
        assert!(has_forward_ref(&json!({ "targetDocumentId": B }), &m));
    }

    #[test]
    fn deeply_nested_unknown_uuid_detected() {
        let m = map(&[(A, "new-a")]);
        let v = json!({
            "links": [
                { "id": "abc", "targetDocumentId": A },
                { "id": "def", "targetDocumentId": C },  // C unknown
            ]
        });
        assert!(has_forward_ref(&v, &m));
    }

    #[test]
    fn all_uuids_known_means_no_forward_ref_even_when_nested() {
        let m = map(&[(A, "new-a"), (B, "new-b")]);
        let v = json!({
            "links": [
                { "targetDocumentId": A },
                { "targetDocumentId": B },
            ]
        });
        assert!(!has_forward_ref(&v, &m));
    }

    #[test]
    fn non_uuid_strings_ignored() {
        let m = map(&[]);
        // "11111111" looks vaguely uuidish but isn't 36 chars in 8-4-4-4-12 form
        assert!(!has_forward_ref(&json!({ "id": "not-a-uuid" }), &m));
        assert!(!has_forward_ref(&json!({ "id": "11111111" }), &m));
    }

    #[test]
    fn rewrite_replaces_known_uuids() {
        let m = map(&[(A, "new-a"), (B, "new-b")]);
        let mut v = json!({
            "targetDocumentId": A,
            "noteRef": B,
            "title": "unchanged",
            "nested": { "childRef": A }
        });
        rewrite_ids_in_value(&mut v, &m);
        assert_eq!(v["targetDocumentId"], "new-a");
        assert_eq!(v["noteRef"], "new-b");
        assert_eq!(v["title"], "unchanged");
        assert_eq!(v["nested"]["childRef"], "new-a");
    }

    #[test]
    fn rewrite_leaves_unknown_uuids_alone() {
        let m = map(&[(A, "new-a")]);
        let mut v = json!({ "targetDocumentId": B });
        rewrite_ids_in_value(&mut v, &m);
        // External UUIDs pass through unchanged
        assert_eq!(v["targetDocumentId"], B);
    }
}
