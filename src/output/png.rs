use std::sync::Arc;

use anyhow::{Result, bail};

/// Scale factor for high-quality PNG output.
/// 3x produces sharp, retina-quality text that remains crisp when zoomed in.
const SCALE: f32 = 3.0;

/// Render an SVG string to PNG bytes using resvg for rasterization.
/// Loads system fonts so that text renders correctly.
/// Renders at 3x scale for high-DPI / retina-quality output.
pub fn render_png(svg_str: &str) -> Result<Vec<u8>> {
    let mut fontdb = usvg::fontdb::Database::new();
    fontdb.load_system_fonts();

    let options = usvg::Options {
        fontdb: Arc::new(fontdb),
        ..Default::default()
    };

    let tree = usvg::Tree::from_str(svg_str, &options)
        .map_err(|e| anyhow::anyhow!("Failed to parse SVG: {e}"))?;

    let size = tree.size();
    let width = (size.width() * SCALE).ceil() as u32;
    let height = (size.height() * SCALE).ceil() as u32;

    if width == 0 || height == 0 {
        bail!("SVG has zero dimensions ({width}x{height})");
    }

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("Failed to create {width}x{height} pixmap"))?;

    let transform = tiny_skia::Transform::from_scale(SCALE, SCALE);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    pixmap
        .encode_png()
        .map_err(|e| anyhow::anyhow!("PNG encode failed: {e}"))
}
