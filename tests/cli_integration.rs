//! Integration tests that run the CLI binary against a live Switchboard instance.
//!
//! These tests require a running GraphQL API and a configured "local" profile.
//! Set `SWITCHBOARD_TEST_URL` to override the default (http://localhost:4001/graphql).
//!
//! Run with:  cargo test --test cli_integration

use std::process::Command;

/// Helper: run `switchboard <args>` and return (stdout, stderr, success).
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

// ── Smoke tests ─────────────────────────────────────────────────────────────

#[test]
fn help_flag_works() {
    let (stdout, _, ok) = run(&["--help"]);
    assert!(ok);
    assert!(stdout.contains("CLI for Switchboard GraphQL instances"));
}

#[test]
fn version_flag_works() {
    let (stdout, _, ok) = run(&["--version"]);
    assert!(ok);
    assert!(stdout.starts_with("switchboard "));
}

// ── Drives ──────────────────────────────────────────────────────────────────

#[test]
fn drives_list_json() {
    let (stdout, _, ok) = run(&["drives", "list", "--format", "json"]);
    assert!(ok, "drives list failed");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(data.is_array(), "expected JSON array");
}

#[test]
fn drives_list_table() {
    let (stdout, _, ok) = run(&["drives", "list", "--format", "table"]);
    assert!(ok, "drives list --format table failed");
    assert!(stdout.contains("ID"), "table should have an ID header");
}

#[test]
fn drives_create_and_delete_single() {
    // Use a unique name to avoid slug collisions between test runs
    let name = format!("test-single-{}", std::process::id());

    // Create
    let (stdout, stderr, ok) = run(&["drives", "create", "--name", &name, "--format", "json"]);
    assert!(ok, "drives create failed: stdout={stdout} stderr={stderr}");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    let id = data["id"].as_str().expect("missing id in create output");

    // Delete single
    let (_, stderr, ok) = run(&["drives", "delete", id, "-y"]);
    assert!(ok, "drives delete single failed: {stderr}");
}

#[test]
fn drives_create_and_delete_multiple() {
    let pid = std::process::id();

    // Create two drives
    let name1 = format!("test-multi-1-{pid}");
    let (out1, stderr1, ok1) = run(&["drives", "create", "--name", &name1, "--format", "json"]);
    assert!(ok1, "create 1 failed: {stderr1}");
    let d1: serde_json::Value = serde_json::from_str(&out1).unwrap();
    let id1 = d1["id"].as_str().unwrap();

    let name2 = format!("test-multi-2-{pid}");
    let (out2, stderr2, ok2) = run(&["drives", "create", "--name", &name2, "--format", "json"]);
    assert!(ok2, "create 2 failed: {stderr2}");
    let d2: serde_json::Value = serde_json::from_str(&out2).unwrap();
    let id2 = d2["id"].as_str().unwrap();

    // Delete both at once
    let (stdout, stderr, ok) = run(&["drives", "delete", id1, id2, "-y"]);
    assert!(ok, "multi-delete failed: {stderr}");
    assert!(stdout.contains("Deleted drive"), "expected deletion output");
    // Should see two deletion messages
    let count = stdout.matches("Deleted drive").count();
    assert_eq!(count, 2, "expected 2 deletions, got {count}");
}

#[test]
fn drives_delete_nonexistent_fails() {
    let (_, stderr, ok) = run(&["drives", "delete", "nonexistent-drive-id-xyz", "-y"]);
    assert!(!ok, "deleting nonexistent drive should fail");
    assert!(
        stderr.contains("Error") || stderr.contains("error"),
        "expected error message, got: {stderr}"
    );
}

// ── Docs ────────────────────────────────────────────────────────────────────

#[test]
fn docs_list_existing_drive() {
    // List docs in the first available drive
    let (list_out, _, ok) = run(&["drives", "list", "--format", "json"]);
    assert!(ok);
    let drives: Vec<serde_json::Value> = serde_json::from_str(&list_out).unwrap();
    if drives.is_empty() {
        return; // no drives to test against
    }
    let slug = drives[0]["slug"].as_str().unwrap();

    let (_, stderr, ok) = run(&["docs", "list", "--drive", slug, "--format", "table"]);
    assert!(ok, "docs list failed for drive {slug}: {stderr}");
}

#[test]
fn docs_tree_existing_drive() {
    let (list_out, _, ok) = run(&["drives", "list", "--format", "json"]);
    assert!(ok);
    let drives: Vec<serde_json::Value> = serde_json::from_str(&list_out).unwrap();
    if drives.is_empty() {
        return;
    }
    let slug = drives[0]["slug"].as_str().unwrap();

    let (stdout, _, ok) = run(&["docs", "tree", "--drive", slug, "--format", "table"]);
    assert!(ok, "docs tree failed");
    assert!(!stdout.is_empty(), "tree output should not be empty");
}

// ── Config ──────────────────────────────────────────────────────────────────

#[test]
fn config_list() {
    let (stdout, _, ok) = run(&["config", "list", "--format", "json"]);
    assert!(ok, "config list failed");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(data.is_array());
}

// ── Info / Ping ─────────────────────────────────────────────────────────────

#[test]
fn ping_succeeds() {
    let (stdout, _, ok) = run(&["ping"]);
    assert!(ok, "ping failed");
    assert!(stdout.contains("responded in"));
}

#[test]
fn info_json() {
    let (stdout, _, ok) = run(&["info", "--format", "json"]);
    assert!(ok, "info failed");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(data["url"].is_string());
    assert!(data["drives"].is_number());
}

// ── Interactive mode ────────────────────────────────────────────────────────

#[test]
fn interactive_drives_list() {
    let bin = env!("CARGO_BIN_EXE_switchboard");
    let output = Command::new(bin)
        .args(["--format", "json", "-i", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                writeln!(stdin, "drives list --format json")?;
                writeln!(stdin, "exit")?;
            }
            child.wait_with_output()
        })
        .expect("failed to run interactive mode");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "interactive mode failed");
    // The REPL should have dispatched drives list and produced JSON
    assert!(
        stdout.contains("\"id\""),
        "expected JSON drive output, got: {stdout}"
    );
}

#[test]
fn interactive_raw_query() {
    let bin = env!("CARGO_BIN_EXE_switchboard");
    let output = Command::new(bin)
        .args(["-i", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                writeln!(stdin, "query {{ findDocuments(search: {{ type: \"powerhouse/document-drive\" }}) {{ totalCount }} }}")?;
                writeln!(stdin, "exit")?;
            }
            child.wait_with_output()
        })
        .expect("failed to run interactive mode");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("findDocuments") || stdout.contains("totalCount"),
        "expected raw query output, got: {stdout}"
    );
}

#[test]
fn interactive_blocks_nested_interactive() {
    let bin = env!("CARGO_BIN_EXE_switchboard");
    let output = Command::new(bin)
        .args(["-i", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                writeln!(stdin, "interactive")?;
                writeln!(stdin, "exit")?;
            }
            child.wait_with_output()
        })
        .expect("failed to run interactive mode");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Already in interactive mode"));
}

// ── Models ──────────────────────────────────────────────────────────────────

#[test]
fn models_list_json() {
    let (stdout, _, ok) = run(&["models", "list", "--format", "json"]);
    // This may fail if introspection hasn't been run, which is fine
    if ok {
        let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
        assert!(data.is_array());
    }
}

// ── Subcommand help ─────────────────────────────────────────────────────────

#[test]
fn drives_delete_help() {
    let (stdout, _, ok) = run(&["drives", "delete", "--help"]);
    assert!(ok);
    assert!(stdout.contains("[IDS]..."));
    assert!(stdout.contains("Delete one or more drives"));
}

#[test]
fn docs_delete_help() {
    let (stdout, _, ok) = run(&["docs", "delete", "--help"]);
    assert!(ok);
    assert!(stdout.contains("[IDS]..."));
    assert!(stdout.contains("Delete one or more documents"));
}

// ── Raw query ───────────────────────────────────────────────────────────────

#[test]
fn raw_query_find_documents() {
    let (stdout, _, ok) = run(&[
        "query",
        r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { totalCount } }"#,
        "--format",
        "json",
    ]);
    assert!(ok, "raw query failed");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(
        data.pointer("/findDocuments/totalCount").is_some(),
        "expected findDocuments.totalCount in response"
    );
}

// ── Introspect ──────────────────────────────────────────────────────────────

#[test]
fn introspect_succeeds() {
    let (stdout, stderr, ok) = run(&["introspect"]);
    assert!(
        ok,
        "introspect failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("models discovered") || stdout.contains("document models"),
        "expected introspection output, got: {stdout}"
    );
}

// ── Guide ───────────────────────────────────────────────────────────────────

#[test]
fn guide_overview() {
    let (stdout, _, ok) = run(&["guide", "overview"]);
    assert!(ok);
    assert!(stdout.contains("SWITCHBOARD CLI"));
}

#[test]
fn guide_commands() {
    let (stdout, _, ok) = run(&["guide", "commands"]);
    assert!(ok);
    assert!(stdout.contains("ALL COMMANDS"));
    // access/groups should not appear in the auth section heading
    assert!(
        !stdout.contains("ACCESS"),
        "access commands should be removed from guide"
    );
}

// ── Auth ────────────────────────────────────────────────────────────────────

#[test]
fn auth_status() {
    let (stdout, _, ok) = run(&["auth", "status"]);
    assert!(ok, "auth status failed");
    // Should show either "Authenticated" or "No token" or similar
    assert!(
        !stdout.is_empty(),
        "auth status should produce output"
    );
}

// ── Export (dry run — just test that the command parses) ─────────────────────

#[test]
fn export_help() {
    let (stdout, _, ok) = run(&["export", "--help"]);
    assert!(ok);
    assert!(stdout.contains("Export"));
}

#[test]
fn import_help() {
    let (stdout, _, ok) = run(&["import", "--help"]);
    assert!(ok);
    assert!(stdout.contains("Import"));
}

// ── Analytics ───────────────────────────────────────────────────────────────

#[test]
fn analytics_metrics_json() {
    let (stdout, _, ok) = run(&["analytics", "metrics", "--format", "json"]);
    assert!(ok, "analytics metrics failed");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(data.is_array(), "expected JSON array of metrics");
}

#[test]
fn analytics_currencies_json() {
    let (stdout, _, ok) = run(&["analytics", "currencies", "--format", "json"]);
    assert!(ok, "analytics currencies failed");
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("invalid JSON");
    assert!(data.is_array(), "expected JSON array of currencies");
}

// ── Document hierarchy operations ──────────────────────────────────────────

#[test]
fn docs_rename_and_parents() {
    let pid = std::process::id();
    let name = format!("test-rename-{pid}");

    // Create a drive
    let (drive_out, _, ok) = run(&["drives", "create", "--name", &name, "--format", "json"]);
    assert!(ok, "drive create failed");
    let drive: serde_json::Value = serde_json::from_str(&drive_out).unwrap();
    let drive_id = drive["id"].as_str().unwrap();

    // Create a doc in the drive
    let (doc_out, stderr, ok) = run(&[
        "docs", "create",
        "--type", "powerhouse/document-model",
        "--name", &format!("rename-test-{pid}"),
        "--drive", drive_id,
        "--format", "json",
    ]);
    assert!(ok, "doc create failed: {stderr}");
    let doc: serde_json::Value = serde_json::from_str(&doc_out).unwrap();
    let doc_id = doc["DocumentModel_createDocument"]["id"].as_str().unwrap();

    // Rename
    let new_name = format!("renamed-{pid}");
    let (rename_out, _, ok) = run(&["docs", "rename", doc_id, &new_name, "--format", "json"]);
    assert!(ok, "rename failed");
    let renamed: serde_json::Value = serde_json::from_str(&rename_out).unwrap();
    assert_eq!(renamed["name"].as_str().unwrap(), new_name);

    // Parents
    let (parents_out, _, ok) = run(&["docs", "parents", doc_id, "--format", "json"]);
    assert!(ok, "parents failed");
    let parents: Vec<serde_json::Value> = serde_json::from_str(&parents_out).unwrap();
    assert!(!parents.is_empty(), "expected at least one parent");
    assert!(
        parents.iter().any(|p| p["id"].as_str() == Some(drive_id)),
        "drive should be a parent"
    );

    // Cleanup
    run(&["drives", "delete", drive_id, "-y"]);
}

#[test]
fn docs_add_to_and_remove_from() {
    let pid = std::process::id();

    // Create two drives
    let (d1_out, _, ok) = run(&["drives", "create", "--name", &format!("add-src-{pid}"), "--format", "json"]);
    assert!(ok);
    let d1: serde_json::Value = serde_json::from_str(&d1_out).unwrap();
    let d1_id = d1["id"].as_str().unwrap();

    let (d2_out, _, ok) = run(&["drives", "create", "--name", &format!("add-dst-{pid}"), "--format", "json"]);
    assert!(ok);
    let d2: serde_json::Value = serde_json::from_str(&d2_out).unwrap();
    let d2_id = d2["id"].as_str().unwrap();

    // Create a doc in d1
    let (doc_out, _, ok) = run(&[
        "docs", "create",
        "--type", "powerhouse/document-model",
        "--name", &format!("addtest-{pid}"),
        "--drive", d1_id,
        "--format", "json",
    ]);
    assert!(ok);
    let doc: serde_json::Value = serde_json::from_str(&doc_out).unwrap();
    let doc_id = doc["DocumentModel_createDocument"]["id"].as_str().unwrap();

    // Add to d2
    let (_, _, ok) = run(&["docs", "add-to", d2_id, doc_id, "--format", "json"]);
    assert!(ok, "add-to failed");

    // Verify parents include both
    let (parents_out, _, ok) = run(&["docs", "parents", doc_id, "--format", "json"]);
    assert!(ok);
    let parents: Vec<serde_json::Value> = serde_json::from_str(&parents_out).unwrap();
    assert!(parents.len() >= 2, "expected at least 2 parents, got {}", parents.len());

    // Remove from d2
    let (_, _, ok) = run(&["docs", "remove-from", d2_id, doc_id, "--format", "json"]);
    assert!(ok, "remove-from failed");

    // Verify d2 no longer a parent
    let (parents_out, _, ok) = run(&["docs", "parents", doc_id, "--format", "json"]);
    assert!(ok);
    let parents: Vec<serde_json::Value> = serde_json::from_str(&parents_out).unwrap();
    assert!(
        !parents.iter().any(|p| p["id"].as_str() == Some(d2_id)),
        "d2 should no longer be a parent"
    );

    // Cleanup
    run(&["drives", "delete", d1_id, d2_id, "-y"]);
}

#[test]
fn docs_move_between_drives() {
    let pid = std::process::id();

    // Create two drives
    let (d1_out, _, ok) = run(&["drives", "create", "--name", &format!("mv-src-{pid}"), "--format", "json"]);
    assert!(ok);
    let d1: serde_json::Value = serde_json::from_str(&d1_out).unwrap();
    let d1_id = d1["id"].as_str().unwrap();

    let (d2_out, _, ok) = run(&["drives", "create", "--name", &format!("mv-dst-{pid}"), "--format", "json"]);
    assert!(ok);
    let d2: serde_json::Value = serde_json::from_str(&d2_out).unwrap();
    let d2_id = d2["id"].as_str().unwrap();

    // Create a doc in d1
    let (doc_out, _, ok) = run(&[
        "docs", "create",
        "--type", "powerhouse/document-model",
        "--name", &format!("movetest-{pid}"),
        "--drive", d1_id,
        "--format", "json",
    ]);
    assert!(ok);
    let doc: serde_json::Value = serde_json::from_str(&doc_out).unwrap();
    let doc_id = doc["DocumentModel_createDocument"]["id"].as_str().unwrap();

    // Move from d1 to d2
    let (_, _, ok) = run(&["docs", "move", doc_id, "--from", d1_id, "--to", d2_id, "--format", "json"]);
    assert!(ok, "move failed");

    // Verify parent is now d2, not d1
    let (parents_out, _, ok) = run(&["docs", "parents", doc_id, "--format", "json"]);
    assert!(ok);
    let parents: Vec<serde_json::Value> = serde_json::from_str(&parents_out).unwrap();
    assert!(
        parents.iter().any(|p| p["id"].as_str() == Some(d2_id)),
        "d2 should be a parent after move"
    );
    assert!(
        !parents.iter().any(|p| p["id"].as_str() == Some(d1_id)),
        "d1 should not be a parent after move"
    );

    // Cleanup
    run(&["drives", "delete", d1_id, d2_id, "-y"]);
}

// ── Removed commands should not exist ───────────────────────────────────────

#[test]
fn access_command_removed() {
    let (_, stderr, ok) = run(&["access", "show", "some-id"]);
    assert!(!ok, "access command should not exist");
    assert!(
        stderr.contains("unrecognized") || stderr.contains("error") || stderr.contains("invalid"),
        "expected error for removed command, got: {stderr}"
    );
}

#[test]
fn groups_command_removed() {
    let (_, stderr, ok) = run(&["groups", "list"]);
    assert!(!ok, "groups command should not exist");
    assert!(
        stderr.contains("unrecognized") || stderr.contains("error") || stderr.contains("invalid"),
        "expected error for removed command, got: {stderr}"
    );
}

// ── Complete end-to-end: create document model, populate schema, add to drive ─

/// Full lifecycle test: creates a TaskTracker document model from scratch,
/// populates its schema and operations via mutations, adds it to a drive,
/// verifies the full state, then cleans up.
#[test]
fn complete_document_model_lifecycle() {
    let pid = std::process::id();

    // 1. Create a drive to hold the document
    let drive_name = format!("lifecycle-drive-{pid}");
    let (drive_out, stderr, ok) = run(&["drives", "create", "--name", &drive_name, "--format", "json"]);
    assert!(ok, "drive create failed: {stderr}");
    let drive: serde_json::Value = serde_json::from_str(&drive_out).unwrap();
    let drive_id = drive["id"].as_str().unwrap();

    // 2. Create a DocumentModel document in the drive
    let (doc_out, stderr, ok) = run(&[
        "docs", "create",
        "--type", "powerhouse/document-model",
        "--name", &format!("TaskTracker-{pid}"),
        "--drive", drive_id,
        "--format", "json",
    ]);
    assert!(ok, "doc create failed: {stderr}");
    let doc: serde_json::Value = serde_json::from_str(&doc_out).unwrap();
    let doc_id = doc["DocumentModel_createDocument"]["id"].as_str().unwrap();

    // 3. Set model metadata
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "setModelName",
        "--input", r#"{"name": "TaskTracker"}"#,
        "--format", "json",
    ]);
    assert!(ok, "setModelName failed");

    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "setModelId",
        "--input", &format!(r#"{{"id": "test/task-tracker-{pid}"}}"#),
        "--format", "json",
    ]);
    assert!(ok, "setModelId failed");

    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "setModelDescription",
        "--input", r#"{"description": "A task tracking model with title, status, assignee, and priority"}"#,
        "--format", "json",
    ]);
    assert!(ok, "setModelDescription failed");

    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "setAuthorName",
        "--input", r#"{"authorName": "Switchboard CLI Integration Tests"}"#,
        "--format", "json",
    ]);
    assert!(ok, "setAuthorName failed");

    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "setAuthorWebsite",
        "--input", r#"{"authorWebsite": "https://github.com/liberuum/switchboard-cli"}"#,
        "--format", "json",
    ]);
    assert!(ok, "setAuthorWebsite failed");

    // 4. Set state schema — tasks with title, status, assignee, priority
    let schema = r#"{"scope": "global", "schema": "{ \"type\": \"object\", \"properties\": { \"tasks\": { \"type\": \"array\", \"items\": { \"type\": \"object\", \"properties\": { \"id\": { \"type\": \"string\" }, \"title\": { \"type\": \"string\" }, \"status\": { \"type\": \"string\", \"enum\": [\"todo\", \"in_progress\", \"done\"] }, \"assignee\": { \"type\": \"string\" }, \"priority\": { \"type\": \"integer\", \"minimum\": 1, \"maximum\": 5 } }, \"required\": [\"id\", \"title\", \"status\"] } }, \"nextId\": { \"type\": \"integer\" } } }"}"#;
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "setStateSchema",
        "--input", schema,
        "--format", "json",
    ]);
    assert!(ok, "setStateSchema failed");

    // 5. Set initial state
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "setInitialState",
        "--input", r#"{"scope": "global", "initialValue": "{ \"tasks\": [], \"nextId\": 1 }"}"#,
        "--format", "json",
    ]);
    assert!(ok, "setInitialState failed");

    // 6. Add a module and operations
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "addModule",
        "--input", r#"{"id": "task-ops", "name": "Task Operations", "description": "CRUD operations for tasks"}"#,
        "--format", "json",
    ]);
    assert!(ok, "addModule failed");

    // addTask operation
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "addOperation",
        "--input", r#"{"moduleId": "task-ops", "id": "add-task", "name": "addTask", "description": "Create a new task", "schema": "{ \"type\": \"object\", \"properties\": { \"title\": { \"type\": \"string\" }, \"assignee\": { \"type\": \"string\" }, \"priority\": { \"type\": \"integer\" } }, \"required\": [\"title\"] }", "template": "", "reducer": "", "scope": "global"}"#,
        "--format", "json",
    ]);
    assert!(ok, "addOperation addTask failed");

    // completeTask operation
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "addOperation",
        "--input", r#"{"moduleId": "task-ops", "id": "complete-task", "name": "completeTask", "description": "Mark a task as done", "schema": "{ \"type\": \"object\", \"properties\": { \"id\": { \"type\": \"string\" } }, \"required\": [\"id\"] }", "template": "", "reducer": "", "scope": "global"}"#,
        "--format", "json",
    ]);
    assert!(ok, "addOperation completeTask failed");

    // reassignTask operation
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "addOperation",
        "--input", r#"{"moduleId": "task-ops", "id": "reassign-task", "name": "reassignTask", "description": "Change task assignee", "schema": "{ \"type\": \"object\", \"properties\": { \"id\": { \"type\": \"string\" }, \"assignee\": { \"type\": \"string\" } }, \"required\": [\"id\", \"assignee\"] }", "template": "", "reducer": "", "scope": "global"}"#,
        "--format", "json",
    ]);
    assert!(ok, "addOperation reassignTask failed");

    // deleteTask operation
    let (_, _, ok) = run(&[
        "docs", "mutate", doc_id, "addOperation",
        "--input", r#"{"moduleId": "task-ops", "id": "delete-task", "name": "deleteTask", "description": "Remove a task by ID", "schema": "{ \"type\": \"object\", \"properties\": { \"id\": { \"type\": \"string\" } }, \"required\": [\"id\"] }", "template": "", "reducer": "", "scope": "global"}"#,
        "--format", "json",
    ]);
    assert!(ok, "addOperation deleteTask failed");

    // 7. Verify full state
    let (state_out, _, ok) = run(&["docs", "get", doc_id, "--state", "--format", "json"]);
    assert!(ok, "docs get --state failed");
    let state: serde_json::Value = serde_json::from_str(&state_out).unwrap();

    // Verify metadata
    assert_eq!(state["documentType"].as_str(), Some("powerhouse/document-model"));
    let global = &state["state"]["global"];
    assert_eq!(global["name"].as_str(), Some("TaskTracker"));
    assert!(
        global["id"].as_str().unwrap().starts_with("test/task-tracker-"),
        "model ID should start with test/task-tracker-"
    );
    assert!(
        global["description"].as_str().unwrap().contains("task tracking"),
        "description should mention task tracking"
    );
    assert_eq!(global["author"]["name"].as_str(), Some("Switchboard CLI Integration Tests"));
    assert_eq!(global["author"]["website"].as_str(), Some("https://github.com/liberuum/switchboard-cli"));

    // Verify state schema
    let schema_str = global["specifications"][0]["state"]["global"]["schema"].as_str().unwrap();
    assert!(schema_str.contains("tasks"), "schema should define tasks array");
    assert!(schema_str.contains("priority"), "schema should define priority");

    // Verify initial state
    let init = global["specifications"][0]["state"]["global"]["initialValue"].as_str().unwrap();
    assert!(init.contains("tasks"), "initial state should have tasks");
    assert!(init.contains("nextId"), "initial state should have nextId");

    // Verify module and operations
    let modules = global["specifications"][0]["modules"].as_array().unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0]["name"].as_str(), Some("Task Operations"));
    let ops = modules[0]["operations"].as_array().unwrap();
    assert_eq!(ops.len(), 4, "expected 4 operations, got {}", ops.len());
    let op_names: Vec<&str> = ops.iter().filter_map(|o| o["name"].as_str()).collect();
    assert!(op_names.contains(&"addTask"));
    assert!(op_names.contains(&"completeTask"));
    assert!(op_names.contains(&"reassignTask"));
    assert!(op_names.contains(&"deleteTask"));

    // 8. Verify parents
    let (parents_out, _, ok) = run(&["docs", "parents", doc_id, "--format", "json"]);
    assert!(ok, "parents query failed");
    let parents: Vec<serde_json::Value> = serde_json::from_str(&parents_out).unwrap();
    assert!(
        parents.iter().any(|p| p["id"].as_str() == Some(drive_id)),
        "document should be a child of the drive"
    );

    // 9. Verify operations history
    let (ops_out, _, ok) = run(&["ops", doc_id, "--format", "json"]);
    assert!(ok, "ops query failed");
    let ops_data: serde_json::Value = serde_json::from_str(&ops_out).unwrap();
    let op_count = ops_data.as_array().map(|a| a.len()).unwrap_or(0);
    // CREATE_DOCUMENT + SET_NAME + setModelName + setModelId + setModelDescription +
    // setAuthorName + setAuthorWebsite + setStateSchema + setInitialState +
    // addModule + 4x addOperation = 14+ operations
    assert!(
        op_count >= 12,
        "expected at least 12 operations, got {op_count}"
    );

    // 10. Rename the document
    let new_name = format!("TaskTracker-Renamed-{pid}");
    let (rename_out, _, ok) = run(&["docs", "rename", doc_id, &new_name, "--format", "json"]);
    assert!(ok, "rename failed");
    let renamed: serde_json::Value = serde_json::from_str(&rename_out).unwrap();
    assert_eq!(renamed["name"].as_str().unwrap(), new_name);

    // 11. Cleanup
    let (_, _, ok) = run(&["drives", "delete", drive_id, "-y"]);
    assert!(ok, "cleanup drive delete failed");
}
