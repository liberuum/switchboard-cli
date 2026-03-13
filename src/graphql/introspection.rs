use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::client::GraphQLClient;
use crate::config::profiles;

const INTROSPECTION_QUERY: &str = r#"{
  __schema {
    mutationType {
      fields {
        name
        args {
          name
          type {
            name
            kind
            ofType { name kind ofType { name kind ofType { name kind } } }
          }
        }
      }
    }
  }
}"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOperation {
    pub full_name: String,
    pub operation: String,
    pub args: Vec<OperationArg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationArg {
    pub name: String,
    pub type_name: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentModel {
    pub prefix: String,
    pub document_type: String,
    pub create_mutation: String,
    pub operations: Vec<ModelOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntrospectionCache {
    pub models: BTreeMap<String, DocumentModel>,
    pub timestamp: String,
    pub url: String,
}

impl IntrospectionCache {
    pub fn find_by_prefix(&self, prefix: &str) -> Option<&DocumentModel> {
        self.models.values().find(|m| m.prefix == prefix)
    }

    pub fn find_by_type(&self, doc_type: &str) -> Option<&DocumentModel> {
        self.models.get(doc_type)
    }

    /// Find a model that can handle a given document type string.
    /// Tries exact match first, then case-insensitive prefix match.
    pub fn find_model(&self, type_or_prefix: &str) -> Option<&DocumentModel> {
        self.find_by_type(type_or_prefix)
            .or_else(|| self.find_by_prefix(type_or_prefix))
            .or_else(|| {
                let lower = type_or_prefix.to_lowercase();
                self.models
                    .values()
                    .find(|m| m.prefix.to_lowercase() == lower)
            })
    }
}

pub async fn run_introspection(client: &GraphQLClient) -> Result<IntrospectionCache> {
    let data = client
        .query(INTROSPECTION_QUERY, None)
        .await
        .context("Introspection query failed")?;

    let mut models: BTreeMap<String, DocumentModel> = BTreeMap::new();

    // Parse mutations to find _createDocument and other model-specific mutations
    if let Some(fields) = data
        .pointer("/__schema/mutationType/fields")
        .and_then(|v| v.as_array())
    {
        // First pass: find all _createDocument mutations to discover model prefixes
        for field in fields {
            let name = field["name"].as_str().unwrap_or_default();
            if let Some(prefix) = name.strip_suffix("_createDocument") {
                let doc_type = prefix_to_document_type(prefix);
                let args = parse_args(field);
                models.insert(
                    doc_type.clone(),
                    DocumentModel {
                        prefix: prefix.to_string(),
                        document_type: doc_type,
                        create_mutation: name.to_string(),
                        operations: vec![ModelOperation {
                            full_name: name.to_string(),
                            operation: "createDocument".to_string(),
                            args,
                        }],
                    },
                );
            }
        }

        // Second pass: find all other model-specific mutations
        for field in fields {
            let name = field["name"].as_str().unwrap_or_default();
            if name.ends_with("_createDocument") {
                continue; // Already handled
            }

            // Check if this mutation belongs to a known prefix
            for model in models.values_mut() {
                if let Some(op_name) = name.strip_prefix(&format!("{}_", model.prefix)) {
                    let args = parse_args(field);
                    model.operations.push(ModelOperation {
                        full_name: name.to_string(),
                        operation: op_name.to_string(),
                        args,
                    });
                    break;
                }
            }
        }
    }

    let cache = IntrospectionCache {
        models,
        timestamp: chrono_now(),
        url: client.url.clone(),
    };

    Ok(cache)
}

fn parse_args(field: &Value) -> Vec<OperationArg> {
    let mut args = Vec::new();
    if let Some(field_args) = field["args"].as_array() {
        for arg in field_args {
            let name = arg["name"].as_str().unwrap_or_default().to_string();
            let (type_name, required) = extract_type_info(&arg["type"]);
            args.push(OperationArg {
                name,
                type_name,
                required,
            });
        }
    }
    args
}

fn extract_type_info(type_val: &Value) -> (String, bool) {
    let kind = type_val["kind"].as_str().unwrap_or_default();
    if kind == "NON_NULL" {
        let inner = &type_val["ofType"];
        let (name, _) = extract_type_info(inner);
        (name, true)
    } else if kind == "LIST" {
        let inner = &type_val["ofType"];
        let (name, _) = extract_type_info(inner);
        (format!("[{name}]"), false)
    } else {
        let name = type_val["name"].as_str().unwrap_or("Unknown").to_string();
        (name, false)
    }
}

/// Convert PascalCase prefix to document type string
/// e.g., "Invoice" -> "powerhouse/invoice"
/// e.g., "BuilderProfile" -> "powerhouse/builder-profile"
fn prefix_to_document_type(prefix: &str) -> String {
    let mut result = String::new();
    for (i, ch) in prefix.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('-');
        }
        result.push(ch.to_ascii_lowercase());
    }
    format!("powerhouse/{result}")
}

fn chrono_now() -> String {
    // Simple ISO-8601 timestamp without chrono dependency
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", duration.as_secs())
}

pub fn cache_path(profile_name: &str) -> Result<PathBuf> {
    let dir = profiles::cache_dir()?;
    Ok(dir.join(format!("{profile_name}.json")))
}

pub fn load_cache(profile_name: &str) -> Result<Option<IntrospectionCache>> {
    let path = cache_path(profile_name)?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read cache: {}", path.display()))?;
    match serde_json::from_str::<IntrospectionCache>(&contents) {
        Ok(cache) => Ok(Some(cache)),
        Err(_) => {
            // Cache format is incompatible (e.g. written by a different version).
            // Delete the stale file and re-introspect on next use.
            eprintln!(
                "Cache format outdated, removing {}. Run `switchboard introspect` to rebuild.",
                path.display()
            );
            let _ = std::fs::remove_file(&path);
            Ok(None)
        }
    }
}

pub fn save_cache(profile_name: &str, cache: &IntrospectionCache) -> Result<()> {
    let path = cache_path(profile_name)?;
    let contents = serde_json::to_string_pretty(cache).context("Failed to serialize cache")?;
    std::fs::write(&path, contents)
        .with_context(|| format!("Failed to write cache: {}", path.display()))?;
    Ok(())
}
