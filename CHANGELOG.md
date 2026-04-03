# Changelog

## [1.0.0] - 2026-04-03

### Added

- Keyboard-driven grid overlay for Wayland compositors
- Two-key cell selection (column + row) with 10x16 grid
- Cursor nudging with `hjkl` / `HJKL` / `Ctrl+hjkl` / `Alt+hjkl` (1 px)
- Left-click, double-click, triple-click (`Shift+Enter`), and right-click
- Click-and-drag with animated pointer interpolation
- Scroll pass-through via arrow keys
- Undo/backspace to step back one selection phase
- Context-sensitive status bar with key hints
- System font discovery via fontconfig (`fc-match`)

### Requirements

- Wayland compositor with `wlr-layer-shell` and `wlr-virtual-pointer`
- fontconfig
- US QWERTY keyboard layout
