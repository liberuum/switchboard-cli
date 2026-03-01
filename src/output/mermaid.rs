use crate::output::tree::{DriveTree, TreeEntry};

/// Sanitize an ID for use as a Mermaid node identifier.
/// Mermaid node IDs cannot contain hyphens.
fn mermaid_id(id: &str) -> String {
    id.replace('-', "_")
}

/// Escape a name for use inside a Mermaid node label.
/// Double quotes must become `&quot;` to avoid breaking the label syntax.
fn escape_label(name: &str) -> String {
    name.replace('"', "&quot;")
}

/// Render a `DriveTree` as Mermaid flowchart markup.
///
/// The output is a self-contained `graph TD` block that can be pasted into
/// GitHub README, Notion, or any tool that renders Mermaid diagrams.
pub fn render_mermaid(tree: &DriveTree) -> String {
    let mut lines = Vec::new();
    lines.push("graph TD".to_string());

    for drive in &tree.drives {
        let nid = mermaid_id(&drive.id);
        let label = escape_label(&drive.name);

        // Drive node definition
        lines.push(format!(
            "    {nid}[\"\u{1F5C4} {label}<br/><small>{slug} \u{00B7} rev:{rev}</small>\"]",
            slug = escape_label(&drive.slug),
            rev = drive.revision,
        ));
        lines.push(format!(
            "    style {nid} fill:#14151A,stroke:#04D9EB,color:#FFFFFF"
        ));

        // Recurse into children
        emit_children(&mut lines, &nid, &drive.children);
    }

    lines.join("\n")
}

/// Recursively emit Mermaid node definitions and edges for a list of tree entries.
fn emit_children(lines: &mut Vec<String>, parent_id: &str, children: &[TreeEntry]) {
    for entry in children {
        match entry {
            TreeEntry::Folder(folder) => {
                let nid = mermaid_id(&folder.id);
                let label = escape_label(&folder.name);

                lines.push(format!(
                    "    {nid}[\"\u{1F4C1} {label}\"]"
                ));
                lines.push(format!(
                    "    style {nid} fill:#14151A,stroke:#7A3AFF,color:#FFFFFF"
                ));
                lines.push(format!("    {parent_id} --> {nid}"));

                emit_children(lines, &nid, &folder.children);
            }
            TreeEntry::File(file) => {
                let nid = mermaid_id(&file.id);
                let label = escape_label(&file.name);
                let doc_type = escape_label(&file.document_type);

                let meta = match file.revision {
                    Some(rev) => format!("{doc_type} \u{00B7} rev:{rev}"),
                    None => doc_type,
                };

                lines.push(format!(
                    "    {nid}[\"\u{1F4C4} {label}<br/><small>{meta}</small>\"]"
                ));
                lines.push(format!(
                    "    style {nid} fill:#14151A,stroke:#07C262,color:#FFFFFF"
                ));
                lines.push(format!("    {parent_id} --> {nid}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::tree::{DriveNode, FileNode, FolderNode};

    #[test]
    fn mermaid_id_replaces_hyphens() {
        assert_eq!(mermaid_id("abc-def-123"), "abc_def_123");
        assert_eq!(mermaid_id("no_hyphens"), "no_hyphens");
    }

    #[test]
    fn escape_label_handles_quotes() {
        assert_eq!(escape_label(r#"Say "hello""#), "Say &quot;hello&quot;");
    }

    #[test]
    fn render_empty_tree() {
        let tree = DriveTree { url: None, profile: None, drives: vec![] };
        assert_eq!(render_mermaid(&tree), "graph TD");
    }

    #[test]
    fn render_single_drive_with_file_and_folder() {
        let tree = DriveTree {
            url: None,
            profile: None,
            drives: vec![DriveNode {
                id: "d-1".into(),
                name: "My Drive".into(),
                slug: "my-drive".into(),
                document_type: "powerhouse/document-model".into(),
                revision: 48,
                editor: None,
                file_count: 1,
                folder_count: 1,
                children: vec![
                    TreeEntry::Folder(FolderNode {
                        id: "f-1".into(),
                        name: "Docs".into(),
                        children: vec![TreeEntry::File(FileNode {
                            id: "file-1".into(),
                            name: "README".into(),
                            document_type: "makerdao/atlas".into(),
                            revision: Some(14),
                        })],
                    }),
                    TreeEntry::File(FileNode {
                        id: "file-2".into(),
                        name: "Budget".into(),
                        document_type: "makerdao/budget".into(),
                        revision: None,
                    }),
                ],
            }],
        };

        let output = render_mermaid(&tree);

        // Verify structure
        assert!(output.starts_with("graph TD"));

        // Drive node
        assert!(output.contains("d_1["));
        assert!(output.contains("My Drive"));
        assert!(output.contains("my-drive"));
        assert!(output.contains("rev:48"));
        assert!(output.contains("style d_1 fill:#14151A,stroke:#04D9EB,color:#FFFFFF"));

        // Folder node
        assert!(output.contains("f_1["));
        assert!(output.contains("Docs"));
        assert!(output.contains("style f_1 fill:#14151A,stroke:#7A3AFF,color:#FFFFFF"));
        assert!(output.contains("d_1 --> f_1"));

        // File inside folder
        assert!(output.contains("file_1["));
        assert!(output.contains("README"));
        assert!(output.contains("makerdao/atlas"));
        assert!(output.contains("rev:14"));
        assert!(output.contains("style file_1 fill:#14151A,stroke:#07C262,color:#FFFFFF"));
        assert!(output.contains("f_1 --> file_1"));

        // File at drive root
        assert!(output.contains("file_2["));
        assert!(output.contains("Budget"));
        assert!(output.contains("d_1 --> file_2"));
    }
}
