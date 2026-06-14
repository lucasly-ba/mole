# mole

**Keyboard-only mouse navigation for Linux/X11.** Hit a keybinding, and every
clickable thing on screen gets a two-letter label — type the letters and the
pointer teleports there, clicks, or starts a drag. It's [Vimium]'s hint mode and
[Helix]'s jump mode, but for your whole desktop instead of one app.

Targets are found through the **AT-SPI accessibility tree** (the same data screen
readers use), which is precise and near-instant. For windows that expose nothing
to accessibility — Electron apps, terminals, games — it falls back to **OCR**.

```
┌──────────────────────────────────────────────┐
│  [aa]File   [as]Edit   [ad]View      [af]Help │
│                                                │
│   [ag]New tab        [ah]Reload                │
│   [aj]Bookmarks      [ak]History               │
│                                                │
│        type "ah"  →  pointer jumps to Reload   │
└──────────────────────────────────────────────┘
```

> Status: works on X11. Wayland support is a planned next step (see
> [Limitations](#limitations)).

## Why

On a keyboard-driven setup, reaching for the mouse is the slow part. Existing
tools each give up something: [warpd] is grid-only with no text awareness;
[chelleport] is X11-only with ~1s OCR latency. mole aims for the best of
both — accessibility-tree precision first, optimised OCR only as a fallback —
in a single native Rust binary.

## Features

- **Hint any element** and teleport the pointer to it.
- **Click modes**: left / right / double-click the hinted element.
- **Drag & select**: pick a start hint and an end hint; the selection is dragged
  and copied to the clipboard.
- **Free movement**: `hjkl` pointer nudging with a large-step modifier.
- **AT-SPI first, OCR fallback** — fast where possible, universal where needed.
- **Daemon + tiny client** so each trigger has almost no startup cost.
- **TOML config with hot reload** — edit and save, no restart.

## Requirements

- An **X11** session (any window manager — i3, bspwm, GNOME-on-Xorg, …).
- A **compositor** for see-through hints (picom, compton, or a compositing WM).
  Without one the overlay still works, it just isn't transparent.
- Optional: the **`tesseract`** binary on `PATH` for the OCR fallback.

## Install

### With Nix (flake)

A flake is provided, so on any system with Nix you get a reproducible build and
all native dependencies (cairo, tesseract) without touching your system:

```sh
nix run github:lucaslyba/mole -- daemon      # run the daemon
nix develop                                        # dev shell with the toolchain
nix build                                          # build the package
```

### With cargo

```sh
# Native deps: cairo (build) and, for OCR, the tesseract binary (runtime).
# Debian/Ubuntu:  apt install libcairo2-dev tesseract-ocr
# Arch:           pacman -S cairo tesseract tesseract-data-eng
# Fedora:         dnf install cairo-devel tesseract

cargo install --path .
# Or without the OCR fallback:
cargo install --path . --no-default-features
```

## Usage

mole runs as a daemon and is triggered by a small client command — the
client is what you bind to a key in your WM.

```sh
mole daemon            # start the background process (autostart this)

mole teleport          # hint, then move the pointer there
mole click             # hint, then left-click
mole double-click      # hint, then double-click
mole right-click       # hint, then right-click
mole drag              # pick two hints, drag between them, copy selection
mole move              # free hjkl pointer movement
mole ping              # check the daemon is alive
mole reload            # reload config now
mole dump-config       # print the default config to stdout
```

While hints are showing: type a label to pick it, **Backspace** to correct,
**Esc** to cancel.

### Example i3 binding

```i3config
exec_always --no-startup-id mole daemon

bindsym $mod+a       exec --no-startup-id mole click
bindsym $mod+Shift+a exec --no-startup-id mole teleport
bindsym $mod+g       exec --no-startup-id mole drag
bindsym $mod+m       exec --no-startup-id mole move
```

The same idea works in any WM: bind a key to `exec mole <command>`.

## Configuration

mole reads `~/.config/mole/config.toml` (override with `--config`) and
hot-reloads it on save. Run `mole dump-config` for the full annotated
default, or see [`mole.example.toml`](./mole.example.toml). Everything
is optional — omit a key and the default is used. Highlights:

```toml
[keys]
hint_alphabet = "asdfghjkl"   # home-row keys used to build labels

[movement]
step = 24                      # hjkl small step (px)
large_step = 160               # with Shift

[hints]
background = [255, 220, 90, 230]   # RGBA
font_size = 13.0

[detection]
backends = ["atspi", "ocr"]    # order tried; drop "ocr" to disable the fallback
```

## How it works

A trigger runs one pipeline:

1. **Capture** the screen (X11 `GetImage`).
2. **Detect** elements — walk the AT-SPI tree over D-Bus; fall back to Tesseract
   over the capture for apps that expose nothing.
3. **Label** each element with a short, prefix-free key sequence and lay the
   boxes out without overlapping.
4. **Overlay** a transparent ARGB window and draw the hints with cairo.
5. **Match** keystrokes live, narrowing the visible hints as you type.
6. **Act** — teleport / click / drag, then tear the overlay down.

For a deeper, decision-by-decision write-up — including what's runtime-tested vs.
not, and the trade-offs behind each crate — see [`JOURNEY.md`](./JOURNEY.md).

## Limitations

- **X11 only** for now. Wayland needs `wlr-layer-shell` + virtual-pointer
  protocols; the code is structured to grow a second backend.
- **Electron apps** often expose a poor AT-SPI tree — they lean on the OCR path.
- **Games / OpenGL** expose no accessibility data at all; OCR or a fixed grid is
  the only option.
- **OCR latency** is Tesseract-bound (~0.5–1s on a dense screen). AT-SPI is the
  fast path; PaddleOCR is a candidate if the fallback needs to be quicker.

## License

MIT © Lucas Ly Ba

[Vimium]: https://github.com/philc/vimium
[Helix]: https://helix-editor.com/
[warpd]: https://github.com/rvaiya/warpd
[chelleport]: https://github.com/lubomyr/chelleport
