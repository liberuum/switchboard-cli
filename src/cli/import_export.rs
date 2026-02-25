use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use serde_json::Value;
use std::path::Path;

use crate::cli::helpers::{self, resolve_drive_id};
use crate::graphql::GraphQLClient;
use crate::output::OutputFormat;
use crate::phd::{self, PhdHeader, PhdOperations, PhdState};

#[derive(Subcommand)]
pub enum ExportCommand {
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
    },
    /// Export all documents in a drive as .phd files
    Drive {
        /// Drive ID or slug
        drive: String,
        /// Output directory (defaults to ./<drive-name>/)
        #[arg(long, short)]
        out: Option<String>,
    },
}

pub async fn run_export(cmd: ExportCommand, _format: OutputFormat, profile_name: Option<&str>, quiet: bool) -> Result<()> {
    match cmd {
        ExportCommand::Doc { doc_id, drive, out } => export_doc(&doc_id, &drive, out.as_deref(), profile_name, quiet).await,
        ExportCommand::Drive { drive, out } => export_drive(&drive, out.as_deref(), profile_name, quiet).await,
    }
}

/// Build the proper PhdHeader matching the reference download-drive-documents.ts format
fn build_header(doc: &Value) -> PhdHeader {
    let doc_id = doc["id"].as_str().unwrap_or("").to_string();
    let doc_name = doc["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("document").to_string();
    let doc_type = doc["documentType"].as_str().unwrap_or("unknown").to_string();
    let revision = doc["revision"].as_u64().unwrap_or(0);

    PhdHeader {
        id: doc_id.clone(),
        sig: serde_json::json!({ "publicKey": {}, "nonce": "" }),
        document_type: doc_type,
        created_at_utc_iso: doc["createdAtUtcIso"].as_str().map(|s| s.to_string()),
        slug: Some(doc_id),
        name: doc_name,
        branch: "main".to_string(),
        revision: serde_json::json!({ "global": revision }),
        last_modified_at_utc_iso: doc["lastModifiedAtUtcIso"].as_str().map(|s| s.to_string()),
        meta: Value::Object(serde_json::Map::new()),
    }
}

/// Build the PhdState wrapping stateJSON under the `global` key
fn build_current_state(state_json: Value) -> PhdState {
    PhdState {
        global: state_json,
        ..PhdState::default()
    }
}

async fn export_doc(
    doc_id: &str,
    drive: &str,
    out_path: Option<&str>,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, _cache) = helpers::setup_with_cache(profile_name)?;
    let drive_id = resolve_drive_id(&client, drive).await?;

    // Use the drive endpoint which has real operations and state
    let base_url = base_url_from(&client.url);
    let drive_client = GraphQLClient::new(
        format!("{base_url}/d/{drive_id}"),
        _profile.token.clone(),
    );

    let (doc, operations) = fetch_document_via_drive(&drive_client, doc_id).await?;

    let header = build_header(&doc);
    let state_json = parse_state_json(&doc);
    let phd_ops = PhdOperations { global: operations.clone() };
    let initial_state = PhdState::default();
    let current_state = build_current_state(state_json);

    // Determine output path
    let safe_name = sanitize_filename(&header.name);
    let default_path = format!("{safe_name}.phd");
    let out = out_path.unwrap_or(&default_path);
    let path = Path::new(out);

    phd::write_phd(path, &header, &initial_state, &current_state, &phd_ops)?;

    if !quiet {
        let file_size = std::fs::metadata(path)?.len();
        println!(
            "{} Saved {} ({}, {} ops, {})",
            "✓".green(),
            path.display(),
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
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, _cache) = helpers::setup_with_cache(profile_name)?;
    let drive_id = resolve_drive_id(&client, drive).await?;

    // Build drive endpoint client for fetching docs with operations
    let base_url = base_url_from(&client.url);
    let drive_client = GraphQLClient::new(
        format!("{base_url}/d/{drive_id}"),
        _profile.token.clone(),
    );

    // Get drive info and node tree
    let drive_query = format!(
        r#"{{
  driveDocument(idOrSlug: "{drive}") {{
    name
    state {{
      nodes {{
        ... on DocumentDrive_FileNode {{ id name kind documentType parentFolder }}
        ... on DocumentDrive_FolderNode {{ id name kind parentFolder }}
      }}
    }}
  }}
}}"#,
        drive = drive.replace('"', r#"\""#)
    );

    let data = client.query(&drive_query, None).await?;
    let drive_name = data
        .pointer("/driveDocument/name")
        .and_then(|v| v.as_str())
        .unwrap_or(drive);

    let nodes = data
        .pointer("/driveDocument/state/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

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
        let folders = nodes.iter().filter(|n| n["kind"].as_str() == Some("folder")).count();
        println!("  Name: {} ({} files, {} folders)", drive_name, files.len(), folders);
    }

    // Create output directory
    let safe_name = sanitize_filename(drive_name);
    let default_dir = format!("./{safe_name}");
    let dir_str = out_dir.unwrap_or(&default_dir);
    let dir = Path::new(dir_str);
    std::fs::create_dir_all(dir)?;

    if !quiet {
        println!("  Downloading {} documents...", files.len());
    }

    let mut success = 0;
    let total = files.len();

    for (i, file_node) in files.iter().enumerate() {
        let file_id = file_node["id"].as_str().unwrap_or("");
        let file_name = file_node["name"].as_str().unwrap_or("document");
        let file_type = file_node["documentType"].as_str().unwrap_or("unknown");

        match fetch_document_via_drive(&drive_client, file_id).await {
            Ok((doc, operations)) => {
                let state_json = parse_state_json(&doc);

                let header = build_header(&doc);
                let phd_ops = PhdOperations { global: operations.clone() };
                let initial_state = PhdState::default();
                let current_state = build_current_state(state_json);

                let safe_file = sanitize_filename(file_name);
                let file_path = dir.join(format!("{safe_file}.phd"));

                match phd::write_phd(&file_path, &header, &initial_state, &current_state, &phd_ops) {
                    Ok(()) => {
                        if !quiet {
                            let size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
                            println!(
                                "  [{}/{}] {} ({}) → {} {}",
                                i + 1, total, file_name, file_type,
                                format_bytes(size), "✓".green()
                            );
                        }
                        success += 1;
                    }
                    Err(e) => {
                        println!(
                            "  [{}/{}] {} → {} {e}",
                            i + 1, total, file_name, "✗".red()
                        );
                    }
                }
            }
            Err(e) => {
                println!(
                    "  [{}/{}] {} → {} {e}",
                    i + 1, total, file_name, "✗".red()
                );
            }
        }
    }

    if !quiet {
        println!("{} {success} documents saved to {}/", "✓".green(), dir.display());
    }
    Ok(())
}

const OP_BATCH_SIZE: usize = 100;

/// Fetch a document's full data (state + operations) via the drive endpoint.
/// Matches the logic from download-drive-documents.ts:
/// - Uses {base_url}/d/{driveId} with document(id:) query
/// - Paginates operations with first/skip
/// - Transforms flat API ops to nested action format (gqlOperationToInternal)
async fn fetch_document_via_drive(
    drive_client: &GraphQLClient,
    doc_id: &str,
) -> Result<(Value, Vec<Value>)> {
    // First batch: metadata + stateJSON + first page of operations
    let variables = serde_json::json!({
        "id": doc_id,
        "first": OP_BATCH_SIZE,
        "skip": 0,
    });
    let data = drive_client.query(
        r#"query ($id: String!, $first: Int, $skip: Int) {
            document(id: $id) {
                id name documentType revision
                createdAtUtcIso lastModifiedAtUtcIso
                operations(first: $first, skip: $skip) {
                    id type index skip hash timestampUtcMs inputText error
                }
                stateJSON
            }
        }"#,
        Some(&variables),
    ).await?;

    let doc = data.get("document")
        .filter(|v| !v.is_null())
        .ok_or_else(|| anyhow::anyhow!("Document '{doc_id}' not found on drive endpoint"))?;

    let mut all_ops: Vec<Value> = doc
        .get("operations")
        .and_then(|v: &Value| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Paginate remaining operations if first batch was full
    while !all_ops.is_empty() && all_ops.len() % OP_BATCH_SIZE == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(REQUEST_DELAY_MS)).await;
        let vars = serde_json::json!({
            "id": doc_id,
            "first": OP_BATCH_SIZE,
            "skip": all_ops.len(),
        });
        let more = drive_client.query(
            r#"query ($id: String!, $first: Int, $skip: Int) {
                document(id: $id) {
                    operations(first: $first, skip: $skip) {
                        id type index skip hash timestampUtcMs inputText error
                    }
                }
            }"#,
            Some(&vars),
        ).await?;

        let batch = more
            .pointer("/document/operations")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if batch.is_empty() {
            break;
        }
        all_ops.extend(batch);
    }

    // Transform flat API ops to the nested action format (matching gqlOperationToInternal)
    let transformed_ops: Vec<Value> = all_ops.iter().map(|op| {
        let input_text = op.get("inputText").and_then(|v| v.as_str()).unwrap_or("{}");
        let input: Value = serde_json::from_str(input_text).unwrap_or(Value::String(input_text.to_string()));

        serde_json::json!({
            "id": op.get("id"),
            "index": op.get("index"),
            "skip": op.get("skip").and_then(|v| v.as_u64()).unwrap_or(0),
            "hash": op.get("hash"),
            "timestampUtcMs": op.get("timestampUtcMs"),
            "error": op.get("error").cloned().unwrap_or(Value::Null),
            "action": {
                "id": op.get("id"),
                "type": op.get("type"),
                "timestampUtcMs": op.get("timestampUtcMs"),
                "input": input,
                "scope": "global",
            }
        })
    }).collect();

    Ok((doc.clone(), transformed_ops))
}

fn parse_state_json(doc: &Value) -> Value {
    match doc.get("stateJSON") {
        Some(Value::String(s)) => {
            serde_json::from_str(s).unwrap_or(Value::Object(serde_json::Map::new()))
        }
        Some(v) => v.clone(),
        None => Value::Object(serde_json::Map::new()),
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
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

const PUSH_BATCH_SIZE: usize = 50;
const REQUEST_DELAY_MS: u64 = 200;

/// Derive the base URL from a GraphQL endpoint URL
/// e.g. "http://localhost:4001/graphql" -> "http://localhost:4001"
fn base_url_from(graphql_url: &str) -> String {
    graphql_url
        .trim_end_matches('/')
        .trim_end_matches("/graphql")
        .to_string()
}

pub async fn run_import(
    files: Vec<String>,
    drive: String,
    _format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;
    let drive_id = resolve_drive_id(&client, &drive).await?;

    if files.is_empty() {
        bail!("No .phd files specified");
    }

    // Build the drive endpoint for pushUpdates and state verification
    let base_url = base_url_from(&client.url);
    let drive_endpoint = format!("{base_url}/d/{drive_id}");
    let drive_client = GraphQLClient::new(drive_endpoint.clone(), _profile.token.clone());

    if !quiet {
        println!("  Importing {} file(s) into drive '{drive}'...", files.len());
        println!("  Drive endpoint: {drive_endpoint}");
    }

    let mut success = 0;

    for file_str in &files {
        let path = Path::new(file_str);
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or(file_str);

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
        let ops_count = contents.operations.global.len();

        if !quiet {
            println!("  Type: {doc_type}");
            println!("  Name: {doc_name}");
            println!("  Ops:  {ops_count} global");
        }

        // Find the matching model
        let model = match cache.find_model(doc_type) {
            Some(m) => m,
            None => {
                println!("  {} No matching model found for type '{doc_type}'", "✗".red());
                continue;
            }
        };

        // Step 1: Create the document via model-specific mutation
        let mutation = format!(
            r#"mutation {{ {create}(name: "{name}", driveId: "{drive_id}") }}"#,
            create = model.create_mutation,
            name = doc_name.replace('"', r#"\""#),
        );

        let data = match client.query(&mutation, None).await {
            Ok(d) => d,
            Err(e) => {
                println!("  {} Failed to create document: {e}", "✗".red());
                continue;
            }
        };

        let new_doc_id = match data
            .get(&model.create_mutation)
            .and_then(|v| v.as_str().or_else(|| v.get("id").and_then(|id| id.as_str())))
        {
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

        // Step 2: Push operations via pushUpdates on the drive endpoint
        if ops_count > 0 {
            match push_operations(&drive_client, &drive_id, &new_doc_id, doc_type, &contents.operations, quiet).await {
                Ok(pushed) => {
                    if !quiet {
                        println!("  Pushed: {pushed} operations");
                    }
                }
                Err(e) => {
                    println!("  {} Failed to push operations: {e}", "✗".red());
                    continue;
                }
            }
        } else if !quiet {
            println!("  No operations to push");
        }

        // Step 3: Verify state matches the .phd current-state
        tokio::time::sleep(std::time::Duration::from_millis(REQUEST_DELAY_MS)).await;
        if !quiet {
            match verify_state(&drive_client, &new_doc_id, &contents.current_state.global).await {
                Ok(true) => println!("  State:  {} EXACT MATCH", "✓".green()),
                Ok(false) => println!("  State:  {} MISMATCH (see diffs above)", "~".yellow()),
                Err(e) => println!("  State:  {} Could not verify: {e}", "~".yellow()),
            }
        }

        if !quiet {
            println!("  {} Imported", "✓".green());
        }
        success += 1;
    }

    if !quiet {
        println!("\n{} {success}/{} documents imported into drive '{drive}'", "✓".green(), files.len());
    }
    Ok(())
}

/// Push operations in batches via pushUpdates on the drive endpoint
async fn push_operations(
    drive_client: &GraphQLClient,
    drive_id: &str,
    doc_id: &str,
    doc_type: &str,
    operations: &PhdOperations,
    quiet: bool,
) -> Result<usize> {
    let ops = &operations.global;
    if ops.is_empty() {
        return Ok(0);
    }

    let total_batches = (ops.len() + PUSH_BATCH_SIZE - 1) / PUSH_BATCH_SIZE;
    let mut total_pushed = 0;

    for (batch_idx, chunk) in ops.chunks(PUSH_BATCH_SIZE).enumerate() {
        // Transform operations to the InputStrandUpdate format
        let input_ops: Vec<Value> = chunk.iter().map(|op| {
            // Handle both flat format (from our export) and nested action format (from reference .phd files)
            let (op_type, input, action_id) = if let Some(action) = op.get("action") {
                // Nested action format: { action: { type, input, id, ... } }
                let t = action.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let inp = match action.get("input") {
                    Some(v) => serde_json::to_string(v).unwrap_or_default(),
                    None => "{}".to_string(),
                };
                let aid = action.get("id").and_then(|v| v.as_str())
                    .or_else(|| op.get("id").and_then(|v| v.as_str()))
                    .unwrap_or("").to_string();
                (t, inp, aid)
            } else {
                // Flat format: { type, inputText, id, ... }
                let t = op.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let inp = op.get("inputText").and_then(|v| v.as_str()).unwrap_or("{}").to_string();
                let aid = op.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                (t, inp, aid)
            };

            serde_json::json!({
                "index": op.get("index").and_then(|v| v.as_u64()).unwrap_or(0),
                "skip": op.get("skip").and_then(|v| v.as_u64()).unwrap_or(0),
                "type": op_type,
                "id": op.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "actionId": action_id,
                "input": input,
                "hash": op.get("hash").and_then(|v| v.as_str()).unwrap_or(""),
                "timestampUtcMs": op.get("timestampUtcMs"),
                "error": op.get("error").cloned().unwrap_or(Value::Null),
            })
        }).collect();

        let strand = serde_json::json!({
            "driveId": drive_id,
            "documentId": doc_id,
            "documentType": doc_type,
            "scope": "global",
            "branch": "main",
            "operations": input_ops,
        });

        let variables = serde_json::json!({ "strands": [strand] });

        if batch_idx > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(REQUEST_DELAY_MS)).await;
        }

        let result = drive_client.query(
            r#"mutation ($strands: [InputStrandUpdate!]) {
                pushUpdates(strands: $strands) { revision status error }
            }"#,
            Some(&variables),
        ).await?;

        let update = result
            .get("pushUpdates")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first());

        if let Some(update) = update {
            let status = update.get("status").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
            if status != "SUCCESS" {
                let error = update.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
                bail!("pushUpdates failed at batch {}/{total_batches}: status={status}, error={error}", batch_idx + 1);
            }
            if !quiet {
                let revision = update.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("    [{}/{}] {} ops → revision {}", batch_idx + 1, total_batches, chunk.len(), revision);
            }
        }

        total_pushed += chunk.len();
    }

    Ok(total_pushed)
}

/// Verify the imported document's state matches the expected state from the .phd
async fn verify_state(
    drive_client: &GraphQLClient,
    doc_id: &str,
    expected_global: &Value,
) -> Result<bool> {
    let variables = serde_json::json!({ "id": doc_id });
    let data = drive_client.query(
        r#"query ($id: String!) { document(id: $id) { stateJSON } }"#,
        Some(&variables),
    ).await?;

    let actual = match data.pointer("/document/stateJSON") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Object(serde_json::Map::new())),
        Some(v) => v.clone(),
        None => Value::Object(serde_json::Map::new()),
    };

    let expected_str = serde_json::to_string(expected_global)?;
    let actual_str = serde_json::to_string(&actual)?;

    if expected_str == actual_str {
        return Ok(true);
    }

    // Report differences
    if let (Some(expected_map), Some(actual_map)) = (expected_global.as_object(), actual.as_object()) {
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
                    let ev_str = ev.map(|v| serde_json::to_string(v).unwrap_or_default()).unwrap_or_else(|| "undefined".to_string());
                    let av_str = av.map(|v| serde_json::to_string(v).unwrap_or_default()).unwrap_or_else(|| "undefined".to_string());
                    let ev_short = if ev_str.len() > 60 { &ev_str[..60] } else { &ev_str };
                    let av_short = if av_str.len() > 60 { &av_str[..60] } else { &av_str };
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
