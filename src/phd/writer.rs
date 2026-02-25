use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

use super::types::{PhdHeader, PhdOperations, PhdState};

/// Write a .phd ZIP archive to the given path
pub fn write_phd(
    path: &Path,
    header: &PhdHeader,
    initial_state: &PhdState,
    current_state: &PhdState,
    operations: &PhdOperations,
) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("Failed to create {}", path.display()))?;

    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // header.json
    zip.start_file("header.json", options)?;
    let header_json = serde_json::to_string_pretty(header)?;
    zip.write_all(header_json.as_bytes())?;

    // state.json (initial empty state)
    zip.start_file("state.json", options)?;
    let state_json = serde_json::to_string_pretty(initial_state)?;
    zip.write_all(state_json.as_bytes())?;

    // current-state.json (state with global = stateJSON from API)
    zip.start_file("current-state.json", options)?;
    let current_json = serde_json::to_string_pretty(current_state)?;
    zip.write_all(current_json.as_bytes())?;

    // operations.json
    zip.start_file("operations.json", options)?;
    let ops_json = serde_json::to_string_pretty(operations)?;
    zip.write_all(ops_json.as_bytes())?;

    zip.finish()?;
    Ok(())
}
