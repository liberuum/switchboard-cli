use anyhow::{Result, bail};
use clap::Subcommand;
use colored::Colorize;
use serde_json::Value;
use std::path::Path;

use crate::cli::helpers;
use crate::graphql::GraphQLClient;
use crate::output::OutputFormat;
use crate::phd::{self, PhdHeader, PhdOperations, PhdState};

#[derive(Subcommand)]
pub enum ExportCommand {
    /// Export everything: all drives and their documents
    All {
        /// Output directory (defaults to ./switchboard-export/)
        #[arg(long, short)]
        out: Option<String>,
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

pub async fn run_export(
    cmd: ExportCommand,
    _format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    match cmd {
        ExportCommand::All { out } => export_all(out.as_deref(), profile_name, quiet).await,
        ExportCommand::Doc { doc_id, drive, out } => {
            export_doc(&doc_id, &drive, out.as_deref(), profile_name, quiet).await
        }
        ExportCommand::Drive { drive, out } => {
            export_drive(&drive, out.as_deref(), profile_name, quiet).await
        }
    }
}

/// Build the proper PhdHeader matching the reference download-drive-documents.ts format
fn build_header(doc: &Value) -> PhdHeader {
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
    }
}

/// Build the PhdState wrapping state under the `global` key
fn build_current_state(state: Value) -> PhdState {
    PhdState {
        global: state,
        ..PhdState::default()
    }
}

async fn export_all(out_dir: Option<&str>, profile_name: Option<&str>, quiet: bool) -> Result<()> {
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

            match fetch_document(&client, file_id).await {
                Ok((doc, operations)) => {
                    let header = build_header(&doc);
                    let state = extract_state(&doc);
                    let phd_ops = PhdOperations {
                        global: operations.clone(),
                    };
                    let initial_state = PhdState::default();
                    let current_state = build_current_state(state);

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
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, _cache) = helpers::setup_with_cache(profile_name)?;

    // Resolve document: if drive is provided, use "drive/doc_id" format
    let identifier = format!("{drive}/{doc_id}");
    let resolved_id = helpers::resolve_doc(&client, &identifier).await?;

    let (doc, operations) = fetch_document(&client, &resolved_id).await?;

    let header = build_header(&doc);
    let state = extract_state(&doc);
    let phd_ops = PhdOperations {
        global: operations.clone(),
    };
    let initial_state = PhdState::default();
    let current_state = build_current_state(state);

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

    for (i, file_node) in files.iter().enumerate() {
        let file_id = file_node["id"].as_str().unwrap_or("");
        let file_name = file_node["name"].as_str().unwrap_or("document");
        let file_type = file_node["documentType"].as_str().unwrap_or("unknown");

        match fetch_document(&client, file_id).await {
            Ok((doc, operations)) => {
                let state = extract_state(&doc);

                let header = build_header(&doc);
                let phd_ops = PhdOperations {
                    global: operations.clone(),
                };
                let initial_state = PhdState::default();
                let current_state = build_current_state(state);

                let safe_file = sanitize_filename(file_name);
                let file_path = dir.join(format!("{safe_file}.phd"));

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

const OP_BATCH_SIZE: usize = 100;
const REQUEST_DELAY_MS: u64 = 200;

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
async fn fetch_document(client: &GraphQLClient, doc_id: &str) -> Result<(Value, Vec<Value>)> {
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

    // Fetch operations with pagination
    let mut all_ops: Vec<Value> = Vec::new();
    loop {
        let offset = all_ops.len();
        let ops_query = format!(
            r#"{{ documentOperations(filter: {{ documentId: "{escaped}" }}, paging: {{ limit: {OP_BATCH_SIZE}, offset: {offset} }}) {{ items {{ id index action {{ type input scope }} timestampUtcMs hash skip error }} totalCount }} }}"#,
        );

        if !all_ops.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(REQUEST_DELAY_MS)).await;
        }

        let ops_data = client.query(&ops_query, None).await?;
        let batch = ops_data
            .pointer("/documentOperations/items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if batch.is_empty() {
            break;
        }

        let batch_len = batch.len();
        all_ops.extend(batch);

        // If we got fewer than the batch size, we're done
        if batch_len < OP_BATCH_SIZE {
            break;
        }
    }

    // Transform operations to the nested action format expected by .phd
    let transformed_ops: Vec<Value> = all_ops
        .iter()
        .map(|op| {
            let action = &op["action"];
            let input = action
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let scope = action["scope"].as_str().unwrap_or("global");

            serde_json::json!({
                "id": op.get("id"),
                "index": op.get("index"),
                "skip": op.get("skip").and_then(|v| v.as_u64()).unwrap_or(0),
                "hash": op.get("hash"),
                "timestampUtcMs": op.get("timestampUtcMs"),
                "error": op.get("error").cloned().unwrap_or(Value::Null),
                "action": {
                    "id": op.get("id"),
                    "type": action.get("type"),
                    "timestampUtcMs": op.get("timestampUtcMs"),
                    "input": input,
                    "scope": scope,
                }
            })
        })
        .collect();

    Ok((doc, transformed_ops))
}

/// Extract state from a document value. In the new API, state is a JSONObject directly.
fn extract_state(doc: &Value) -> Value {
    doc.get("state")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()))
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
    _format: OutputFormat,
    profile_name: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let (_name, _profile, client, cache) = helpers::setup_with_cache(profile_name)?;

    if files.is_empty() {
        bail!("No .phd files specified");
    }

    // Resolve the drive identifier
    let drive_id = helpers::resolve_doc(&client, &drive).await?;

    if !quiet {
        println!(
            "  Importing {} file(s) into drive '{drive}'...",
            files.len()
        );
    }

    let mut success = 0;

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
                println!(
                    "  {} No matching model found for type '{doc_type}'",
                    "✗".red()
                );
                continue;
            }
        };

        // Step 1: Create the document via model-specific mutation
        let mutation = format!(
            r#"mutation {{ {create}(name: "{name}", parentIdentifier: "{drive_id}") {{ id }} }}"#,
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

        let new_doc_id = match data.get(&model.create_mutation).and_then(|v| {
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

        // Step 2: Push operations via model-specific mutations
        if ops_count > 0 {
            match push_operations_via_mutate(&client, &new_doc_id, &contents.operations, model, quiet)
                .await
            {
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
            match verify_state(&client, &new_doc_id, &contents.current_state.global).await {
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
        println!(
            "\n{} {success}/{} documents imported into drive '{drive}'",
            "✓".green(),
            files.len()
        );
    }
    Ok(())
}

/// Push operations via model-specific mutations (e.g. DocumentModel_setModelName).
/// Skips document-scope ops (CREATE_DOCUMENT, UPGRADE_DOCUMENT) since the doc
/// was already created. Converts SCREAMING_SNAKE op types to camelCase and
/// looks them up in the introspection cache for proper typed mutations.
async fn push_operations_via_mutate(
    client: &GraphQLClient,
    doc_id: &str,
    operations: &PhdOperations,
    model: &crate::graphql::introspection::DocumentModel,
    quiet: bool,
) -> Result<usize> {
    let mut total_pushed = 0;

    for op in &operations.global {
        let (op_type, input, scope) = if let Some(action) = op.get("action") {
            let t = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let i = action
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let s = action["scope"].as_str().unwrap_or("global").to_string();
            (t.to_string(), i, s)
        } else {
            let t = op.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let input_text = op.get("inputText").and_then(|v| v.as_str()).unwrap_or("{}");
            let i: Value = serde_json::from_str(input_text)
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let s = op
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("global")
                .to_string();
            (t.to_string(), i, s)
        };

        // Skip document-scope operations — internal lifecycle ops
        if scope == "document" {
            continue;
        }

        // Convert SCREAMING_SNAKE (e.g. SET_MODEL_NAME) to camelCase (e.g. setModelName)
        let camel_name = screaming_snake_to_camel(&op_type);

        // Find the matching operation in the model's introspection cache
        let model_op = match model
            .operations
            .iter()
            .find(|o| o.operation == camel_name)
        {
            Some(op) => op,
            None => {
                if !quiet {
                    println!("    ⚠ {op_type}: no matching mutation found (tried {camel_name})");
                }
                continue;
            }
        };

        // Build the mutation using the model-specific typed mutation
        let has_input_arg = model_op.args.iter().any(|a| a.name == "input");

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

            let query = format!(
                "mutation($docId: PHID!, $input: {input_type}{bang}) {{ {name}(docId: $docId, input: $input) {{ id }} }}",
                name = model_op.full_name,
            );
            let vars = serde_json::json!({
                "docId": doc_id,
                "input": input,
            });
            (query, vars)
        } else {
            // Direct args — pass input fields as top-level variables
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

            let query = format!(
                "mutation({decls}) {{ {name}({args}) {{ id }} }}",
                decls = var_decls.join(", "),
                name = model_op.full_name,
                args = arg_refs.join(", "),
            );
            (query, Value::Object(vars_map))
        };

        if total_pushed > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(REQUEST_DELAY_MS)).await;
        }

        match client.query(&mutation, Some(&vars)).await {
            Ok(_) => {
                total_pushed += 1;
            }
            Err(e) => {
                let err_str = format!("{e}");
                if !quiet {
                    println!("    ⚠ {op_type}: {err_str}");
                }
                // Continue with remaining ops
            }
        }
    }

    Ok(total_pushed)
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
        .pointer("/document/document/state")
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
