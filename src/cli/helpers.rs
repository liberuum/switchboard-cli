use anyhow::{Result, bail};
use serde_json::Value;

use crate::config::{Config, Profile, load_config};
use crate::graphql::introspection::load_cache;
use crate::graphql::{GraphQLClient, IntrospectionCache};

/// Resolve the active profile from CLI args or config default
pub fn resolve_profile(config: &Config, profile_name: Option<&str>) -> Result<(String, Profile)> {
    if let Some(name) = profile_name {
        match config.get_profile(name) {
            Some(p) => Ok((name.to_string(), p.clone())),
            None => bail!(
                "Profile '{name}' not found. Run `switchboard config list` to see available profiles."
            ),
        }
    } else {
        match config.default_profile() {
            Some((name, p)) => Ok((name.to_string(), p.clone())),
            None => bail!("No default profile configured. Run `switchboard init` first."),
        }
    }
}

/// Build a GraphQLClient from the active profile
pub fn build_client(profile: &Profile) -> GraphQLClient {
    GraphQLClient::new(profile.url.clone(), profile.token.clone())
}

/// Load config, resolve profile, build client — the common preamble for most commands
pub fn setup(profile_name: Option<&str>) -> Result<(String, Profile, GraphQLClient)> {
    let config = load_config()?;
    let (name, profile) = resolve_profile(&config, profile_name)?;
    let client = build_client(&profile);
    Ok((name, profile, client))
}

/// Load config, resolve profile, build client, and load introspection cache
pub fn setup_with_cache(
    profile_name: Option<&str>,
) -> Result<(String, Profile, GraphQLClient, IntrospectionCache)> {
    let (name, profile, client) = setup(profile_name)?;
    let cache = load_cache(&name)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No introspection cache found for profile '{name}'. Run `switchboard introspect` first."
        )
    })?;
    Ok((name, profile, client, cache))
}

/// Resolve a slug or UUID to a drive UUID via the API
pub async fn resolve_drive_id(client: &GraphQLClient, id_or_slug: &str) -> Result<String> {
    // If it looks like a UUID, return as-is
    if is_uuid(id_or_slug) {
        return Ok(id_or_slug.to_string());
    }

    // Otherwise treat as slug and resolve
    let query = format!(
        r#"{{ driveIdBySlug(slug: "{}") }}"#,
        id_or_slug.replace('"', r#"\""#)
    );
    let data = client.query(&query, None).await?;
    match data.get("driveIdBySlug").and_then(|v| v.as_str()) {
        Some(id) => Ok(id.to_string()),
        None => bail!("Could not resolve slug '{id_or_slug}' to a drive ID"),
    }
}

/// Convert a serde_json::Value into a GraphQL literal string (unquoted keys).
pub fn json_to_graphql(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_graphql).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", json_to_graphql(v)))
                .collect();
            format!("{{ {} }}", fields.join(", "))
        }
    }
}

/// Resolve a document identifier (UUID or name) to `(doc_id, drive_id)`.
/// If `id_or_name` looks like a UUID, searches drives for that ID.
/// Otherwise, searches all drives for a document with a matching name.
pub async fn resolve_doc(client: &GraphQLClient, id_or_name: &str) -> Result<(String, String)> {
    let data = client
        .query(
            r#"{ driveDocuments { id state { nodes { ... on DocumentDrive_FileNode { id name kind } } } } }"#,
            None,
        )
        .await?;

    let drives = data
        .get("driveDocuments")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let is_uuid = is_uuid(id_or_name);

    for drv in &drives {
        let drive_id = drv["id"].as_str().unwrap_or("");
        let mut nodes = drv
            .pointer("/state/nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Fall back to per-drive query if nodes are empty
        if nodes.is_empty() && !drive_id.is_empty() {
            let q = format!(
                r#"{{ driveDocument(idOrSlug: "{drive_id}") {{ state {{ nodes {{ ... on DocumentDrive_FileNode {{ id name kind }} }} }} }} }}"#
            );
            if let Ok(d) = client.query(&q, None).await {
                nodes = d
                    .pointer("/driveDocument/state/nodes")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
            }
        }

        for node in &nodes {
            if node["kind"].as_str() != Some("file") {
                continue;
            }
            let node_id = node["id"].as_str().unwrap_or("");
            let node_name = node["name"].as_str().unwrap_or("");

            if is_uuid && node_id == id_or_name {
                return Ok((node_id.to_string(), drive_id.to_string()));
            }
            if !is_uuid && node_name.eq_ignore_ascii_case(id_or_name) {
                return Ok((node_id.to_string(), drive_id.to_string()));
            }
        }
    }

    bail!("Document '{}' not found in any drive", id_or_name)
}

/// Fetch available drives and present a `Select` picker.
/// Returns `(id, slug, name)` for the chosen drive.
pub async fn select_drive(client: &GraphQLClient) -> Result<(String, String, String)> {
    let data = client
        .query("{ driveDocuments { id name slug } }", None)
        .await?;

    let drives: Vec<(String, String, String)> = data
        .get("driveDocuments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|d| {
                    let id = d["id"].as_str().unwrap_or("").to_string();
                    let name = d["name"].as_str().unwrap_or("").to_string();
                    let slug = d["slug"].as_str().unwrap_or("").to_string();
                    (id, slug, name)
                })
                .collect()
        })
        .unwrap_or_default();

    if drives.is_empty() {
        bail!("No drives found. Create one with `drives create` first.");
    }

    // Build display labels: "name (slug)"
    let labels: Vec<String> = drives
        .iter()
        .map(|(id, slug, name)| {
            let identifier = if !slug.is_empty() { slug.as_str() } else { id.as_str() };
            format!("{name}  ({identifier})")
        })
        .collect();

    println!("\nAvailable drives:");
    let selection = dialoguer::Select::new()
        .with_prompt("Select drive")
        .items(&labels)
        .interact()?;

    Ok(drives[selection].clone())
}

fn is_uuid(s: &str) -> bool {
    // Simple UUID check: 8-4-4-4-12 hex pattern
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}
