use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct DriveTree {
    /// The Switchboard GraphQL endpoint URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The profile name used to connect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub drives: Vec<DriveNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriveNode {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub document_type: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    pub file_count: usize,
    pub folder_count: usize,
    pub children: Vec<TreeEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum TreeEntry {
    #[serde(rename = "folder")]
    Folder(FolderNode),
    #[serde(rename = "file")]
    File(FileNode),
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderNode {
    pub id: String,
    pub name: String,
    pub children: Vec<TreeEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileNode {
    pub id: String,
    pub name: String,
    pub document_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

/// Build a hierarchical DriveTree from raw GraphQL data.
/// Each tuple is (drive_metadata_json, flat_nodes_array).
/// The revisions HashMap maps doc_id -> revision (enriched from per-model queries).
pub fn build_drive_tree(
    drives: &[(Value, Vec<Value>)],
    revisions: &HashMap<String, u64>,
) -> DriveTree {
    let mut drive_nodes = Vec::new();

    for (drive, nodes) in drives {
        let id = drive["id"].as_str().unwrap_or("").to_string();
        let name = drive["name"].as_str().unwrap_or("").to_string();
        let slug = drive["slug"].as_str().unwrap_or("").to_string();
        let document_type = drive["documentType"].as_str().unwrap_or("").to_string();
        let revision = drive["revision"].as_u64().unwrap_or(0);
        let editor = drive
            .pointer("/meta/preferredEditor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let children = build_children(nodes, None, revisions);
        let file_count = count_files(&children);
        let folder_count = count_folders(&children);

        drive_nodes.push(DriveNode {
            id,
            name,
            slug,
            document_type,
            revision,
            editor,
            file_count,
            folder_count,
            children,
        });
    }

    DriveTree {
        url: None,
        profile: None,
        drives: drive_nodes,
    }
}

/// Recursively build tree entries for nodes whose parentFolder matches `parent`.
/// `parent` is None for root-level nodes.
fn build_children(
    nodes: &[Value],
    parent: Option<&str>,
    revisions: &HashMap<String, u64>,
) -> Vec<TreeEntry> {
    let matching: Vec<&Value> = nodes
        .iter()
        .filter(|n| {
            let pf = n["parentFolder"].as_str().unwrap_or("");
            match parent {
                None => pf.is_empty(),
                Some(pid) => pf == pid,
            }
        })
        .collect();

    let mut entries = Vec::new();

    // Folders first
    for node in &matching {
        let kind = node["kind"].as_str().unwrap_or("");
        if kind == "folder" {
            let folder_id = node["id"].as_str().unwrap_or("");
            let name = node["name"].as_str().unwrap_or("").to_string();
            let children = build_children(nodes, Some(folder_id), revisions);
            entries.push(TreeEntry::Folder(FolderNode {
                id: folder_id.to_string(),
                name,
                children,
            }));
        }
    }

    // Then files
    for node in &matching {
        let kind = node["kind"].as_str().unwrap_or("");
        if kind != "folder" {
            let file_id = node["id"].as_str().unwrap_or("");
            let name = node["name"].as_str().unwrap_or("").to_string();
            let document_type = node["documentType"].as_str().unwrap_or("").to_string();
            let revision = revisions.get(file_id).copied();
            entries.push(TreeEntry::File(FileNode {
                id: file_id.to_string(),
                name,
                document_type,
                revision,
            }));
        }
    }

    entries
}

fn count_files(children: &[TreeEntry]) -> usize {
    let mut count = 0;
    for entry in children {
        match entry {
            TreeEntry::File(_) => count += 1,
            TreeEntry::Folder(f) => count += count_files(&f.children),
        }
    }
    count
}

fn count_folders(children: &[TreeEntry]) -> usize {
    let mut count = 0;
    for entry in children {
        if let TreeEntry::Folder(f) = entry {
            count += 1 + count_folders(&f.children);
        }
    }
    count
}
