//! Overlay rendering: hint-pill grid, crosshair, and status bar.
//!
//! All drawing targets a `tiny_skia::Pixmap` which is then converted to
//! Wayland's ARGB8888 format for presentation.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Shader, Stroke, Transform};

use crate::{
    font::font,
    input::{self, AppState, SelectionPhase},
};

// ===========================================================================
// Color palette.
//
// Design: "dark glass" — semi-transparent dark pills that read well over any
// background.  One accent (blue) for interactive states, warm amber for drag.
// Three-tier visual hierarchy: matched > default > dimmed.
// ===========================================================================

// -- Hint pills (two-letter bubbles on the grid) ----------------------------

const PILL_W: f32 = 36.0;
const PILL_H: f32 = 22.0;
const PILL_RADIUS: f32 = 6.0;
const PILL_FONT_SIZE: f32 = 14.0;
const PILL_LETTER_GAP: f32 = 2.0;
const PILL_BOLD_PASSES: i32 = 2;

const PILL_SHADOW_OFFSET: f32 = 1.5;
const PILL_BORDER_WIDTH: f32 = 1.0;

// Default state — dark glass with a hint of blue, bright border frames the text.
fn pill_bg() -> Color {
    Color::from_rgba8(16, 20, 38, 0xD8)
}
fn pill_border() -> Color {
    Color::from_rgba8(130, 135, 150, 0xB0)
}
fn pill_text() -> Color {
    Color::from_rgba8(240, 242, 250, 0xFF)
}

// Dimmed state — columns that don't match the pending selection fade away.
fn pill_dim_bg() -> Color {
    Color::from_rgba8(15, 17, 28, 0x20)
}
fn pill_dim_border() -> Color {
    Color::from_rgba8(15, 17, 28, 0x20)
}
fn pill_dim_text() -> Color {
    Color::from_rgba8(90, 95, 110, 0x30)
}

// Match state — the active column blazes with the accent blue.
fn pill_match_bg() -> Color {
    Color::from_rgba8(45, 110, 230, 0xF0)
}
fn pill_match_border() -> Color {
    Color::from_rgba8(110, 170, 255, 0xE0)
}
fn pill_match_text() -> Color {
    Color::from_rgba8(255, 255, 255, 0xFF)
}

fn pill_shadow() -> Color {
    Color::from_rgba8(0, 0, 0, 0x48)
}

// -- Crosshair (shown after cell selection) ---------------------------------

fn crosshair_color() -> Color {
    Color::from_rgba8(55, 120, 235, 0xFF) // same accent blue
}
fn crosshair_drag_color() -> Color {
    Color::from_rgba8(240, 175, 55, 0xFF) // warm amber
}
fn crosshair_outline() -> Color {
    Color::from_rgba8(0, 0, 0, 0xB0)
}
const CROSSHAIR_ARM: f32 = 20.0;
const CROSSHAIR_GAP: f32 = 4.0;
const CROSSHAIR_THICK: f32 = 2.0;

// -- Screen overlay ---------------------------------------------------------

fn screen_dim() -> Color {
    Color::from_rgba8(0, 0, 0, 0x30)
}

// -- Status bar (bottom-center help text) -----------------------------------

fn status_bg() -> Color {
    Color::from_rgba8(16, 18, 28, 0xE8)
}
/// Accent blue for key labels — matches the pill match / crosshair palette.
fn status_key_color() -> Color {
    Color::from_rgba8(110, 170, 255, 0xFF)
}
/// Muted text for action descriptions.
fn status_desc_color() -> Color {
    Color::from_rgba8(170, 175, 190, 0xD0)
}
/// Dim separator between key groups.
fn status_sep_color() -> Color {
    Color::from_rgba8(80, 85, 100, 0x80)
}
/// Bright highlight for the cell label / state indicator.
fn status_label_color() -> Color {
    Color::from_rgba8(255, 255, 255, 0xFF)
}
/// Warm amber for drag state indicator.
fn status_drag_color() -> Color {
    Color::from_rgba8(240, 175, 55, 0xFF)
}
const STATUS_FONT_SIZE: f32 = 15.5;
const STATUS_PAD_X: f32 = 14.0;
const STATUS_PAD_Y: f32 = 7.0;
const STATUS_BAR_RADIUS: f32 = 8.0;
const STATUS_BOLD_PASSES: i32 = 2;

// ===========================================================================
// Public rendering entry points.
// ===========================================================================

/// Renders either the hint-pill grid (pre-selection) or the crosshair
/// (post-selection) into `pixmap`.
pub fn render_grid(pixmap: &mut Pixmap, state: &AppState) {
    pixmap.fill(Color::TRANSPARENT);

    // Dim the desktop so the overlay elements stand out.
    let w = pixmap.width() as f32;
    let h = pixmap.height() as f32;
    fill_rect(pixmap, 0.0, 0.0, w, h, screen_dim());

    if state.phase.is_cell_selected() {
        draw_crosshair(pixmap, state.phase.cursor(), state.is_dragging());
    } else {
        draw_pill_grid(pixmap, &state.phase);
    }
}

/// Renders the context-sensitive help bar at the bottom of the screen.
pub fn render_status_bar(pixmap: &mut Pixmap, state: &AppState) {
    let segments = status_segments(state);
    let w = pixmap.width() as f32;
    let h = pixmap.height() as f32;

    let Some(ft) = font() else { return };

    // Pre-rasterize all glyphs to measure total width.
    let rasterized: Vec<_> = segments
        .iter()
        .map(|seg| {
            let glyphs: Vec<_> = seg
                .text
                .chars()
                .map(|ch| ft.rasterize(ch, STATUS_FONT_SIZE))
                .collect();
            let w: f32 = glyphs.iter().map(|(m, _)| m.advance_width).sum();
            (glyphs, w)
        })
        .collect();
    let text_w: f32 = rasterized.iter().map(|(_, w)| w).sum();

    let bar_h = STATUS_FONT_SIZE + STATUS_PAD_Y * 2.0;
    let bar_w = text_w + STATUS_PAD_X * 2.0;
    let bar_x = (w - bar_w) / 2.0;
    let bar_y = h - bar_h - 8.0;

    fill_rounded_rect(
        pixmap,
        bar_x,
        bar_y,
        bar_w,
        bar_h,
        STATUS_BAR_RADIUS,
        status_bg(),
    );

    let mut gx = bar_x + STATUS_PAD_X;
    let baseline_y = bar_y + STATUS_PAD_Y + STATUS_FONT_SIZE;

    for (i, seg) in segments.iter().enumerate() {
        let bold = seg.bold;
        let (ref glyphs, _) = rasterized[i];
        for (metrics, bitmap) in glyphs {
            let passes = if bold { STATUS_BOLD_PASSES } else { 1 };
            for off in 0..passes {
                blit_glyph(
                    pixmap,
                    gx + metrics.xmin as f32 + off as f32,
                    baseline_y - metrics.height as f32 - metrics.ymin as f32,
                    metrics.width,
                    metrics.height,
                    bitmap,
                    seg.color,
                );
            }
            gx += metrics.advance_width;
        }
    }
}

/// Convert a tiny_skia `Pixmap` (premultiplied RGBA) to Wayland's ARGB8888
/// format (premultiplied BGRA in memory on little-endian).
pub fn pixmap_to_argb8888(pixmap: &Pixmap, out: &mut [u8]) {
    let src = pixmap.data();
    assert_eq!(
        out.len(),
        src.len(),
        "output buffer size must match pixmap data size"
    );
    // Swap R and B channels: RGBA → BGRA.
    for (dst, src) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        dst[0] = src[2]; // B
        dst[1] = src[1]; // G
        dst[2] = src[0]; // R
        dst[3] = src[3]; // A
    }
}

// ===========================================================================
// Private drawing helpers.
// ===========================================================================

fn draw_crosshair(pixmap: &mut Pixmap, (cx, cy): (u32, u32), dragging: bool) {
    let cx = cx as f32;
    let cy = cy as f32;
    let color = if dragging {
        crosshair_drag_color()
    } else {
        crosshair_color()
    };
    let half = CROSSHAIR_THICK / 2.0;
    let arm = CROSSHAIR_ARM;
    let gap = CROSSHAIR_GAP;

    // Outline: slightly larger rectangles behind each arm.
    let outline = crosshair_outline();
    let o = 1.0; // outline padding
    fill_rect(
        pixmap,
        cx - arm - o,
        cy - half - o,
        arm - gap + o,
        CROSSHAIR_THICK + o * 2.0,
        outline,
    );
    fill_rect(
        pixmap,
        cx + gap,
        cy - half - o,
        arm - gap + o,
        CROSSHAIR_THICK + o * 2.0,
        outline,
    );
    fill_rect(
        pixmap,
        cx - half - o,
        cy - arm - o,
        CROSSHAIR_THICK + o * 2.0,
        arm - gap + o,
        outline,
    );
    fill_rect(
        pixmap,
        cx - half - o,
        cy + gap,
        CROSSHAIR_THICK + o * 2.0,
        arm - gap + o,
        outline,
    );

    // Colored arms.
    fill_rect(
        pixmap,
        cx - arm,
        cy - half,
        arm - gap,
        CROSSHAIR_THICK,
        color,
    );
    fill_rect(
        pixmap,
        cx + gap,
        cy - half,
        arm - gap,
        CROSSHAIR_THICK,
        color,
    );
    fill_rect(
        pixmap,
        cx - half,
        cy - arm,
        CROSSHAIR_THICK,
        arm - gap,
        color,
    );
    fill_rect(
        pixmap,
        cx - half,
        cy + gap,
        CROSSHAIR_THICK,
        arm - gap,
        color,
    );
}

fn draw_pill_grid(pixmap: &mut Pixmap, phase: &SelectionPhase) {
    let w = pixmap.width() as f32;
    let h = pixmap.height() as f32;
    let cols = input::grid_cols() as f32;
    let rows = input::grid_rows() as f32;
    let cell_w = w / cols;
    let cell_h = h / rows;
    let pending = phase.pending_col();

    for (row_idx, &row_key) in input::ROW_KEYS.iter().enumerate() {
        for (col_idx, &col_key) in input::COL_KEYS.iter().enumerate() {
            let cx = col_idx as f32 * cell_w + cell_w / 2.0;
            let cy = row_idx as f32 * cell_h + cell_h / 2.0;

            let col_match = pending == Some(col_key);
            let (bg, border, text) = match (pending.is_some(), col_match) {
                (true, false) => (pill_dim_bg(), pill_dim_border(), pill_dim_text()),
                (_, true) => (pill_match_bg(), pill_match_border(), pill_match_text()),
                _ => (pill_bg(), pill_border(), pill_text()),
            };

            draw_hint_pill(
                pixmap,
                cx,
                cy,
                col_key.to_ascii_uppercase(),
                row_key.to_ascii_uppercase(),
                bg,
                border,
                text,
            );
        }
    }
}

/// Draw a single hint pill: shadow, rounded-rect fill, border stroke, text.
#[allow(clippy::too_many_arguments)]
fn draw_hint_pill(
    pixmap: &mut Pixmap,
    cx: f32,
    cy: f32,
    ch1: char,
    ch2: char,
    bg: Color,
    border: Color,
    text_color: Color,
) {
    let px = cx - PILL_W / 2.0;
    let py = cy - PILL_H / 2.0;

    // Shadow (offset down-right).
    fill_rounded_rect(
        pixmap,
        px + PILL_SHADOW_OFFSET,
        py + PILL_SHADOW_OFFSET,
        PILL_W,
        PILL_H,
        PILL_RADIUS,
        pill_shadow(),
    );

    // Background fill.
    fill_rounded_rect(pixmap, px, py, PILL_W, PILL_H, PILL_RADIUS, bg);

    // Border stroke.
    stroke_rounded_rect(
        pixmap,
        px,
        py,
        PILL_W,
        PILL_H,
        PILL_RADIUS,
        PILL_BORDER_WIDTH,
        border,
    );

    // Text (two characters, faux-bold via horizontal repetition).
    let Some(ft) = font() else { return };
    let (m1, bm1) = ft.rasterize(ch1, PILL_FONT_SIZE);
    let (m2, bm2) = ft.rasterize(ch2, PILL_FONT_SIZE);
    let text_w = m1.advance_width + PILL_LETTER_GAP + m2.advance_width;
    let start_x = px + (PILL_W - text_w) / 2.0;
    let baseline_y = py + PILL_H / 2.0 + PILL_FONT_SIZE / 3.0;

    let g1x = start_x + m1.xmin as f32;
    let g1y = baseline_y - m1.height as f32 - m1.ymin as f32;
    let g2x = start_x + m1.advance_width + PILL_LETTER_GAP + m2.xmin as f32;
    let g2y = baseline_y - m2.height as f32 - m2.ymin as f32;

    for off in 0..PILL_BOLD_PASSES {
        blit_glyph(
            pixmap,
            g1x + off as f32,
            g1y,
            m1.width,
            m1.height,
            &bm1,
            text_color,
        );
        blit_glyph(
            pixmap,
            g2x + off as f32,
            g2y,
            m2.width,
            m2.height,
            &bm2,
            text_color,
        );
    }
}

/// A styled text segment in the status bar.
struct Span {
    text: String,
    color: Color,
    bold: bool,
}

impl Span {
    fn key(s: &str) -> Self {
        Self {
            text: s.to_string(),
            color: status_key_color(),
            bold: true,
        }
    }
    fn desc(s: &str) -> Self {
        Self {
            text: s.to_string(),
            color: status_desc_color(),
            bold: false,
        }
    }
    fn sep() -> Self {
        Self {
            text: " \u{00b7} ".to_string(), // middle dot
            color: status_sep_color(),
            bold: false,
        }
    }
    fn label(s: &str) -> Self {
        Self {
            text: s.to_string(),
            color: status_label_color(),
            bold: true,
        }
    }
    fn drag(s: &str) -> Self {
        Self {
            text: s.to_string(),
            color: status_drag_color(),
            bold: true,
        }
    }
}

/// Builds styled segments for the status bar.
fn status_segments(state: &AppState) -> Vec<Span> {
    let dragging = state.is_dragging();
    let phase = &state.phase;

    if phase.is_cell_selected() && dragging {
        let mut s = vec![
            Span::drag("DRAG "),
            Span::label(&phase.cell_label()),
            Span::sep(),
            Span::key("/ "),
            Span::key("Space"),
            Span::desc(" drop"),
        ];
        push_nudge_spans(&mut s);
        push_nav_spans(&mut s);
        s
    } else if phase.is_cell_selected() {
        let mut s = vec![
            Span::label(&phase.cell_label()),
            Span::sep(),
            Span::key("Space"),
            Span::desc(" click"),
            Span::sep(),
            Span::key("Enter"),
            Span::desc(" double"),
            Span::sep(),
            Span::key("S-Enter"),
            Span::desc(" triple"),
            Span::sep(),
            Span::key(" . "),
            Span::desc(" right"),
            Span::sep(),
            Span::key("/"),
            Span::desc(" drag"),
        ];
        push_nudge_spans(&mut s);
        push_nav_spans(&mut s);
        s
    } else if let Some(col) = phase.pending_col() {
        vec![
            Span::label(&format!("{col}_")),
            Span::desc(" type row key"),
            Span::sep(),
            Span::key("BS"),
            Span::desc(" back"),
            Span::sep(),
            Span::key("Esc"),
            Span::desc(" quit"),
        ]
    } else if dragging {
        vec![
            Span::drag("DRAG"),
            Span::desc("  type col+row to pick drop target"),
            Span::sep(),
            Span::key("Esc"),
            Span::desc(" quit"),
        ]
    } else {
        vec![
            Span::desc("type col+row to select"),
            Span::sep(),
            Span::key("Esc"),
            Span::desc(" quit"),
        ]
    }
}

fn push_nudge_spans(s: &mut Vec<Span>) {
    s.push(Span::sep());
    s.push(Span::key("hjkl"));
    s.push(Span::desc(" nudge"));
    s.push(Span::sep());
    s.push(Span::key("HJKL"));
    s.push(Span::desc(" big"));
    s.push(Span::sep());
    s.push(Span::key("C-hjkl"));
    s.push(Span::desc(" bigger"));
    s.push(Span::sep());
    s.push(Span::key("A-hjkl"));
    s.push(Span::desc(" 1px"));
}

fn push_nav_spans(s: &mut Vec<Span>) {
    s.push(Span::sep());
    s.push(Span::key("BS"));
    s.push(Span::desc(" back"));
    s.push(Span::sep());
    s.push(Span::key("Esc"));
    s.push(Span::desc(" quit"));
}

/// Plain-text version of the status bar content for testing.
#[cfg(test)]
pub(crate) fn status_text(state: &AppState) -> String {
    status_segments(state)
        .iter()
        .map(|s| s.text.as_str())
        .collect()
}

// ===========================================================================
// tiny_skia drawing primitives.
// ===========================================================================

fn paint(color: Color) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(color),
        anti_alias: true,
        ..Paint::default()
    }
}

fn fill_rect(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Color) {
    if let Some(rect) = Rect::from_xywh(x, y, w, h) {
        let path = PathBuilder::from_rect(rect);
        pixmap.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn fill_rounded_rect(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        pixmap.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn stroke_rounded_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: f32,
    color: Color,
) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }
}

/// Composite a fontdue glyph bitmap onto the pixmap. The bitmap contains
/// per-pixel coverage values; we blend them with `color` using standard
/// source-over compositing.
fn blit_glyph(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    gw: usize,
    gh: usize,
    bitmap: &[u8],
    color: Color,
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let ix = x.round() as i32;
    let iy = y.round() as i32;

    let data = pixmap.data_mut();
    let cr = (color.red() * 255.0 + 0.5) as u32;
    let cg = (color.green() * 255.0 + 0.5) as u32;
    let cb = (color.blue() * 255.0 + 0.5) as u32;
    let ca = (color.alpha() * 255.0 + 0.5) as u32;

    for row in 0..gh {
        let py = iy + row as i32;
        if py < 0 || py >= ph {
            continue;
        }
        for col in 0..gw {
            let px = ix + col as i32;
            if px < 0 || px >= pw {
                continue;
            }
            let coverage = bitmap[row * gw + col] as u32;
            if coverage == 0 {
                continue;
            }
            let off = (py as usize * pw as usize + px as usize) * 4;
            let dst = &mut data[off..off + 4];
            let src_a = (ca * coverage + 127) / 255;
            let inv = 255 - src_a;
            // tiny_skia stores premultiplied RGBA.
            dst[0] = ((cr * src_a + dst[0] as u32 * inv + 127) / 255) as u8;
            dst[1] = ((cg * src_a + dst[1] as u32 * inv + 127) / 255) as u8;
            dst[2] = ((cb * src_a + dst[2] as u32 * inv + 127) / 255) as u8;
            dst[3] = ((src_a * 255 + dst[3] as u32 * inv + 127) / 255) as u8;
        }
    }
}

// ===========================================================================
// Tests.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rect_path_builds_successfully() {
        assert!(rounded_rect_path(0.0, 0.0, 40.0, 24.0, 6.0).is_some());
    }

    #[test]
    fn rounded_rect_path_clamps_radius() {
        // Radius larger than half the dimension should still produce a path.
        assert!(rounded_rect_path(0.0, 0.0, 10.0, 10.0, 20.0).is_some());
    }

    #[test]
    fn fill_rect_does_not_panic_on_zero_size() {
        let mut pm = Pixmap::new(4, 4).unwrap();
        fill_rect(&mut pm, 0.0, 0.0, 0.0, 0.0, Color::BLACK);
    }

    #[test]
    fn fill_rect_paints_pixels() {
        let mut pm = Pixmap::new(4, 4).unwrap();
        fill_rect(
            &mut pm,
            0.0,
            0.0,
            4.0,
            4.0,
            Color::from_rgba8(255, 0, 0, 255),
        );
        // Center pixel should be opaque red (premultiplied RGBA).
        let off = (2 * 4 + 2) * 4;
        let px = &pm.data()[off..off + 4];
        assert_eq!(px, &[255, 0, 0, 255]);
    }

    #[test]
    fn blit_glyph_full_coverage_opaque() {
        let mut pm = Pixmap::new(4, 4).unwrap();
        blit_glyph(&mut pm, 0.0, 0.0, 1, 1, &[255], Color::WHITE);
        assert_eq!(&pm.data()[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn blit_glyph_zero_coverage_leaves_buffer_unchanged() {
        let mut pm = Pixmap::new(4, 4).unwrap();
        let before = pm.data().to_vec();
        blit_glyph(&mut pm, 0.0, 0.0, 1, 1, &[0], Color::WHITE);
        assert_eq!(pm.data(), &before[..]);
    }

    #[test]
    fn blit_glyph_negative_coords_do_not_panic() {
        let mut pm = Pixmap::new(4, 4).unwrap();
        blit_glyph(&mut pm, -5.0, -5.0, 2, 2, &[255; 4], Color::WHITE);
    }

    #[test]
    fn pixmap_to_argb8888_swaps_channels() {
        let mut pm = Pixmap::new(1, 1).unwrap();
        // Paint a single red pixel.
        fill_rect(
            &mut pm,
            0.0,
            0.0,
            1.0,
            1.0,
            Color::from_rgba8(255, 0, 0, 255),
        );
        let mut out = [0u8; 4];
        pixmap_to_argb8888(&pm, &mut out);
        // RGBA [255, 0, 0, 255] → BGRA [0, 0, 255, 255].
        assert_eq!(out, [0, 0, 255, 255]);
    }

    // -- Status text ----------------------------------------------------------

    #[test]
    fn status_text_initial() {
        let state = AppState::new(1920, 1080);
        let text = status_text(&state);
        assert!(text.contains("col+row"));
        assert!(text.contains("Esc"));
    }

    #[test]
    fn status_text_column_selected() {
        let mut state = AppState::new(1920, 1080);
        state.phase = state.phase.select_column('a', 1920, 1080).unwrap();
        let text = status_text(&state);
        assert!(text.contains("a_"));
        assert!(text.contains("row key"));
    }

    #[test]
    fn status_text_cell_selected() {
        let mut state = AppState::new(1920, 1080);
        state.phase = state.phase.select_column('a', 1920, 1080).unwrap();
        state.phase = state.phase.select_cell('w', 1920, 1080).unwrap();
        let text = status_text(&state);
        assert!(text.contains("aw"));
        assert!(text.contains("Space"));
        assert!(text.contains("click"));
        assert!(text.contains("triple"));
    }

    #[test]
    fn status_text_dragging() {
        let mut state = AppState::new(1920, 1080);
        state.drag_origin = Some((100, 200));
        let text = status_text(&state);
        assert!(text.contains("DRAG"));
    }

    #[test]
    fn status_text_drag_with_cell() {
        let mut state = AppState::new(1920, 1080);
        state.phase = state.phase.select_column('a', 1920, 1080).unwrap();
        state.phase = state.phase.select_cell('w', 1920, 1080).unwrap();
        state.drag_origin = Some((100, 200));
        let text = status_text(&state);
        assert!(text.contains("DRAG"));
        assert!(text.contains("drop"));
    }

    // -- Render smoke tests ---------------------------------------------------

    #[test]
    fn render_grid_initial_does_not_panic() {
        let mut pm = Pixmap::new(320, 240).unwrap();
        let state = AppState::new(320, 240);
        render_grid(&mut pm, &state);
    }

    #[test]
    fn render_grid_cell_selected_draws_crosshair() {
        let mut pm = Pixmap::new(320, 240).unwrap();
        let mut state = AppState::new(320, 240);
        state.phase = state.phase.select_column('a', 320, 240).unwrap();
        state.phase = state.phase.select_cell('w', 320, 240).unwrap();
        render_grid(&mut pm, &state);
        // The pixmap should have non-transparent pixels from the crosshair.
        assert!(pm.data().iter().any(|&b| b != 0));
    }
}
