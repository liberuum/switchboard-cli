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
    let (_, _, ok) = run(&["drives", "delete", id, "-y"]);
    assert!(ok, "drives delete single failed");
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
        stderr.contains("Error"),
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

    let (_, _, ok) = run(&["docs", "list", "--drive", slug, "--format", "table"]);
    assert!(ok, "docs list failed for drive {slug}");
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
                writeln!(stdin, "query {{ driveDocuments {{ id }} }}")?;
                writeln!(stdin, "exit")?;
            }
            child.wait_with_output()
        })
        .expect("failed to run interactive mode");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout.contains("driveDocuments"),
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
