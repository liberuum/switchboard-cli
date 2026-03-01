mod json;
pub mod mermaid;
pub mod png;
pub mod svg;
pub mod table;
pub mod tree;

pub use json::print_json;
pub use mermaid::render_mermaid;
pub use table::print_table;
pub use tree::{DocStateView, DriveTree, TreeEntry, build_drive_tree};

use std::io::IsTerminal;

use anyhow::{Result, bail};
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Raw,
    Svg,
    Png,
    Mermaid,
}

impl OutputFormat {
    /// Returns true if this format produces visual output (SVG/PNG/Mermaid)
    pub fn is_visual(&self) -> bool {
        matches!(self, Self::Svg | Self::Png | Self::Mermaid)
    }

    /// Returns the default file extension for visual formats.
    pub fn default_extension(&self) -> Option<&'static str> {
        match self {
            Self::Svg => Some("svg"),
            Self::Png => Some("png"),
            Self::Mermaid => Some("mmd"),
            _ => None,
        }
    }
}

/// Resolve the output path for visual formats.
/// If `--out` was provided, use that. Otherwise, generate a default filename
/// like `switchboard-visualize.svg` from the command name and format extension.
pub fn resolve_visual_output(
    out: Option<&str>,
    format: OutputFormat,
    command: &str,
) -> Option<String> {
    if let Some(path) = out {
        return Some(path.to_string());
    }
    format
        .default_extension()
        .map(|ext| format!("switchboard-{command}.{ext}"))
}

/// Write bytes to a file or stdout.
/// For PNG, bail if stdout is a TTY and no --out given.
pub fn write_output(data: &[u8], out_path: Option<&str>, is_binary: bool) -> Result<()> {
    if is_binary && out_path.is_none() && std::io::stdout().is_terminal() {
        bail!("Binary output (PNG) requires --out <file>. Example: --format png --out diagram.png");
    }

    match out_path {
        Some(path) => {
            std::fs::write(path, data)?;
            eprintln!("Wrote {path}");
            Ok(())
        }
        None => {
            use std::io::Write;
            std::io::stdout().write_all(data)?;
            Ok(())
        }
    }
}
