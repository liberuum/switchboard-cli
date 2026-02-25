use anyhow::{Result, bail};

use crate::config::{Config, Profile, load_config};
use crate::graphql::{GraphQLClient, IntrospectionCache};
use crate::graphql::introspection::load_cache;

/// Resolve the active profile from CLI args or config default
pub fn resolve_profile(config: &Config, profile_name: Option<&str>) -> Result<(String, Profile)> {
    if let Some(name) = profile_name {
        match config.get_profile(name) {
            Some(p) => Ok((name.to_string(), p.clone())),
            None => bail!("Profile '{name}' not found. Run `switchboard config list` to see available profiles."),
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
    let cache = load_cache(&name)?
        .ok_or_else(|| anyhow::anyhow!(
            "No introspection cache found for profile '{name}'. Run `switchboard introspect` first."
        ))?;
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

/// Truncate a string to max_len, appending "..." if truncated
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
