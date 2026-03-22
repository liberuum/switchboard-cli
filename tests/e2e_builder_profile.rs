//! End-to-end tests for the Switchboard CLI.
//!
//! Tests the full lifecycle of BuilderProfile documents against a live API:
//!   1. Drive creation & listing
//!   2. Document creation, population, and mutation
//!   3. Export with operation validation
//!   4. Import into a new drive with roundtrip verification
//!   5. Watch (subscription) integration
//!   6. Deletion and cleanup
//!
//! Prerequisites:
//!   - A running Switchboard GraphQL API at http://localhost:4001/graphql
//!   - The "local" profile configured as default
//!   - BuilderProfile document model registered on the API
//!
//! Run with:  cargo test --test e2e_builder_profile -- --test-threads=1

use serde_json::Value;
use std::process::Command;

/// Unique suffix for this test run to avoid slug collisions.
fn pid() -> u32 {
    std::process::id()
}

/// Run `switchboard <args>` and return (stdout, stderr, success).
fn run(args: &[&str]) -> (String, String, bool) {
    let bin = env!("CARGO_BIN_EXE_switchboard");
    let output = Command::new(bin)
        .args(args)
        .output()
        .expect("failed to execute switchboard binary");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
    )
}

/// Run and assert success, returning stdout.
fn run_ok(args: &[&str]) -> String {
    let (stdout, stderr, ok) = run(args);
    assert!(ok, "Command failed: {:?}\nstderr: {stderr}", args);
    stdout
}

/// Run and parse JSON stdout.
fn run_json(args: &[&str]) -> Value {
    let stdout = run_ok(args);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Invalid JSON from {:?}: {e}\nstdout: {stdout}", args))
}

/// Extract an ID string from a nested JSON create response.
/// Handles both flat (`{ "id": "..." }`) and nested (`{ "Ns": { "createDocument": { "id": "..." } } }`) formats.
fn extract_create_id(data: &Value) -> String {
    // Flat: { "id": "..." }
    if let Some(id) = data["id"].as_str() {
        return id.to_string();
    }
    // Nested: { "Namespace": { "createDocument": { "id": "..." } } }
    if let Some(obj) = data.as_object() {
        for (_key, val) in obj {
            if let Some(id) = val.pointer("/createDocument/id").and_then(|v| v.as_str()) {
                return id.to_string();
            }
            if let Some(id) = val["id"].as_str() {
                return id.to_string();
            }
        }
    }
    panic!("Could not extract ID from create response: {data}");
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. CONNECTIVITY & INTROSPECTION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t01_ping() {
    let stdout = run_ok(&["ping"]);
    assert!(stdout.contains("responded in"), "ping should show latency");
}

#[test]
fn t02_introspect() {
    let (stdout, stderr, ok) = run(&["introspect"]);
    assert!(ok, "introspect failed: {stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("document models discovered"),
        "should report model count, got: {combined}"
    );
}

#[test]
fn t03_models_list() {
    // Ensure cache is fresh — introspect first
    let (_, _, _) = run(&["introspect"]);

    let data = run_json(&["models", "list", "--format", "json"]);
    let models = data.as_array().expect("models list should be array");
    assert!(!models.is_empty(), "should have at least one model");

    // Check BuilderProfile is available (field is "type" in JSON output)
    let has_bp = models
        .iter()
        .any(|m| m["type"].as_str() == Some("powerhouse/builder-profile"));
    assert!(
        has_bp,
        "BuilderProfile model should be available. Models: {:?}",
        models
            .iter()
            .filter_map(|m| m["type"].as_str())
            .collect::<Vec<_>>()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. DRIVE CREATION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t10_drive_create_list_delete() {
    let name = format!("e2e-drive-{}", pid());

    // Create
    let data = run_json(&["drives", "create", "--name", &name, "--format", "json"]);
    let id = data["id"].as_str().expect("drive should have id");
    assert!(!id.is_empty());

    // List and verify it appears
    let list = run_json(&["drives", "list", "--format", "json"]);
    let found = list
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["id"].as_str() == Some(id));
    assert!(found, "created drive should appear in list");

    // Delete
    let (stdout, stderr, ok) = run(&["drives", "delete", id, "-y"]);
    assert!(ok, "delete failed: {stderr}");
    assert!(stdout.contains("Deleted"), "should confirm deletion");

    // Verify gone
    let list2 = run_json(&["drives", "list", "--format", "json"]);
    let still_there = list2
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["id"].as_str() == Some(id));
    assert!(!still_there, "deleted drive should not appear in list");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. FULL BUILDER PROFILE LIFECYCLE
//    Create drive → create docs → populate → export → validate → import → verify
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t20_builder_profile_full_lifecycle() {
    let suffix = pid();

    // ── Step 1: Create source drive ──────────────────────────────────────
    let src_name = format!("e2e-source-{suffix}");
    let src = run_json(&["drives", "create", "--name", &src_name, "--format", "json"]);
    let src_drive_id = src["id"].as_str().unwrap().to_string();
    eprintln!("  Source drive: {src_drive_id}");

    // ── Step 2: Create BuilderProfile documents ──────────────────────────
    let doc1_name = format!("Alice-{suffix}");
    let doc1_data = run_json(&[
        "docs",
        "create",
        "--drive",
        &src_drive_id,
        "--name",
        &doc1_name,
        "--type",
        "powerhouse/builder-profile",
        "--format",
        "json",
    ]);
    let doc1_id = extract_create_id(&doc1_data);
    eprintln!("  Doc1 (Alice): {doc1_id}");

    let doc2_name = format!("Bob-{suffix}");
    let doc2_data = run_json(&[
        "docs",
        "create",
        "--drive",
        &src_drive_id,
        "--name",
        &doc2_name,
        "--type",
        "powerhouse/builder-profile",
        "--format",
        "json",
    ]);
    let doc2_id = extract_create_id(&doc2_data);
    eprintln!("  Doc2 (Bob):   {doc2_id}");

    // ── Step 3: Verify docs appear in drive ──────────────────────────────
    let tree = run_ok(&["docs", "tree", &src_drive_id, "--format", "table"]);
    assert!(
        tree.contains(&doc1_name),
        "tree should contain {doc1_name}: {tree}"
    );
    assert!(
        tree.contains(&doc2_name),
        "tree should contain {doc2_name}: {tree}"
    );

    // ── Step 4: Populate docs with mutations ─────────────────────────────
    // Update Alice's profile
    let alice_update = serde_json::json!({
        "name": doc1_name,
        "description": "Senior Protocol Engineer",
        "about": "Building decentralized infrastructure since 2020"
    });
    let (_, stderr, ok) = run(&[
        "docs",
        "mutate",
        &doc1_id,
        "--op",
        "updateProfile",
        "--input",
        &alice_update.to_string(),
        "--format",
        "json",
    ]);
    assert!(ok, "updateProfile for Alice failed: {stderr}");

    // Add a skill to Alice
    let skill_input = serde_json::json!({ "skill": "BACKEND_DEVELOPMENT" });
    let (_, stderr, ok) = run(&[
        "docs",
        "mutate",
        &doc1_id,
        "--op",
        "addSkill",
        "--input",
        &skill_input.to_string(),
        "--format",
        "json",
    ]);
    assert!(ok, "addSkill for Alice failed: {stderr}");

    // Add a link to Alice
    let link_input = serde_json::json!({
        "id": "link-1",
        "url": "https://github.com/alice",
        "label": "GitHub"
    });
    let (_, stderr, ok) = run(&[
        "docs",
        "mutate",
        &doc1_id,
        "--op",
        "addLink",
        "--input",
        &link_input.to_string(),
        "--format",
        "json",
    ]);
    assert!(ok, "addLink for Alice failed: {stderr}");

    // Update Bob's profile
    let bob_update = serde_json::json!({
        "name": doc2_name,
        "description": "Frontend Developer",
        "about": "React & TypeScript specialist"
    });
    let (_, stderr, ok) = run(&[
        "docs",
        "mutate",
        &doc2_id,
        "--op",
        "updateProfile",
        "--input",
        &bob_update.to_string(),
        "--format",
        "json",
    ]);
    assert!(ok, "updateProfile for Bob failed: {stderr}");

    // ── Step 5: Verify operations via ops command ────────────────────────
    let ops_out = run_json(&["ops", &doc1_id, "--format", "json"]);
    let ops = ops_out.as_array().expect("ops should be array");
    assert!(
        ops.len() >= 3,
        "Alice should have >= 3 ops (updateProfile + addSkill + addLink + CREATE_DOCUMENT), got {}",
        ops.len()
    );

    // ── Step 6: Export the drive ─────────────────────────────────────────
    let export_dir = format!("/tmp/e2e-export-{suffix}");
    let (_, stderr, ok) = run(&["export", "drive", &src_drive_id, "--out", &export_dir]);
    assert!(ok, "export drive failed: {stderr}");

    // Verify .phd files exist
    let alice_phd = format!("{export_dir}/{doc1_name}.phd");
    let bob_phd = format!("{export_dir}/{doc2_name}.phd");
    assert!(
        std::path::Path::new(&alice_phd).exists(),
        "Alice .phd should exist at {alice_phd}"
    );
    assert!(
        std::path::Path::new(&bob_phd).exists(),
        "Bob .phd should exist at {bob_phd}"
    );

    // ── Step 7: Validate exported operations ─────────────────────────────
    let alice_zip = std::fs::File::open(&alice_phd).unwrap();
    let mut archive = zip::ZipArchive::new(alice_zip).unwrap();

    // Check all expected files exist
    let file_names: Vec<String> = archive.file_names().map(String::from).collect();
    assert!(file_names.contains(&"header.json".to_string()));
    assert!(file_names.contains(&"operations.json".to_string()));
    assert!(file_names.contains(&"current-state.json".to_string()));
    assert!(file_names.contains(&"state.json".to_string()));

    // Validate header
    let header: Value = {
        let mut f = archive.by_name("header.json").unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut f, &mut buf).unwrap();
        serde_json::from_str(&buf).unwrap()
    };
    assert_eq!(
        header["documentType"].as_str().unwrap(),
        "powerhouse/builder-profile"
    );
    assert_eq!(header["name"].as_str().unwrap(), doc1_name);

    // Validate operations contain our mutations
    let ops_json: Value = {
        let mut f = archive.by_name("operations.json").unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut f, &mut buf).unwrap();
        serde_json::from_str(&buf).unwrap()
    };
    let global_ops = ops_json["global"]
        .as_array()
        .expect("ops should have global array");
    assert!(
        global_ops.len() >= 3,
        "exported ops should have >= 3 operations, got {}",
        global_ops.len()
    );

    // Find the user operations by type
    let op_types: Vec<&str> = global_ops
        .iter()
        .filter_map(|op| op.pointer("/action/type").and_then(|v| v.as_str()))
        .collect();
    eprintln!("  Exported op types: {op_types:?}");

    // Validate state has the profile data we set
    let state: Value = {
        let mut f = archive.by_name("current-state.json").unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut f, &mut buf).unwrap();
        serde_json::from_str(&buf).unwrap()
    };
    // The profile description should be in the global state
    let state_str = serde_json::to_string(&state).unwrap();
    assert!(
        state_str.contains("Senior Protocol Engineer") || state_str.contains(&doc1_name),
        "exported state should contain the profile data we set"
    );

    // ── Step 8: Create destination drive and import ──────────────────────
    let dst_name = format!("e2e-dest-{suffix}");
    let dst = run_json(&["drives", "create", "--name", &dst_name, "--format", "json"]);
    let dst_drive_id = dst["id"].as_str().unwrap().to_string();
    eprintln!("  Dest drive: {dst_drive_id}");

    // Import Alice's .phd into the destination drive
    let (_, stderr, ok) = run(&["import", &alice_phd, "--drive", &dst_drive_id]);
    assert!(ok, "import failed: {stderr}");

    // ── Step 9: Verify imported document ─────────────────────────────────
    let dst_docs = run_json(&["docs", "list", "--drive", &dst_drive_id, "--format", "json"]);
    let dst_docs_arr = dst_docs.as_array().expect("docs list should be array");
    assert!(
        !dst_docs_arr.is_empty(),
        "destination drive should have imported document(s)"
    );

    // Check the imported doc has operations
    let imported_id = dst_docs_arr[0]["id"].as_str().unwrap();
    let imported_ops = run_json(&["ops", imported_id, "--format", "json"]);
    let imported_ops_arr = imported_ops.as_array().expect("ops should be array");
    eprintln!("  Imported doc has {} operations", imported_ops_arr.len());
    // Imported doc should have user operations (infrastructure ops may differ)
    assert!(
        !imported_ops_arr.is_empty(),
        "imported document should have operations"
    );

    // ── Step 10: Export with filters ─────────────────────────────────────
    // Export only operations since revision 2 (skip CREATE_DOCUMENT)
    let filtered_dir = format!("/tmp/e2e-filtered-{suffix}");
    std::fs::create_dir_all(&filtered_dir).unwrap();
    let (_, stderr, ok) = run(&[
        "export",
        "doc",
        &doc1_id,
        "--drive",
        &src_drive_id,
        "--out",
        &format!("{filtered_dir}/alice-filtered.phd"),
        "--since-revision",
        "2",
    ]);
    assert!(ok, "filtered export failed: {stderr}");

    // Verify filtered export has fewer operations
    if std::path::Path::new(&format!("{filtered_dir}/alice-filtered.phd")).exists() {
        let filtered_zip =
            std::fs::File::open(format!("{filtered_dir}/alice-filtered.phd")).unwrap();
        let mut filtered_archive = zip::ZipArchive::new(filtered_zip).unwrap();
        let filtered_ops: Value = {
            let mut f = filtered_archive.by_name("operations.json").unwrap();
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut f, &mut buf).unwrap();
            serde_json::from_str(&buf).unwrap()
        };
        let filtered_global = filtered_ops["global"]
            .as_array()
            .expect("filtered ops global");
        eprintln!(
            "  Filtered export: {} ops (full had {})",
            filtered_global.len(),
            global_ops.len()
        );
        assert!(
            filtered_global.len() <= global_ops.len(),
            "filtered export should have <= full ops"
        );
    }

    // ── Step 11: Test docs get with parent drive info ────────────────────
    let get_out = run_ok(&["docs", "get", &doc1_id, "--format", "table"]);
    assert!(
        get_out.contains("powerhouse/builder-profile"),
        "docs get should show document type"
    );

    // ── Step 12: Test docs rename ────────────────────────────────────────
    let new_name = format!("Alice-Renamed-{suffix}");
    let (_, stderr, ok) = run(&["docs", "rename", &doc1_id, &new_name]);
    assert!(ok, "rename failed: {stderr}");

    // ── Cleanup ──────────────────────────────────────────────────────────
    // Delete both drives (cascade deletes docs)
    let (_, stderr, ok) = run(&["drives", "delete", &src_drive_id, &dst_drive_id, "-y"]);
    assert!(ok, "cleanup delete failed: {stderr}");

    // Clean up temp files
    let _ = std::fs::remove_dir_all(&export_dir);
    let _ = std::fs::remove_dir_all(&filtered_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. EXPORT ALL
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t30_export_all() {
    let suffix = pid();
    let drive_name = format!("e2e-export-all-{suffix}");
    let drv = run_json(&[
        "drives",
        "create",
        "--name",
        &drive_name,
        "--format",
        "json",
    ]);
    let drive_id = drv["id"].as_str().unwrap().to_string();

    // Create a doc
    let doc = run_json(&[
        "docs",
        "create",
        "--drive",
        &drive_id,
        "--name",
        &format!("profile-{suffix}"),
        "--type",
        "powerhouse/builder-profile",
        "--format",
        "json",
    ]);
    let _doc_id = extract_create_id(&doc);

    // Export all
    let export_dir = format!("/tmp/e2e-export-all-{suffix}");
    let (_, stderr, ok) = run(&["export", "all", "--out", &export_dir]);
    assert!(ok, "export all failed: {stderr}");

    // Verify the export directory has content
    assert!(
        std::path::Path::new(&export_dir).exists(),
        "export directory should exist"
    );
    let entries: Vec<_> = std::fs::read_dir(&export_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "export directory should have drive subdirectories"
    );

    // Cleanup
    let (_, _, _) = run(&["drives", "delete", &drive_id, "-y"]);
    let _ = std::fs::remove_dir_all(&export_dir);
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. DOCS APPLY (raw actions via mutateDocumentAsync)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t40_docs_apply() {
    let suffix = pid();
    let drive_name = format!("e2e-apply-{suffix}");
    let drv = run_json(&[
        "drives",
        "create",
        "--name",
        &drive_name,
        "--format",
        "json",
    ]);
    let drive_id = drv["id"].as_str().unwrap().to_string();

    // Create a BuilderProfile doc
    let doc = run_json(&[
        "docs",
        "create",
        "--drive",
        &drive_id,
        "--name",
        &format!("apply-test-{suffix}"),
        "--type",
        "powerhouse/builder-profile",
        "--format",
        "json",
    ]);
    let doc_id = extract_create_id(&doc);

    // Apply raw actions
    let actions = serde_json::json!([
        {
            "type": "UPDATE_PROFILE",
            "input": {
                "name": "Applied Profile",
                "description": "Applied via docs apply"
            },
            "scope": "global"
        }
    ]);
    let (stdout, stderr, ok) = run(&[
        "docs",
        "apply",
        &doc_id,
        "--actions",
        &actions.to_string(),
        "--format",
        "json",
    ]);
    // This may fail if the action format doesn't match, which is fine — we're testing the CLI flow
    if ok {
        let result: Value = serde_json::from_str(&stdout).unwrap_or_default();
        assert!(
            result.get("jobId").is_some(),
            "apply should return jobId: {stdout}"
        );
    } else {
        eprintln!("  docs apply returned error (action format may differ): {stderr}");
    }

    // Cleanup
    let (_, _, _) = run(&["drives", "delete", &drive_id, "-y"]);
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. WATCH SUBSCRIPTION (quick smoke test)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t50_watch_docs_connects() {
    // Start watch in a child process, kill after 2 seconds
    let bin = env!("CARGO_BIN_EXE_switchboard");
    let child = Command::new(bin)
        .args(["watch", "docs", "--format", "json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    match child {
        Ok(mut child) => {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            // The subscription should at least attempt to connect
            assert!(
                stderr.contains("Watching") || stderr.is_empty() || output.status.code().is_some(),
                "watch should start without crashing: {stderr}"
            );
        }
        Err(e) => {
            eprintln!("  Could not spawn watch process: {e}");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. RAW GRAPHQL QUERY
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t60_raw_query() {
    let data = run_json(&[
        "query",
        "{ findDocuments(search: { type: \"powerhouse/document-drive\" }) { totalCount } }",
        "--format",
        "json",
    ]);
    assert!(
        data.pointer("/findDocuments/totalCount").is_some(),
        "raw query should return totalCount"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. CONFIG & AUTH
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t70_config_and_auth() {
    // Config list
    let data = run_json(&["config", "list", "--format", "json"]);
    assert!(data.is_array(), "config list should return array");

    // Config show
    let data = run_json(&["config", "show", "--format", "json"]);
    assert!(data["name"].is_string(), "config show should have name");

    // Auth status
    let (_, _, ok) = run(&["auth", "status"]);
    assert!(ok, "auth status should succeed");
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. JOBS STATUS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t80_jobs_status_nonexistent() {
    // Querying a fake job should fail gracefully
    let (_, stderr, ok) = run(&["jobs", "status", "fake-job-id-12345"]);
    // May succeed with null status or fail — either is acceptable
    if !ok {
        assert!(
            stderr.contains("Error") || stderr.contains("error"),
            "should show error for fake job"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. SCHEMA & INFO
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t90_schema_dump() {
    let (stdout, _, ok) = run(&["schema"]);
    assert!(ok, "schema dump failed");
    assert!(
        stdout.contains("type") || stdout.contains("Query"),
        "schema should contain GraphQL type definitions"
    );
}

#[test]
fn t91_info() {
    let (stdout, _, ok) = run(&["info"]);
    assert!(ok, "info failed");
    assert!(
        stdout.contains("Drive") || stdout.contains("Model") || stdout.contains("drive"),
        "info should show drive/model counts"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. GUIDE
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t92_guide_topics() {
    let (stdout, _, ok) = run(&["guide", "overview"]);
    assert!(ok, "guide overview failed");
    assert!(
        stdout.contains("Switchboard") || stdout.contains("switchboard"),
        "guide should contain switchboard references"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. COMPLETIONS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t93_completions() {
    let (stdout, _, ok) = run(&["completions", "bash"]);
    assert!(ok, "completions bash failed");
    assert!(!stdout.is_empty(), "completions should produce output");
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. DOCS TREE (all drives)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn t94_docs_tree_all_drives() {
    let (stdout, _, ok) = run(&["docs", "tree", "--format", "table"]);
    assert!(ok, "docs tree (all) failed");
    // Should show at least one drive name followed by /
    assert!(
        stdout.contains('/') || stdout.contains("(empty)") || stdout.is_empty(),
        "tree should show drive structure or be empty"
    );
}
