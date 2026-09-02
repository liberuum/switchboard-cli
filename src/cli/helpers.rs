use anyhow::{Context, Result, bail};
use colored::Colorize;

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

/// The signing identity for a profile, if one is configured.
///
/// Resolution order: `SWITCHBOARD_UNSIGNED=1` disables signing for this
/// invocation; `SWITCHBOARD_IDENTITY_DIR` / `SWITCHBOARD_APP_NAME` override the
/// profile's values (so a plugin can name itself per call without touching
/// the user's config); otherwise the profile's `identity` block. `None` means
/// "write unsigned" — the Switchboard then signs with its own identity.
///
/// A configured identity that fails to load is an ERROR, not a silent
/// downgrade: the user asked for signed writes, and an unsigned write would
/// be attributed to whoever the server is logged in as.
pub fn load_identity(profile: &Profile) -> Result<Option<(crate::identity::Identity, String)>> {
    if std::env::var("SWITCHBOARD_UNSIGNED")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let env_dir = std::env::var("SWITCHBOARD_IDENTITY_DIR").ok();
    let env_app = std::env::var("SWITCHBOARD_APP_NAME").ok();
    let (dir, app_name) = match (&profile.identity, env_dir) {
        (_, Some(dir)) => (
            std::path::PathBuf::from(dir),
            env_app
                .clone()
                .or_else(|| profile.identity.as_ref().map(|i| i.app_name.clone()))
                .unwrap_or_else(|| "switchboard-cli".to_string()),
        ),
        (Some(cfg), None) => (
            std::path::PathBuf::from(&cfg.ph_dir),
            env_app.clone().unwrap_or_else(|| cfg.app_name.clone()),
        ),
        (None, None) => return Ok(None),
    };
    let identity = crate::identity::Identity::load(&dir).with_context(|| {
        format!(
            "signing identity configured but unusable ({}). Run `ph login` there and \
             `switchboard auth login --renown --ph-dir {}` again, or set SWITCHBOARD_UNSIGNED=1 \
             to write unsigned on purpose.",
            dir.display(),
            dir.display()
        )
    })?;
    if identity.credential_expired() {
        eprintln!(
            "{} Renown credential for {} expired on {} — operations are still signed by this key, \
             but the key↔address binding is stale. Run `ph login` in {} to renew.",
            "⚠".yellow(),
            identity.did,
            identity.credential_expires().unwrap_or("?"),
            dir.display()
        );
    }
    Ok(Some((identity, app_name)))
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
        let is_uuid = is_uuid(doc_part);

        // Fast path: documentOutgoingRelationships is the proper relationship index.
        let children_query = format!(
            r#"{{ documentOutgoingRelationships(sourceIdentifier: "{drive_id}", relationshipType: "child") {{ items {{ id slug name }} }} }}"#,
        );

        if let Ok(data) = client.query(&children_query, None).await
            && let Some(items) = data
                .pointer("/documentOutgoingRelationships/items")
                .and_then(|v| v.as_array())
        {
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

        // Fallback: documentOutgoingRelationships can return empty even when
        // the drive's node list has the doc (relationship index lag, especially
        // after ADD_FILE on a remote reactor). Scan the drive's state.global.nodes
        // directly — that's the source of truth `docs list` and Connect use.
        let nodes_query =
            format!(r#"{{ document(identifier: "{drive_id}") {{ document {{ state }} }} }}"#,);
        if let Ok(data) = client.query(&nodes_query, None).await
            && let Some(nodes) = data
                .pointer("/document/document/state/global/nodes")
                .and_then(|v| v.as_array())
        {
            for node in nodes {
                if node["kind"].as_str() != Some("file") {
                    continue;
                }
                let node_id = node["id"].as_str().unwrap_or("");
                let node_name = node["name"].as_str().unwrap_or("");
                if (is_uuid && node_id == doc_part) || node_name.eq_ignore_ascii_case(doc_part) {
                    return Ok(node_id.to_string());
                }
            }
        }

        // As a last resort: if the user passed a UUID, try resolving it
        // directly (it might exist on the reactor even if not indexed under
        // this drive yet). The caller asked for a specific UUID, so honor it.
        if is_uuid && let Ok(id) = resolve_single_doc(client, doc_part).await {
            return Ok(id);
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

/// Resolve a single identifier (UUID, slug, or name) to its UUID.
/// First tries `document(identifier)` for UUIDs/slugs, then falls back
/// to a name search via `findDocuments` for human-readable names.
async fn resolve_single_doc(client: &GraphQLClient, identifier: &str) -> Result<String> {
    let escaped = identifier.replace('"', r#"\""#);

    // Try direct lookup (works for UUIDs and slugs)
    let query = format!(r#"{{ document(identifier: "{escaped}") {{ document {{ id }} }} }}"#,);
    if let Ok(data) = client.query(&query, None).await
        && let Some(id) = data
            .pointer("/document/document/id")
            .and_then(|v| v.as_str())
    {
        return Ok(id.to_string());
    }

    // Fallback: search by name. Try drives first, then search each drive's children.
    let mut matches: Vec<(String, String)> = Vec::new(); // (id, name)

    // Search drives by name
    let drive_query = r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug } } }"#;
    if let Ok(data) = client.query(drive_query, None).await
        && let Some(items) = data
            .pointer("/findDocuments/items")
            .and_then(|v| v.as_array())
    {
        for item in items {
            let item_name = item["name"].as_str().unwrap_or("");
            let item_slug = item["slug"].as_str().unwrap_or("");
            let item_id = item["id"].as_str().unwrap_or("");
            if item_name.eq_ignore_ascii_case(identifier)
                || item_slug.eq_ignore_ascii_case(identifier)
            {
                matches.push((item_id.to_string(), item_name.to_string()));
            }
        }
    }

    // If no drive matched, search document children across all drives
    if matches.is_empty()
        && let Ok(data) = client
            .query(
                r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id } } }"#,
                None,
            )
            .await
        && let Some(drives) = data
            .pointer("/findDocuments/items")
            .and_then(|v| v.as_array())
    {
        for drv in drives {
            let drv_id = drv["id"].as_str().unwrap_or("");
            if drv_id.is_empty() {
                continue;
            }
            let children_query = format!(
                r#"{{ documentOutgoingRelationships(sourceIdentifier: "{drv_id}", relationshipType: "child") {{ items {{ id name slug }} }} }}"#,
            );
            if let Ok(cd) = client.query(&children_query, None).await
                && let Some(items) = cd
                    .pointer("/documentOutgoingRelationships/items")
                    .and_then(|v| v.as_array())
            {
                for item in items {
                    let item_name = item["name"].as_str().unwrap_or("");
                    let item_slug = item["slug"].as_str().unwrap_or("");
                    let item_id = item["id"].as_str().unwrap_or("");
                    if item_name.eq_ignore_ascii_case(identifier)
                        || item_slug.eq_ignore_ascii_case(identifier)
                    {
                        matches.push((item_id.to_string(), item_name.to_string()));
                    }
                }
            }
        }
    }
    if matches.len() == 1 {
        return Ok(matches[0].0.clone());
    }
    if matches.len() > 1 {
        // Deduplicate by ID
        matches.sort();
        matches.dedup_by(|a, b| a.0 == b.0);
        if matches.len() == 1 {
            return Ok(matches[0].0.clone());
        }
        let list: Vec<String> = matches
            .iter()
            .map(|(id, name)| format!("  {id} ({name})"))
            .collect();
        bail!(
            "Multiple documents match '{}'. Use an ID:\n{}",
            identifier,
            list.join("\n")
        );
    }

    bail!("Document '{}' not found", identifier)
}

/// Fetch available drives and present a `Select` picker.
/// Returns `(id, slug, name)` for the chosen drive.
pub async fn select_drive(client: &GraphQLClient) -> Result<(String, String, String)> {
    let data = client
        .query(
            r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id name slug state } } }"#,
            None,
        )
        .await?;

    let drives: Vec<(String, String, String)> = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|d| {
                    d.pointer("/state/document/isDeleted")
                        .and_then(|v| v.as_bool())
                        != Some(true)
                })
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

/// Resolve a parent identifier (drive or folder, by UUID, slug, or name) to
/// the drive that holds it plus an optional folder ID inside that drive.
///
/// Returns `(drive_id, None)` when the parent is a drive itself, and
/// `(drive_id, Some(folder_id))` when the parent is a folder. Errors when no
/// match is found or when a folder name is ambiguous across drives.
///
/// Used by `docs add-to`, `docs remove-from`, and `docs move` so they can
/// target a drive root or a sub-folder uniformly. Folders are not documents
/// on the new server schema — they live in `drive.state.global.nodes` — so
/// `resolve_doc` alone is insufficient for parent resolution.
pub async fn resolve_drive_and_parent(
    client: &GraphQLClient,
    parent: &str,
) -> Result<(String, Option<String>)> {
    // Single round-trip: fetch every drive with id/slug/name/state. We then
    // match `parent` against drive identifiers AND folder entries within
    // each drive's node tree. This deliberately avoids calling
    // `document(identifier: <folder-id>)` — folders are tree nodes, not
    // documents, so that resolver would always error with "Document not
    // found" and the failed query gets logged on the reactor side even
    // though the CLI catches it. Scanning the already-fetched node trees
    // is silent.
    let drives_query = r#"{ findDocuments(search: { type: "powerhouse/document-drive" }) { items { id slug name state } } }"#;
    let data = client.query(drives_query, None).await?;
    let drives = data
        .pointer("/findDocuments/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let parent_is_uuid = is_uuid(parent);

    // Pass 1: is `parent` a drive itself? Match by id, slug, or name.
    for d in &drives {
        let drive_id = d["id"].as_str().unwrap_or("");
        let drive_slug = d["slug"].as_str().unwrap_or("");
        let drive_name = d["name"].as_str().unwrap_or("");
        let id_match = parent_is_uuid && drive_id == parent;
        let slug_match = !parent_is_uuid && drive_slug.eq_ignore_ascii_case(parent);
        let name_match = !parent_is_uuid && drive_name.eq_ignore_ascii_case(parent);
        if id_match || slug_match || name_match {
            return Ok((drive_id.to_string(), None));
        }
    }

    // Pass 2: is `parent` a folder anywhere? Match by id (when UUID) or
    // name (otherwise) against `state.global.nodes` entries with kind=folder.
    let mut matches: Vec<(String, String)> = Vec::new();
    for d in &drives {
        let drive_id = d["id"].as_str().unwrap_or("").to_string();
        let nodes = match d.pointer("/state/global/nodes").and_then(|v| v.as_array()) {
            Some(n) => n,
            None => continue,
        };
        for n in nodes {
            if n["kind"].as_str() != Some("folder") {
                continue;
            }
            let id = n["id"].as_str().unwrap_or("");
            let name = n["name"].as_str().unwrap_or("");
            let id_match = parent_is_uuid && id == parent;
            let name_match = !parent_is_uuid && name == parent;
            if id_match || name_match {
                matches.push((drive_id.clone(), id.to_string()));
            }
        }
    }

    match matches.len() {
        0 => bail!("Parent '{parent}' is not a drive or folder visible on this profile"),
        1 => {
            let (drive_id, folder_id) = matches.into_iter().next().unwrap();
            Ok((drive_id, Some(folder_id)))
        }
        n => bail!(
            "Parent '{parent}' is ambiguous — found {n} folders with that name across drives. Pass a UUID instead."
        ),
    }
}

/// Find string values that contain a *literal* backslash-escape sequence
/// (`\n`, `\t`, `\r` as two characters) rather than the control character.
///
/// This is almost always double encoding, not intent: a bash `"\n"` is two
/// characters, and when that text is handed to a program that JSON-encodes it
/// the backslash is escaped again, so the note is stored with the text `\n`
/// where its paragraphs should be. The reactor accepts it — it is a valid
/// string — and the write reports success, so nothing downstream catches it.
/// Refusing at the CLI is the only point where the mistake is both detectable
/// and cheap to fix. (A single-encoded `\n` in JSON text is fine: it parses
/// to a real newline and never reaches this check.)
///
/// Returns the JSON paths of every offending string so the error names the
/// exact field. Empty when the payload is clean.
pub fn find_literal_escapes(value: &serde_json::Value) -> Vec<String> {
    fn walk(v: &serde_json::Value, path: &str, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => {
                if s.contains("\\n") || s.contains("\\t") || s.contains("\\r") {
                    out.push(path.to_string());
                }
            }
            serde_json::Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    walk(item, &format!("{path}[{i}]"), out);
                }
            }
            serde_json::Value::Object(map) => {
                for (k, item) in map {
                    let next = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    walk(item, &next, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, "", &mut out);
    out
}

/// Refuse a write payload that contains literal escape sequences unless the
/// caller opted in. The message tells the agent what happened and how to fix
/// the *cause* (serialize with a real JSON tool), plus the override for the
/// rare case where a string legitimately contains the text `\n`.
pub fn reject_literal_escapes(value: &serde_json::Value, allow: bool) -> anyhow::Result<()> {
    if allow {
        return Ok(());
    }
    let hits = find_literal_escapes(value);
    if hits.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "Refusing to write: {} string field(s) contain a literal backslash-escape (the two characters \\n, \\t or \\r) instead of a real line break:\n  {}\n\
         Cause: the text already held the two characters backslash-n when it was JSON-encoded, so they were escaped again (double encoding). \
         Typical source: a bash \"\\n\" passed as an argument into a script that then serializes it.\n\
         Fix: make the string hold real line breaks BEFORE encoding — write the body in a file or heredoc (or build it inside Python), JSON-encode it once, and pass via --file / --input-file. Then read the document back and check for real newlines.\n\
         If the text genuinely must contain a literal backslash-n, re-run with --allow-literal-escapes.",
        hits.len(),
        hits.join("\n  ")
    );
}

#[cfg(test)]
mod literal_escape_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_payload_passes() {
        let v = json!([{ "type": "SET_CONTENT", "input": { "content": "line one\nline two", "updatedAt": "x" } }]);
        assert!(find_literal_escapes(&v).is_empty());
        assert!(reject_literal_escapes(&v, false).is_ok());
    }

    #[test]
    fn literal_backslash_n_is_reported_with_its_path() {
        // The string below contains a backslash followed by n — two characters.
        let v = json!([{ "type": "SET_CONTENT", "input": { "content": "## Claim\\n\\nBody", "updatedAt": "x" } }]);
        let hits = find_literal_escapes(&v);
        assert_eq!(hits, vec!["[0].input.content".to_string()]);
        let err = reject_literal_escapes(&v, false).unwrap_err().to_string();
        assert!(err.contains("[0].input.content"));
        assert!(err.contains("--allow-literal-escapes"));
    }

    #[test]
    fn override_flag_lets_it_through() {
        let v = json!({ "content": "shows the escape \\n in prose" });
        assert!(reject_literal_escapes(&v, true).is_ok());
    }

    #[test]
    fn nested_objects_and_tabs_are_found() {
        let v = json!({ "a": { "b": ["ok", "bad\\tcell"] }, "c": "fine" });
        assert_eq!(find_literal_escapes(&v), vec!["a.b[1]".to_string()]);
    }
}
