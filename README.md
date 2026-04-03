# mousefree

Keyboard-driven mouse navigation for Wayland. Launch the overlay, type two keys
to jump the cursor to a grid cell, then click, drag, or scroll — all without
touching the mouse.

## Requirements

- A Wayland compositor with `wlr-layer-shell` and `wlr-virtual-pointer` support
  (Sway, Hyprland, river, etc.)
- `fontconfig` (`fc-match`) for system font discovery
- US QWERTY keyboard layout (other layouts are not yet supported)

## Install

With [mise](https://mise.jdx.dev/):

```sh
mise use -g cargo:mousefree
```

Or with cargo directly:

```sh
cargo install mousefree
```

Or build from source:

```sh
git clone https://github.com/swaits/mousefree.git
cd mousefree
cargo install --path .
```

Then bind `mousefree` to a hotkey in your compositor. Every Wayland compositor
has its own way to bind a key to launch a program — consult your compositor's
docs. For example, in Sway:

```
bindsym $mod+semicolon exec mousefree
```

### Controls

| Key | Action |
|-----|--------|
| `a`–`p` | Select column |
| `q`,`w`,`e`,… | Select row (completes cell selection) |
| `Space` | Click |
| `Enter` | Double-click |
| `Shift`+`Enter` | Triple-click (select line/paragraph) |
| `.` | Right-click |
| `/` | Start drag (type a second cell to drop) |
| `h` `j` `k` `l` | Nudge cursor (8 px) |
| `H` `J` `K` `L` | Medium nudge (16 px) |
| `Ctrl`+`h` `j` `k` `l` | Large nudge (32 px) |
| `Alt`+`h` `j` `k` `l` | Pixel-perfect nudge (1 px) |
| Arrow keys | Scroll |
| `Backspace` | Undo / go back |
| `Escape` | Quit |

Keys like `h`, `j`, `k`, `l` are context-sensitive: before a cell is selected
they choose a column/row; after a cell is locked they nudge the cursor.

## Troubleshooting

- **"could not find a system font"** — Install fontconfig (`sudo apt install
  fontconfig` / `sudo pacman -S fontconfig`) and verify with `fc-match
  sans-serif`.
- **Overlay does not appear** — Your compositor may not support
  `wlr-layer-shell` or `wlr-virtual-pointer`. Try Sway, Hyprland, or river.
- **Keys produce wrong characters** — mousefree currently assumes a US QWERTY
  layout. Other layouts are not yet supported.
- **Virtual pointer stuck after crash** — If the process is killed mid-drag, a
  mouse button may stay pressed. Restarting your compositor session clears it.

## License

[MIT](LICENSE)
