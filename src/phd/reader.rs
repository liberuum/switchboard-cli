use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

use super::types::{PhdHeader, PhdOperations, PhdState};

/// Contents extracted from a .phd ZIP archive
pub struct PhdContents {
    pub header: PhdHeader,
    pub _initial_state: PhdState,
    pub current_state: PhdState,
    pub operations: PhdOperations,
}

/// Read a .phd ZIP archive from the given path
pub fn read_phd(path: &Path) -> Result<PhdContents> {
    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;

    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Not a valid ZIP file: {}", path.display()))?;

    let header: PhdHeader = read_json_entry(&mut archive, "header.json")
        .with_context(|| format!("Missing or invalid header.json in {}", path.display()))?;

    let initial_state: PhdState = read_json_entry(&mut archive, "state.json").unwrap_or_default();

    let current_state: PhdState =
        read_json_entry(&mut archive, "current-state.json").unwrap_or_default();

    let operations: PhdOperations = read_json_entry(&mut archive, "operations.json")
        .unwrap_or(PhdOperations { global: vec![] });

    Ok(PhdContents {
        header,
        _initial_state: initial_state,
        current_state,
        operations,
    })
}

fn read_json_entry<T: serde::de::DeserializeOwned>(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> Result<T> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("Entry '{name}' not found in archive"))?;

    let mut contents = String::new();
    entry
        .read_to_string(&mut contents)
        .with_context(|| format!("Failed to read '{name}'"))?;

    serde_json::from_str(&contents).with_context(|| format!("Failed to parse '{name}' as JSON"))
}
