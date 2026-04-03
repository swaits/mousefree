//! System font loading via fontconfig and fontdue.
//!
//! Discovers a sans-serif font using `fc-match`, loads it once at startup,
//! and exposes it for glyph rasterization during rendering.

use anyhow::{Context, Result};
use std::sync::OnceLock;

static FONT: OnceLock<fontdue::Font> = OnceLock::new();

/// Discover and load a system sans-serif font. Must be called once at startup.
pub fn init() -> Result<()> {
    let font = try_fc_match().context(
        "could not find a system font via fc-match. Is fontconfig installed? Try: fc-match sans-serif",
    )?;
    FONT.set(font).ok();
    Ok(())
}

/// Returns the loaded font, or `None` if [`init`] has not been called.
pub fn font() -> Option<&'static fontdue::Font> {
    FONT.get()
}

fn try_fc_match() -> Option<fontdue::Font> {
    let output = std::process::Command::new("fc-match")
        .args(["--format=%{file}", "sans-serif"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout);
    let data = std::fs::read(path.trim()).ok()?;
    fontdue::Font::from_bytes(data, fontdue::FontSettings::default()).ok()
}
