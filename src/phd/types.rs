use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The header.json file inside a .phd archive
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhdHeader {
    pub id: String,
    #[serde(default)]
    pub sig: Value,
    pub document_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_utc_iso: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub name: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub revision: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified_at_utc_iso: Option<String>,
    #[serde(default)]
    pub meta: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_versions: Option<Value>,
}

/// The state wrapper used in state.json and current-state.json
/// Format: { auth: {}, document: { version, hash }, global: <stateJSON>, local: {} }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhdState {
    #[serde(default)]
    pub auth: Value,
    #[serde(default)]
    pub document: Value,
    #[serde(default)]
    pub global: Value,
    #[serde(default)]
    pub local: Value,
}

impl Default for PhdState {
    fn default() -> Self {
        Self {
            auth: Value::Object(serde_json::Map::new()),
            document: serde_json::json!({
                "version": 0,
                "hash": { "algorithm": "sha1", "encoding": "base64" }
            }),
            global: Value::Object(serde_json::Map::new()),
            local: Value::Object(serde_json::Map::new()),
        }
    }
}

/// The operations.json file inside a .phd archive.
/// Operations are grouped by scope name (e.g. "global", "document", "local", custom scopes).
/// Serializes as `{ "global": [...], "document": [...], ... }`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PhdOperations(pub HashMap<String, Vec<Value>>);

impl PhdOperations {
    /// Total number of operations across all scopes.
    pub fn total_ops(&self) -> usize {
        self.0.values().map(|v| v.len()).sum()
    }

    /// Iterate over operations in all non-document scopes (global, local, custom, etc.).
    pub fn non_document_ops(&self) -> impl Iterator<Item = &Value> {
        self.0
            .iter()
            .filter(|(k, _)| k.as_str() != "document")
            .flat_map(|(_, v)| v.iter())
    }
}
