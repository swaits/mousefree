//! Grid selection state machine and application state.
//!
//! Tracks the user's progress through column selection, row selection, and
//! post-selection actions (nudge, undo). The key arrays define the grid layout.

/// Keys displayed along the top axis of the grid (columns).
///
/// Letters only — no digits — so every pill is unambiguously two letters.
pub const COL_KEYS: &[char] = &['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'p'];

/// Keys displayed along the side axis of the grid (rows).
///
/// Inspired by Vimium's easy-to-reach character set.  Overlaps with column
/// keys on purpose — the state machine disambiguates (first press = column,
/// second = row), so combos like "aa" and "ss" are valid and fast to type.
/// Ordered by keyboard row (top → home → bottom ≈ screen top → bottom).
pub const ROW_KEYS: &[char] = &[
    'w', 'e', 'r', 't', 'a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'p', 'c', 'm',
];

/// Returns the number of columns in the selection grid.
pub fn grid_cols() -> u32 {
    COL_KEYS.len() as u32
}

/// Returns the number of rows in the selection grid.
pub fn grid_rows() -> u32 {
    ROW_KEYS.len() as u32
}

#[cfg(test)]
fn is_col_key(ch: char) -> bool {
    COL_KEYS.contains(&ch)
}

/// Returns `true` if `ch` is a valid row-selection key.
pub fn is_row_key(ch: char) -> bool {
    ROW_KEYS.contains(&ch)
}

// ---------------------------------------------------------------------------
// Selection phase — enforces valid state transitions at the type level.
// ---------------------------------------------------------------------------

/// Tracks how far the user has progressed through the two-key grid selection.
#[derive(Clone, Debug)]
pub enum SelectionPhase {
    /// No key pressed yet; cursor sits at screen center.
    Initial { cursor: (u32, u32) },
    /// Column key pressed; waiting for the row key.
    ColumnSelected { col: char, cursor: (u32, u32) },
    /// Both keys pressed; cell is locked and cursor is centered in it.
    CellSelected {
        col: char,
        row: char,
        cursor: (u32, u32),
    },
}

impl SelectionPhase {
    pub fn new(w: u32, h: u32) -> Self {
        Self::Initial {
            cursor: (w / 2, h / 2),
        }
    }

    pub fn cursor(&self) -> (u32, u32) {
        match *self {
            Self::Initial { cursor }
            | Self::ColumnSelected { cursor, .. }
            | Self::CellSelected { cursor, .. } => cursor,
        }
    }

    pub fn is_cell_selected(&self) -> bool {
        matches!(self, Self::CellSelected { .. })
    }

    /// Returns the pending column key, if in the `ColumnSelected` phase.
    pub fn pending_col(&self) -> Option<char> {
        match *self {
            Self::ColumnSelected { col, .. } => Some(col),
            _ => None,
        }
    }

    /// Builds the two-character label shown in the status bar (e.g. "AQ").
    pub fn cell_label(&self) -> String {
        match *self {
            Self::CellSelected { col, row, .. } => {
                let mut s = String::with_capacity(2);
                s.push(col);
                s.push(row);
                s
            }
            _ => String::new(),
        }
    }

    /// Transition: accept a column key, snap cursor horizontally.
    pub fn select_column(&self, col: char, w: u32, h: u32) -> Option<Self> {
        let col_idx = COL_KEYS.iter().position(|&c| c == col)? as u32;
        let cols = grid_cols();
        if cols == 0 || w < cols {
            return None;
        }
        let cell_w = w / cols;
        Some(Self::ColumnSelected {
            col,
            cursor: (col_idx * cell_w + cell_w / 2, h / 2),
        })
    }

    /// Transition: accept a row key (must already have a column), lock the cell.
    pub fn select_cell(&self, row: char, w: u32, h: u32) -> Option<Self> {
        let col = self.pending_col()?;
        let col_idx = COL_KEYS.iter().position(|&c| c == col)? as u32;
        let row_idx = ROW_KEYS.iter().position(|&c| c == row)? as u32;
        let cols = grid_cols();
        let rows = grid_rows();
        if cols == 0 || rows == 0 || w < cols || h < rows {
            return None;
        }
        let cell_w = w / cols;
        let cell_h = h / rows;
        Some(Self::CellSelected {
            col,
            row,
            cursor: (col_idx * cell_w + cell_w / 2, row_idx * cell_h + cell_h / 2),
        })
    }

    /// Offset the cursor by `(dx, dy)` pixels, clamping to screen bounds.
    pub fn nudge(&self, dx: i32, dy: i32, w: u32, h: u32) -> Self {
        let (cx, cy) = self.cursor();
        let mut out = self.clone();
        let new_cursor = (
            (cx as i32 + dx).clamp(0, w as i32 - 1) as u32,
            (cy as i32 + dy).clamp(0, h as i32 - 1) as u32,
        );
        match &mut out {
            Self::Initial { cursor }
            | Self::ColumnSelected { cursor, .. }
            | Self::CellSelected { cursor, .. } => *cursor = new_cursor,
        }
        out
    }

    /// Step back one phase. Returns `None` from `Initial` (caller decides
    /// whether that means "exit" or "ignore").
    pub fn undo(&self, w: u32, h: u32) -> Option<Self> {
        match self {
            Self::CellSelected { .. } | Self::ColumnSelected { .. } => Some(Self::new(w, h)),
            Self::Initial { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level application state.
// ---------------------------------------------------------------------------

/// Complete UI state: the selection phase plus optional drag origin.
pub struct AppState {
    pub phase: SelectionPhase,
    pub drag_origin: Option<(u32, u32)>,
}

impl AppState {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            phase: SelectionPhase::new(w, h),
            drag_origin: None,
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag_origin.is_some()
    }
}

// ===========================================================================
// Tests.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 1920;
    const H: u32 = 1080;

    // -- Grid constants -----------------------------------------------------

    #[test]
    fn grid_dimensions_match_key_arrays() {
        assert_eq!(grid_cols() as usize, COL_KEYS.len());
        assert_eq!(grid_rows() as usize, ROW_KEYS.len());
    }

    #[test]
    fn is_col_key_accepts_all_col_keys() {
        for &k in COL_KEYS {
            assert!(is_col_key(k), "{k} should be a column key");
        }
    }

    #[test]
    fn is_row_key_accepts_all_row_keys() {
        for &k in ROW_KEYS {
            assert!(is_row_key(k), "{k} should be a row key");
        }
    }

    #[test]
    fn row_keys_cover_all_col_keys() {
        // Every column key is also a valid row key, enabling fast double-tap
        // combos like "aa" and "ss".
        for &k in COL_KEYS {
            assert!(is_row_key(k), "{k} is a col key but not a row key");
        }
    }

    // -- SelectionPhase transitions -----------------------------------------

    #[test]
    fn initial_cursor_is_screen_center() {
        let phase = SelectionPhase::new(W, H);
        assert_eq!(phase.cursor(), (W / 2, H / 2));
        assert!(!phase.is_cell_selected());
        assert_eq!(phase.pending_col(), None);
    }

    #[test]
    fn select_column_transitions_to_column_selected() {
        let phase = SelectionPhase::new(W, H);
        let next = phase.select_column('a', W, H).unwrap();
        assert!(next.pending_col().is_some());
        assert_eq!(next.pending_col(), Some('a'));
        assert!(!next.is_cell_selected());
    }

    #[test]
    fn select_column_snaps_cursor_x_to_cell_center() {
        let phase = SelectionPhase::new(W, H);
        let cell_w = W / grid_cols();

        // First column ('a' at index 0).
        let next = phase.select_column('a', W, H).unwrap();
        assert_eq!(next.cursor().0, cell_w / 2);

        // Last column ('p' at index 9).
        let next = phase.select_column('p', W, H).unwrap();
        assert_eq!(next.cursor().0, 9 * cell_w + cell_w / 2);
    }

    #[test]
    fn select_cell_from_column_selected() {
        let phase = SelectionPhase::new(W, H).select_column('a', W, H).unwrap();
        let next = phase.select_cell('w', W, H);
        assert!(next.is_some());
        let next = next.unwrap();
        assert!(next.is_cell_selected());

        let cell_w = W / grid_cols();
        let cell_h = H / grid_rows();
        assert_eq!(next.cursor(), (cell_w / 2, cell_h / 2));
    }

    #[test]
    fn select_cell_from_initial_returns_none() {
        let phase = SelectionPhase::new(W, H);
        assert!(phase.select_cell('w', W, H).is_none());
    }

    #[test]
    fn select_cell_with_overlapping_key() {
        // 'a' is both a col key and a row key — "aa" should work.
        let phase = SelectionPhase::new(W, H).select_column('a', W, H).unwrap();
        let next = phase.select_cell('a', W, H);
        assert!(next.is_some());
        assert!(next.unwrap().is_cell_selected());
    }

    #[test]
    fn select_cell_with_invalid_row_key_returns_none() {
        let phase = SelectionPhase::new(W, H).select_column('a', W, H).unwrap();
        // '!' is not a row key.
        assert!(phase.select_cell('!', W, H).is_none());
    }

    #[test]
    fn cell_label_only_when_selected() {
        let initial = SelectionPhase::new(W, H);
        assert_eq!(initial.cell_label(), "");

        let col = initial.select_column('d', W, H).unwrap();
        assert_eq!(col.cell_label(), "");

        let cell = col.select_cell('w', W, H).unwrap();
        assert_eq!(cell.cell_label(), "dw");
    }

    // -- Nudge --------------------------------------------------------------

    #[test]
    fn nudge_moves_cursor() {
        let phase = SelectionPhase::new(W, H);
        let nudged = phase.nudge(10, -5, W, H);
        let (cx, cy) = nudged.cursor();
        assert_eq!(cx, W / 2 + 10);
        assert_eq!(cy, H / 2 - 5);
    }

    #[test]
    fn nudge_clamps_to_zero() {
        let phase = SelectionPhase::new(W, H);
        let nudged = phase.nudge(-10000, -10000, W, H);
        assert_eq!(nudged.cursor(), (0, 0));
    }

    #[test]
    fn nudge_clamps_to_screen_edge() {
        let phase = SelectionPhase::new(W, H);
        let nudged = phase.nudge(10000, 10000, W, H);
        assert_eq!(nudged.cursor(), (W - 1, H - 1));
    }

    #[test]
    fn nudge_preserves_phase_variant() {
        let cell = SelectionPhase::new(W, H)
            .select_column('a', W, H)
            .unwrap()
            .select_cell('w', W, H)
            .unwrap();
        let nudged = cell.nudge(5, 5, W, H);
        assert!(nudged.is_cell_selected());
    }

    // -- Undo ---------------------------------------------------------------

    #[test]
    fn undo_from_cell_selected_returns_initial() {
        let cell = SelectionPhase::new(W, H)
            .select_column('a', W, H)
            .unwrap()
            .select_cell('w', W, H)
            .unwrap();
        let prev = cell.undo(W, H);
        assert!(prev.is_some());
        assert!(!prev.unwrap().is_cell_selected());
    }

    #[test]
    fn undo_from_column_selected_returns_initial() {
        let col = SelectionPhase::new(W, H).select_column('a', W, H).unwrap();
        let prev = col.undo(W, H);
        assert!(prev.is_some());
        assert_eq!(prev.unwrap().pending_col(), None);
    }

    #[test]
    fn undo_from_initial_returns_none() {
        let initial = SelectionPhase::new(W, H);
        assert!(initial.undo(W, H).is_none());
    }

    // -- AppState -----------------------------------------------------------

    #[test]
    fn app_state_initial_not_dragging() {
        let state = AppState::new(W, H);
        assert!(!state.is_dragging());
    }

    #[test]
    fn app_state_dragging_when_origin_set() {
        let mut state = AppState::new(W, H);
        state.drag_origin = Some((100, 200));
        assert!(state.is_dragging());
    }

    // -- Grid math covers full screen ---------------------------------------

    #[test]
    fn grid_cells_tile_screen() {
        let cell_w = W / grid_cols();
        let cell_h = H / grid_rows();
        // Every cell center should be within screen bounds.
        for col in 0..grid_cols() {
            for row in 0..grid_rows() {
                let cx = col * cell_w + cell_w / 2;
                let cy = row * cell_h + cell_h / 2;
                assert!(cx < W, "col {col} center x={cx} out of bounds");
                assert!(cy < H, "row {row} center y={cy} out of bounds");
            }
        }
    }
}
