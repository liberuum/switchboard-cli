use crate::output::tree::{DriveNode, DriveTree, TreeEntry};

// ── Powerhouse brand theme ──────────────────────────────────────────────────

const BG_COLOR: &str = "#0E0E0D";
const SURFACE_COLOR: &str = "#14151A";
const BORDER_COLOR: &str = "rgba(255,255,255,0.14)";
const TEXT_PRIMARY: &str = "#FFFFFF";
const TEXT_SECONDARY: &str = "#B5B5B5";
const TEXT_TERTIARY: &str = "#646464";
const DRIVE_ACCENT: &str = "#04D9EB";
const FOLDER_ACCENT: &str = "#7A3AFF";
const DOC_ACCENT: &str = "#07C262";
const LINE_COLOR: &str = "#0285FF";
const FONT_FAMILY: &str = "Inter, system-ui, -apple-system, sans-serif";

// ── Layout constants ────────────────────────────────────────────────────────

const MIN_NODE_WIDTH: f64 = 320.0;
const GAP_Y: f64 = 16.0;
const INDENT_X: f64 = 36.0;
const DRIVE_GAP_X: f64 = 48.0;
const PADDING: f64 = 32.0;
const CORNER_RADIUS: f64 = 8.0;
const ACCENT_BAR_W: f64 = 4.0;

// Text layout
const TITLE_FONT: f64 = 14.0;
const META_FONT: f64 = 11.0;
const TITLE_Y: f64 = 22.0; // baseline offset from top of card
const FIRST_META_Y: f64 = 40.0; // first metadata line baseline
const LINE_H: f64 = 16.0; // spacing between metadata lines
const BOTTOM_PAD: f64 = 10.0; // padding below last line
const DRIVE_PAD_X: f64 = 12.0; // inner horizontal padding for drives
const CHILD_PAD_X: f64 = 16.0; // inner horizontal padding for files/folders (after accent bar)
const PAD_RIGHT: f64 = 12.0;
const HEADER_HEIGHT: f64 = 80.0; // space reserved for top header

// ── Internal layout types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum NodeKind {
    Drive,
    Folder,
    File,
}

#[derive(Debug)]
struct MetaLine {
    label: String,
    value: String,
}

#[derive(Debug)]
struct LayoutItem {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    kind: NodeKind,
    title: String,
    meta: Vec<MetaLine>,
    parent_x: Option<f64>,
    parent_y: Option<f64>,
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Render a `DriveTree` as a self-contained SVG string.
pub fn render_svg(tree: &DriveTree) -> String {
    // Compute header height (present when url/profile is set)
    let has_header = tree.url.is_some() || tree.profile.is_some();
    let header_h = if has_header { HEADER_HEIGHT } else { 0.0 };

    // Pass 1: compute column widths, then lay out items
    let mut all_items: Vec<LayoutItem> = Vec::new();
    let mut column_x = PADDING;
    let mut max_height: f64 = 0.0;

    for drive in &tree.drives {
        let col_width = compute_column_width(drive);
        let mut cursor_y = PADDING + header_h;
        let items = layout_drive(drive, column_x, &mut cursor_y, col_width);
        max_height = max_height.max(cursor_y);
        all_items.extend(items);
        column_x += col_width + DRIVE_GAP_X;
    }

    let total_width = if tree.drives.is_empty() {
        PADDING * 2.0
    } else {
        column_x - DRIVE_GAP_X + PADDING
    };
    let total_height = max_height + PADDING;

    // Pass 2: render SVG
    let mut svg = String::with_capacity(8192);

    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{total_width}" height="{total_height}" viewBox="0 0 {total_width} {total_height}">"#,
    ));
    svg.push_str(&format!(
        r#"<rect width="100%" height="100%" fill="{BG_COLOR}"/>"#,
    ));
    svg.push_str(&format!(
        r#"<style>text {{ font-family: {FONT_FAMILY}; }}</style>"#,
    ));

    // Header with instance info
    if has_header {
        render_header(&mut svg, tree, total_width);
    }

    // Connecting lines (render first so they appear behind nodes)
    for item in &all_items {
        if let (Some(px), Some(py)) = (item.parent_x, item.parent_y) {
            let cx = item.x + item.width / 2.0;
            let cy = item.y;
            let mid_y = (py + cy) / 2.0;
            svg.push_str(&format!(
                r#"<path d="M {px} {py} C {px} {mid_y}, {cx} {mid_y}, {cx} {cy}" fill="none" stroke="{LINE_COLOR}" stroke-width="1.5" stroke-opacity="0.6"/>"#,
            ));
        }
    }

    // Nodes
    for item in &all_items {
        render_node(&mut svg, item);
    }

    svg.push_str("</svg>");
    svg
}

// ── Width computation ──────────────────────────────────────────────────────

/// Rough per-character width estimate for proportional fonts.
fn estimate_text_width(text: &str, font_size: f64) -> f64 {
    text.chars().count() as f64 * font_size * 0.6
}

/// Walk a drive and all its children to find the required column width.
fn compute_column_width(drive: &DriveNode) -> f64 {
    let mut max_w = 0.0f64;

    // Drive title
    max_w = max_w.max(estimate_text_width(&drive.name, TITLE_FONT) + DRIVE_PAD_X + PAD_RIGHT);

    // Drive metadata lines
    for ml in drive_meta(drive) {
        let line = format!("{}: {}", ml.label, ml.value);
        max_w = max_w.max(estimate_text_width(&line, META_FONT) + DRIVE_PAD_X + PAD_RIGHT);
    }

    // Walk children
    walk_children_width(&drive.children, 1, &mut max_w);

    max_w.max(MIN_NODE_WIDTH)
}

fn walk_children_width(children: &[TreeEntry], depth: usize, max_w: &mut f64) {
    let indent = INDENT_X * depth as f64;

    for entry in children {
        match entry {
            TreeEntry::Folder(folder) => {
                let pad = indent + CHILD_PAD_X + PAD_RIGHT;
                *max_w = max_w.max(estimate_text_width(&folder.name, 13.0) + pad);
                let id_line = format!("ID: {}", folder.id);
                *max_w = max_w.max(estimate_text_width(&id_line, META_FONT) + pad);
                walk_children_width(&folder.children, depth + 1, max_w);
            }
            TreeEntry::File(file) => {
                let pad = indent + CHILD_PAD_X + PAD_RIGHT;
                *max_w = max_w.max(estimate_text_width(&file.name, 13.0) + pad);
                for ml in file_meta(file) {
                    let line = format!("{}: {}", ml.label, ml.value);
                    *max_w = max_w.max(estimate_text_width(&line, META_FONT) + pad);
                }
            }
        }
    }
}

// ── Metadata builders ──────────────────────────────────────────────────────

fn drive_meta(drive: &DriveNode) -> Vec<MetaLine> {
    let mut lines = vec![
        MetaLine {
            label: "ID".into(),
            value: drive.id.clone(),
        },
        MetaLine {
            label: "Slug".into(),
            value: drive.slug.clone(),
        },
        MetaLine {
            label: "Revision".into(),
            value: drive.revision.to_string(),
        },
        MetaLine {
            label: "Type".into(),
            value: drive.document_type.clone(),
        },
    ];
    if let Some(ref editor) = drive.editor {
        lines.push(MetaLine {
            label: "Editor".into(),
            value: editor.clone(),
        });
    }
    lines.push(MetaLine {
        label: "Contents".into(),
        value: format!("{} files, {} folders", drive.file_count, drive.folder_count),
    });
    lines
}

fn file_meta(file: &crate::output::tree::FileNode) -> Vec<MetaLine> {
    let mut lines = vec![
        MetaLine {
            label: "ID".into(),
            value: file.id.clone(),
        },
        MetaLine {
            label: "Type".into(),
            value: file.document_type.clone(),
        },
    ];
    if let Some(rev) = file.revision {
        lines.push(MetaLine {
            label: "Revision".into(),
            value: rev.to_string(),
        });
    }
    lines
}

fn folder_meta(folder: &crate::output::tree::FolderNode) -> Vec<MetaLine> {
    vec![MetaLine {
        label: "ID".into(),
        value: folder.id.clone(),
    }]
}

// ── Layout helpers ──────────────────────────────────────────────────────────

/// Compute the height of a node card given its metadata line count.
fn node_height(meta_lines: usize) -> f64 {
    FIRST_META_Y + (meta_lines as f64 - 1.0) * LINE_H + BOTTOM_PAD
}

/// Lay out a single drive and all its descendants.
fn layout_drive(
    drive: &DriveNode,
    column_x: f64,
    cursor_y: &mut f64,
    col_width: f64,
) -> Vec<LayoutItem> {
    let mut items = Vec::new();

    let meta = drive_meta(drive);
    let h = node_height(meta.len());
    let drive_y = *cursor_y;

    items.push(LayoutItem {
        x: column_x,
        y: drive_y,
        width: col_width,
        height: h,
        kind: NodeKind::Drive,
        title: drive.name.clone(),
        meta,
        parent_x: None,
        parent_y: None,
    });

    *cursor_y += h + GAP_Y;

    let parent_cx = column_x + col_width / 2.0;
    let parent_bottom = drive_y + h;
    layout_children(
        &drive.children,
        column_x,
        cursor_y,
        1,
        parent_cx,
        parent_bottom,
        col_width,
        &mut items,
    );

    items
}

/// Recursively lay out children at a given depth.
#[allow(clippy::too_many_arguments)]
fn layout_children(
    children: &[TreeEntry],
    column_x: f64,
    cursor_y: &mut f64,
    depth: usize,
    parent_cx: f64,
    parent_bottom: f64,
    col_width: f64,
    items: &mut Vec<LayoutItem>,
) {
    let indent = INDENT_X * depth as f64;

    for entry in children {
        match entry {
            TreeEntry::Folder(folder) => {
                let x = column_x + indent;
                let w = col_width - indent;
                let y = *cursor_y;
                let meta = folder_meta(folder);
                let h = node_height(meta.len());

                items.push(LayoutItem {
                    x,
                    y,
                    width: w,
                    height: h,
                    kind: NodeKind::Folder,
                    title: folder.name.clone(),
                    meta,
                    parent_x: Some(parent_cx),
                    parent_y: Some(parent_bottom),
                });

                *cursor_y += h + GAP_Y;

                let folder_cx = x + w / 2.0;
                let folder_bottom = y + h;
                layout_children(
                    &folder.children,
                    column_x,
                    cursor_y,
                    depth + 1,
                    folder_cx,
                    folder_bottom,
                    col_width,
                    items,
                );
            }
            TreeEntry::File(file) => {
                let x = column_x + indent;
                let w = col_width - indent;
                let y = *cursor_y;
                let meta = file_meta(file);
                let h = node_height(meta.len());

                items.push(LayoutItem {
                    x,
                    y,
                    width: w,
                    height: h,
                    kind: NodeKind::File,
                    title: file.name.clone(),
                    meta,
                    parent_x: Some(parent_cx),
                    parent_y: Some(parent_bottom),
                });

                *cursor_y += h + GAP_Y;
            }
        }
    }
}

// ── Rendering helpers ───────────────────────────────────────────────────────

fn render_header(svg: &mut String, tree: &DriveTree, _total_width: f64) {
    let x = PADDING;

    // Title: "Switchboard"
    svg.push_str(&format!(
        r#"<text x="{x}" y="{y}" fill="{DRIVE_ACCENT}" font-size="20" font-weight="700">Switchboard</text>"#,
        y = PADDING + 6.0,
    ));

    // URL (below title)
    if let Some(ref url) = tree.url {
        svg.push_str(&format!(
            r#"<text x="{x}" y="{y}" fill="{TEXT_SECONDARY}" font-size="12">{url}</text>"#,
            y = PADDING + 28.0,
            url = escape_xml(url),
        ));
    }

    // Caption with profile name and drive count
    let drive_count = tree.drives.len();
    let caption = match tree.profile {
        Some(ref p) => format!(
            "Drive and document structure for profile '{}' \u{00B7} {} drive{}",
            p,
            drive_count,
            if drive_count == 1 { "" } else { "s" }
        ),
        None => format!(
            "{} drive{}",
            drive_count,
            if drive_count == 1 { "" } else { "s" }
        ),
    };
    svg.push_str(&format!(
        r#"<text x="{x}" y="{y}" fill="{TEXT_TERTIARY}" font-size="11">{caption}</text>"#,
        y = PADDING + 46.0,
        caption = escape_xml(&caption),
    ));

    // Separator line
    svg.push_str(&format!(
        r#"<line x1="{x}" y1="{y}" x2="99%" y2="{y}" stroke="{BORDER_COLOR}" stroke-width="1"/>"#,
        y = PADDING + 60.0,
    ));
}

fn render_node(svg: &mut String, item: &LayoutItem) {
    let x = item.x;
    let y = item.y;
    let w = item.width;
    let h = item.height;

    // Card background
    svg.push_str(&format!(
        r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{CORNER_RADIUS}" fill="{SURFACE_COLOR}" stroke="{BORDER_COLOR}"/>"#,
    ));

    // Accent bar
    match item.kind {
        NodeKind::Drive => {
            // Cyan top accent bar
            svg.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{w}" height="{ACCENT_BAR_W}" rx="{CORNER_RADIUS}" fill="{DRIVE_ACCENT}"/>"#,
            ));
        }
        NodeKind::Folder => {
            // Purple left accent bar
            svg.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{ACCENT_BAR_W}" height="{h}" rx="2" fill="{FOLDER_ACCENT}"/>"#,
            ));
        }
        NodeKind::File => {
            // Green left accent bar
            svg.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="{ACCENT_BAR_W}" height="{h}" rx="2" fill="{DOC_ACCENT}"/>"#,
            ));
        }
    }

    // Text start position
    let text_x = match item.kind {
        NodeKind::Drive => x + DRIVE_PAD_X,
        NodeKind::Folder | NodeKind::File => x + CHILD_PAD_X,
    };

    // Title line — no emoji icons (resvg lacks emoji fonts, causing ? in PNG)
    // Node types are already distinguished by accent bar color: cyan=drive, purple=folder, green=file
    svg.push_str(&format!(
        r#"<text x="{text_x}" y="{ty}" fill="{TEXT_PRIMARY}" font-size="{TITLE_FONT}" font-weight="600">{label}</text>"#,
        ty = y + TITLE_Y,
        label = escape_xml(&item.title),
    ));

    // Metadata lines — label in tertiary, value in secondary
    let mut line_y = y + FIRST_META_Y;
    for ml in &item.meta {
        svg.push_str(&format!(
            r#"<text x="{text_x}" y="{line_y}" font-size="{META_FONT}"><tspan fill="{TEXT_TERTIARY}">{label}: </tspan><tspan fill="{TEXT_SECONDARY}">{value}</tspan></text>"#,
            label = escape_xml(&ml.label),
            value = escape_xml(&ml.value),
        ));
        line_y += LINE_H;
    }
}

/// XML-escape text content for safe embedding in SVG.
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::tree::{DriveNode, FileNode, FolderNode};

    #[test]
    fn escape_xml_special_chars() {
        assert_eq!(
            escape_xml("a & b < c > d \"e\""),
            "a &amp; b &lt; c &gt; d &quot;e&quot;"
        );
    }

    #[test]
    fn render_empty_tree() {
        let tree = DriveTree {
            url: None,
            profile: None,
            drives: vec![],
        };
        let svg = render_svg(&tree);
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains(BG_COLOR));
    }

    #[test]
    fn render_single_drive_with_children() {
        let tree = DriveTree {
            url: Some("https://switchboard.example.com/graphql".into()),
            profile: Some("test".into()),
            drives: vec![DriveNode {
                id: "31739b03-aaaa-bbbb-cccc-dddddddddddd".into(),
                name: "My Drive".into(),
                slug: "my-drive".into(),
                document_type: "powerhouse/document-model".into(),
                revision: 48,
                editor: Some("document-model-editor".into()),
                file_count: 2,
                folder_count: 1,
                children: vec![
                    TreeEntry::Folder(FolderNode {
                        id: "f1f1f1f1-aaaa-bbbb-cccc-dddddddddddd".into(),
                        name: "Docs".into(),
                        children: vec![TreeEntry::File(FileNode {
                            id: "92a6e064-aaaa-bbbb-cccc-dddddddddddd".into(),
                            name: "Profile".into(),
                            document_type: "powerhouse/builder-profile".into(),
                            revision: Some(14),
                        })],
                    }),
                    TreeEntry::File(FileNode {
                        id: "aabbccdd-1111-2222-3333-444444444444".into(),
                        name: "Budget".into(),
                        document_type: "makerdao/budget".into(),
                        revision: None,
                    }),
                ],
            }],
        };

        let svg = render_svg(&tree);

        // SVG structure
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("My Drive"));
        assert!(svg.contains("my-drive"));
        assert!(svg.contains("48")); // revision
        assert!(svg.contains("document-model-editor"));
        assert!(svg.contains("2 files, 1 folders"));
        // Full UUID, not truncated
        assert!(svg.contains("31739b03-aaaa-bbbb-cccc-dddddddddddd"));

        // Folder
        assert!(svg.contains("Docs"));
        assert!(svg.contains(FOLDER_ACCENT));
        assert!(svg.contains("f1f1f1f1-aaaa-bbbb-cccc-dddddddddddd"));

        // File with revision
        assert!(svg.contains("Profile"));
        assert!(svg.contains("powerhouse/builder-profile"));
        assert!(svg.contains("92a6e064-aaaa-bbbb-cccc-dddddddddddd"));

        // File without revision
        assert!(svg.contains("Budget"));
        assert!(svg.contains("makerdao/budget"));

        // Labeled metadata
        assert!(svg.contains("ID: "));
        assert!(svg.contains("Slug: "));
        assert!(svg.contains("Revision: "));
        assert!(svg.contains("Type: "));
        assert!(svg.contains("Editor: "));
        assert!(svg.contains("Contents: "));

        // Connecting lines
        assert!(svg.contains(LINE_COLOR));
        assert!(svg.contains("stroke-opacity"));
    }
}
