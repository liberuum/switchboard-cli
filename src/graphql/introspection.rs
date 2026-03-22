use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::client::GraphQLClient;
use crate::config::profiles;

/// Introspection query for the nested mutation API (dev.104+).
/// Top-level mutation fields like `DocumentModel` return a `*Mutations` object
/// whose sub-fields are the actual operations (e.g. `setModelName`).
const INTROSPECTION_QUERY: &str = r#"{
  __schema {
    mutationType {
      fields {
        name
        type {
          name
          kind
          ofType { name kind }
        }
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

/// Query to fetch sub-fields of a nested mutations type (e.g. DocumentModelMutations).
const NESTED_FIELDS_QUERY: &str = r#"query($typeName: String!) {
  __type(name: $typeName) {
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
    /// The top-level mutation namespace (e.g. "DocumentModel").
    /// Mutations are called as `namespace { operation(args) { ... } }`.
    pub namespace: String,
    pub operations: Vec<ModelOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntrospectionCache {
    pub models: BTreeMap<String, DocumentModel>,
    pub timestamp: String,
    pub url: String,
}

impl DocumentModel {
    /// Build a mutation query body for this model.
    /// Nested format: `Namespace { operation(args) { ... } }`
    /// Legacy format: `Namespace_operation(args) { ... }`
    pub fn mutation_body(&self, operation: &str, args: &str, selection: &str) -> String {
        if self.namespace.is_empty() {
            // Legacy flat format
            format!(
                "{prefix}_{operation}({args}) {selection}",
                prefix = self.prefix
            )
        } else {
            // Nested format
            format!(
                "{ns} {{ {operation}({args}) {selection} }}",
                ns = self.namespace
            )
        }
    }
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

    let fields = data
        .pointer("/__schema/mutationType/fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Detect schema format: nested (dev.104+) vs flat (legacy).
    // Nested format: top-level fields return `*Mutations` object types.
    // Flat format: top-level fields are named `Prefix_operation`.
    let mut namespace_types: Vec<(String, String)> = Vec::new(); // (field_name, type_name)
    for field in &fields {
        let name = field["name"].as_str().unwrap_or_default();
        let type_name = field
            .pointer("/type/ofType/name")
            .or_else(|| field.pointer("/type/name"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if type_name.ends_with("Mutations") {
            namespace_types.push((name.to_string(), type_name.to_string()));
        }
    }

    if !namespace_types.is_empty() {
        // Nested mutation format (dev.104+)
        for (ns_name, type_name) in &namespace_types {
            // Skip DocumentDrive — it's infrastructure, not a user document model
            if ns_name == "DocumentDrive" {
                continue;
            }

            let vars = serde_json::json!({ "typeName": type_name });
            let type_data = match client.query(NESTED_FIELDS_QUERY, Some(&vars)).await {
                Ok(d) => d,
                Err(_) => continue,
            };

            let sub_fields = type_data
                .pointer("/__type/fields")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Look for createDocument to confirm this is a document model
            let has_create = sub_fields
                .iter()
                .any(|f| f["name"].as_str() == Some("createDocument"));
            if !has_create {
                continue;
            }

            let doc_type = prefix_to_document_type(ns_name);
            let mut operations = Vec::new();

            for sub_field in &sub_fields {
                let op_name = sub_field["name"].as_str().unwrap_or_default();
                // Skip Async variants — they duplicate the sync ones
                if op_name.ends_with("Async") || op_name == "createEmptyDocument" {
                    continue;
                }
                let args = parse_args(sub_field);
                operations.push(ModelOperation {
                    full_name: op_name.to_string(),
                    operation: op_name.to_string(),
                    args,
                });
            }

            models.insert(
                doc_type.clone(),
                DocumentModel {
                    prefix: ns_name.clone(),
                    document_type: doc_type,
                    create_mutation: "createDocument".to_string(),
                    namespace: ns_name.clone(),
                    operations,
                },
            );
        }
    } else {
        // Legacy flat mutation format (pre-dev.104)
        // First pass: find all _createDocument mutations
        for field in &fields {
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
                        namespace: String::new(),
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
        for field in &fields {
            let name = field["name"].as_str().unwrap_or_default();
            if name.ends_with("_createDocument") {
                continue;
            }
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
