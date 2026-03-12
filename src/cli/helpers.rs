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

/// Resolve a document identifier to its UUID.
///
/// Supports:
/// - `"drive-slug/doc-name"` — finds a child doc within a specific drive
/// - `"identifier"` — resolves directly via `document(identifier)`
///
/// Returns the document's UUID (PHID).
pub async fn resolve_doc(client: &GraphQLClient, id_or_name: &str) -> Result<String> {
    // Handle "drive/doc" format
    if let Some(slash_pos) = id_or_name.find('/') {
        let drive_part = &id_or_name[..slash_pos];
        let doc_part = &id_or_name[slash_pos + 1..];

        // "drive/" (trailing slash, no doc) → treat as the drive document itself
        if doc_part.is_empty() {
            return resolve_single_doc(client, drive_part).await;
        }

        // Try to find doc within the drive's children
        let drive_id = resolve_single_doc(client, drive_part).await?;
        let children_query = format!(
            r#"{{ documentChildren(parentIdentifier: "{drive_id}") {{ items {{ id slug name }} }} }}"#,
        );

        if let Ok(data) = client.query(&children_query, None).await
            && let Some(items) = data
                .pointer("/documentChildren/items")
                .and_then(|v| v.as_array())
        {
            let is_uuid = is_uuid(doc_part);
            for child in items {
                let child_id = child["id"].as_str().unwrap_or("");
                let child_slug = child["slug"].as_str().unwrap_or("");
                let child_name = child["name"].as_str().unwrap_or("");

                if (is_uuid && child_id == doc_part)
                    || child_slug.eq_ignore_ascii_case(doc_part)
                    || child_name.eq_ignore_ascii_case(doc_part)
                {
                    return Ok(child_id.to_string());
                }
            }
        }

        bail!(
            "Document '{}' not found in drive '{}'",
            doc_part,
            drive_part
        )
    }

    // Direct identifier lookup
    resolve_single_doc(client, id_or_name).await
}

/// Resolve a single identifier (UUID or slug) to its UUID via `document(identifier)`.
async fn resolve_single_doc(client: &GraphQLClient, identifier: &str) -> Result<String> {
    let query = format!(
        r#"{{ document(identifier: "{id}") {{ document {{ id }} }} }}"#,
        id = identifier.replace('"', r#"\""#)
    );
    let data = client.query(&query, None).await?;
    match data
        .pointer("/document/document/id")
        .and_then(|v| v.as_str())
    {
        Some(id) => Ok(id.to_string()),
        None => bail!("Document '{}' not found", identifier),
    }
}

/// Fetch available drives and present a `Select` picker.
/// Returns `(id, slug, name)` for the chosen drive.
pub async fn select_drive(client: &GraphQLClient) -> Result<(String, String, String)> {
    let data = client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug } } }"#,
            None,
        )
        .await?;

    let drives: Vec<(String, String, String)> = data
        .pointer("/findDocuments/items")
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
            let identifier = if !slug.is_empty() {
                slug.as_str()
            } else {
                id.as_str()
            };
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

/// Derive the base URL from a GraphQL endpoint URL.
/// e.g. "http://localhost:4001/graphql" → "http://localhost:4001"
pub fn base_url_from(graphql_url: &str) -> String {
    graphql_url
        .trim_end_matches('/')
        .trim_end_matches("/graphql")
        .to_string()
}

pub fn is_uuid(s: &str) -> bool {
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
