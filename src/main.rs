//! mousefree — keyboard-driven mouse navigation for Wayland.
//!
//! Displays a full-screen overlay grid, lets the user select a cell with two
//! keystrokes, then performs mouse actions (click, drag, scroll) at that
//! location via a virtual pointer.

mod font;
mod input;
mod render;
mod wayland;

use anyhow::Context;
use tiny_skia::Pixmap;

use input::{AppState, SelectionPhase};
use wayland::{KeyEvent, WaylandBackend};

/// Pixels per pixel-perfect cursor nudge (Alt+hjkl).
const CURSOR_NUDGE_XS: i32 = 1;
/// Pixels per cursor nudge (hjkl).
const CURSOR_NUDGE_SM: i32 = 8;
/// Pixels per large cursor nudge (HJKL).
const CURSOR_NUDGE_MD: i32 = 16;
/// Pixels per extra-large cursor nudge (Ctrl+hjkl).
const CURSOR_NUDGE_LG: i32 = 32;

fn main() -> anyhow::Result<()> {
    font::init()?;

    let mut wl = WaylandBackend::new()?;
    let (w, h) = wl.screen_size();
    let mut pixmap = Pixmap::new(w, h).context("failed to create pixmap")?;
    let buf_size = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .context("screen dimensions too large for pixel buffer")?;
    let mut argb_buf = vec![0u8; buf_size];
    let mut state = AppState::new(w, h);

    draw(&mut wl, &mut pixmap, &mut argb_buf, &state)?;

    while let Some(key) = wl.next_key()? {
        match handle_key(&key, &mut state, &mut wl, w, h)? {
            KeyAction::Redraw => draw(&mut wl, &mut pixmap, &mut argb_buf, &state)?,
            KeyAction::Exit => {
                wl.exit()?;
                break;
            }
            KeyAction::None => {}
        }
    }
    Ok(())
}

/// Result of processing a single key event.
enum KeyAction {
    None,
    Redraw,
    Exit,
}

fn draw(
    wl: &mut WaylandBackend,
    pixmap: &mut Pixmap,
    argb_buf: &mut [u8],
    state: &AppState,
) -> anyhow::Result<()> {
    render::render_grid(pixmap, state);
    render::render_status_bar(pixmap, state);
    render::pixmap_to_argb8888(pixmap, argb_buf);
    let (w, h) = wl.screen_size();
    wl.present(argb_buf, w, h)
}

// ---------------------------------------------------------------------------
// Key dispatch — thin router that delegates to focused handlers.
// ---------------------------------------------------------------------------

fn handle_key(
    key: &KeyEvent,
    state: &mut AppState,
    wl: &mut WaylandBackend,
    w: u32,
    h: u32,
) -> anyhow::Result<KeyAction> {
    match key {
        KeyEvent::Close => {
            wl.wait_for_key_release()?;
            Ok(KeyAction::Exit)
        }

        KeyEvent::Undo => handle_undo(state, wl, w, h),

        // Nudge cursor (only when a cell is locked).
        KeyEvent::Char('h' | 'j' | 'k' | 'l' | 'H' | 'J' | 'K' | 'L')
        | KeyEvent::CtrlChar('h' | 'j' | 'k' | 'l')
        | KeyEvent::AltChar('h' | 'j' | 'k' | 'l')
            if state.phase.is_cell_selected() =>
        {
            handle_nudge(key, state, wl, w, h)
        }

        // Click / drag actions (only when a cell is locked).
        KeyEvent::Click
        | KeyEvent::DoubleClick
        | KeyEvent::TripleClick
        | KeyEvent::RightClick
        | KeyEvent::Char('/')
            if state.phase.is_cell_selected() =>
        {
            handle_action(key, state, wl)
        }

        // Two-key grid selection (only before a cell is locked).
        KeyEvent::Char(ch) if !state.phase.is_cell_selected() => {
            handle_grid_input(*ch, state, wl, w, h)
        }

        // Scroll passes through to the underlying window.
        KeyEvent::ScrollUp => {
            wl.scroll_up()?;
            Ok(KeyAction::Redraw)
        }
        KeyEvent::ScrollDown => {
            wl.scroll_down()?;
            Ok(KeyAction::Redraw)
        }
        KeyEvent::ScrollLeft => {
            wl.scroll_left()?;
            Ok(KeyAction::Redraw)
        }
        KeyEvent::ScrollRight => {
            wl.scroll_right()?;
            Ok(KeyAction::Redraw)
        }

        _ => Ok(KeyAction::None),
    }
}

// ---------------------------------------------------------------------------
// Focused handlers.
// ---------------------------------------------------------------------------

fn handle_undo(
    state: &mut AppState,
    wl: &mut WaylandBackend,
    w: u32,
    h: u32,
) -> anyhow::Result<KeyAction> {
    if let Some(prev) = state.phase.undo(w, h) {
        state.phase = prev;
        state.drag_origin = None;
        let (cx, cy) = state.phase.cursor();
        wl.move_mouse(cx, cy)?;
        Ok(KeyAction::Redraw)
    } else {
        // Already at Initial — back out of the overlay entirely.
        wl.wait_for_key_release()?;
        Ok(KeyAction::Exit)
    }
}

fn handle_nudge(
    key: &KeyEvent,
    state: &mut AppState,
    wl: &mut WaylandBackend,
    w: u32,
    h: u32,
) -> anyhow::Result<KeyAction> {
    let (dx, dy) = match key {
        KeyEvent::Char('h' | 'H') | KeyEvent::CtrlChar('h') | KeyEvent::AltChar('h') => (-1, 0),
        KeyEvent::Char('j' | 'J') | KeyEvent::CtrlChar('j') | KeyEvent::AltChar('j') => (0, 1),
        KeyEvent::Char('k' | 'K') | KeyEvent::CtrlChar('k') | KeyEvent::AltChar('k') => (0, -1),
        KeyEvent::Char('l' | 'L') | KeyEvent::CtrlChar('l') | KeyEvent::AltChar('l') => (1, 0),
        _ => unreachable!("handle_nudge called with non-hjkl key"),
    };
    let step = match key {
        KeyEvent::AltChar(_) => CURSOR_NUDGE_XS,
        KeyEvent::CtrlChar(_) => CURSOR_NUDGE_LG,
        KeyEvent::Char('H' | 'J' | 'K' | 'L') => CURSOR_NUDGE_MD,
        _ => CURSOR_NUDGE_SM,
    };
    state.phase = state.phase.nudge(dx * step, dy * step, w, h);
    let (cx, cy) = state.phase.cursor();
    wl.move_mouse(cx, cy)?;
    Ok(KeyAction::Redraw)
}

/// Handle click, double-click, right-click, and drag start/complete.
fn handle_action(
    key: &KeyEvent,
    state: &mut AppState,
    wl: &mut WaylandBackend,
) -> anyhow::Result<KeyAction> {
    let (cx, cy) = state.phase.cursor();

    // If already dragging, complete the drag on click or '/'.
    if let Some((ox, oy)) = state.drag_origin {
        if matches!(key, KeyEvent::Click | KeyEvent::Char('/')) {
            wl.wait_for_key_release()?;
            wl.drag_select(ox, oy, cx, cy)?;
            return Ok(KeyAction::Exit);
        }
    }

    match key {
        KeyEvent::Click => {
            wl.wait_for_key_release()?;
            wl.click(cx, cy)?;
            Ok(KeyAction::Exit)
        }
        KeyEvent::DoubleClick => {
            wl.wait_for_key_release()?;
            wl.double_click(cx, cy)?;
            Ok(KeyAction::Exit)
        }
        KeyEvent::TripleClick => {
            wl.wait_for_key_release()?;
            wl.triple_click(cx, cy)?;
            Ok(KeyAction::Exit)
        }
        KeyEvent::RightClick if !state.is_dragging() => {
            wl.wait_for_key_release()?;
            wl.right_click(cx, cy)?;
            Ok(KeyAction::Exit)
        }
        // '/' starts a drag: remember origin, reset selection for drop target.
        KeyEvent::Char('/') if !state.is_dragging() => {
            state.drag_origin = Some((cx, cy));
            let (w, h) = wl.screen_size();
            state.phase = SelectionPhase::new(w, h);
            Ok(KeyAction::Redraw)
        }
        _ => Ok(KeyAction::None),
    }
}

fn handle_grid_input(
    ch: char,
    state: &mut AppState,
    wl: &mut WaylandBackend,
    w: u32,
    h: u32,
) -> anyhow::Result<KeyAction> {
    // Second key: select row.  Checked FIRST so that overlapping col/row
    // keys (e.g. "aa", "ss") complete the cell rather than re-selecting
    // the column.
    if state.phase.pending_col().is_some() && input::is_row_key(ch) {
        if let Some(next) = state.phase.select_cell(ch, w, h) {
            state.phase = next;
            let (cx, cy) = state.phase.cursor();
            wl.move_mouse(cx, cy)?;
            return Ok(KeyAction::Redraw);
        }
    }

    // First key (or column re-selection if ch isn't a valid row key).
    if let Some(next) = state.phase.select_column(ch, w, h) {
        state.phase = next;
        let (cx, cy) = state.phase.cursor();
        wl.move_mouse(cx, cy)?;
        return Ok(KeyAction::Redraw);
    }

    Ok(KeyAction::None)
}
