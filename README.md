# mole

**Keyboard-only mouse navigation for Linux/X11.** Hit a keybinding and mole
reads your whole screen with OCR, then drops a two-letter label on **every line
of text** it finds — a menu entry, a paragraph, a button caption, a URL. Type the
letters and the pointer teleports onto that text, where you can click, drag, or
keep moving by keyboard. It's [Vimium]'s hint mode and [Helix]'s jump mode, but
for your entire desktop instead of one app — and it works on text the app never
told anyone about, because mole is *looking at the pixels*.

The name fits twice over: **mo**use-**le**ss, and a mole tunnels straight to
wherever you point.

```
┌──────────────────────────────────────────────────────┐
│  [aa]File   [as]Edit   [ad]View            [af]Help   │
│                                                        │
│   [ag]Open recent project        [ah]Reload window    │
│   [aj]Push all branches          [ak]Commit history   │
│                                                        │
│        type "aj"  →  pointer jumps onto that phrase    │
└──────────────────────────────────────────────────────┘
```

> Status: works on X11. Wayland support is a planned next step (see
> [Limitations](#limitations)).

## Why

On a keyboard-driven setup, reaching for the mouse is the slow part. Other tools
each give something up: [warpd] is a blind grid with no idea what's on screen;
[chelleport] reads text but is tied to one approach. mole's bet is that **text is
the map**: almost everything you want to click *is* or *sits next to* a word, so
if you can jump to any phrase on screen, you can reach anything — no per-app
accessibility support required. One native Rust binary, driven entirely by what's
visible.

## Features

- **Hint every phrase on screen** — OCR finds runs of words (a line of a menu, a
  sentence, a label) and labels each one.
- **Teleport** the pointer onto any hinted phrase.
- **Click modes**: left / right / double-click the hinted target.
- **Drag & select**: pick a start hint and an end hint; the selection is dragged
  and copied to the clipboard.
- **Free movement**: `hjkl` pointer nudging with a Shift large-step and optional
  hold-to-accelerate.
- **Daemon + tiny client** so each trigger has almost no startup cost, and `hjkl`
  movement needs no scan at all.
- **TOML config with hot reload** — tune scan sensitivity, phrase grouping,
  movement and colours; edit and save, no restart.

## Requirements

- An **X11** session (any window manager — i3, bspwm, GNOME-on-Xorg, …).
- The **`tesseract`** OCR binary on your `PATH` (this is how mole reads the
  screen — it is required, not optional).
- **No compositor required.** The overlay paints a frozen snapshot of your
  desktop behind the hints, so you always see where you're aiming. A running
  compositor (picom, compton, a compositing WM) additionally lets the *live*
  desktop show through, but is optional.
- To build: a **Rust toolchain** and the **cairo** development library.

## Install

mole is a normal Rust program: build it with cargo, put the binary on your
`PATH`, and run it like any other command.

### 1. Install the build and runtime dependencies

```sh
# Rust toolchain (if you don't have it):
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Native libraries — cairo (to build) and tesseract (to run):
# Debian/Ubuntu:  sudo apt install libcairo2-dev tesseract-ocr
# Arch:           sudo pacman -S cairo tesseract tesseract-data-eng
# Fedora:         sudo dnf install cairo-devel tesseract
```

### 2. Build the release binary

```sh
git clone https://github.com/lucasly-ba/mole
cd mole
cargo build --release        # produces ./target/release/mole
```

### 3. Put `mole` on your `PATH`

The easiest way is `cargo install`, which builds in release mode and drops the
binary into `~/.cargo/bin`:

```sh
cargo install --path .
```

If `~/.cargo/bin` isn't already on your `PATH` (cargo will warn you if it
isn't), add it once in your shell's startup file:

```sh
# ~/.bashrc or ~/.zshrc
export PATH="$HOME/.cargo/bin:$PATH"
```

Open a new terminal (or `source` the file) and check it's found:

```sh
mole --help        # if this prints help, you're set
```

Prefer not to use cargo's bin dir? Just copy the binary anywhere already on your
`PATH`, e.g. `install -Dm755 target/release/mole ~/.local/bin/mole`.

<details>
<summary>Nix users</summary>

A flake is provided. `nix profile install .` puts `mole` in
`~/.nix-profile/bin`, or `nix develop` gives you a dev shell with the toolchain
and a `mole` helper that runs `cargo run`. On NixOS prefer this over
`cargo install`, since a cargo-built binary won't find the nix-store cairo at
runtime.

</details>

## Usage

mole runs as a daemon and is triggered by a small client command — the client is
what you bind to a key in your WM.

```sh
mole daemon            # start the background process (autostart this)

mole teleport          # hint, then move the pointer there
mole click             # hint, then left-click
mole double-click      # hint, then double-click
mole right-click       # hint, then right-click
mole drag              # pick two hints, drag between them, copy selection
mole move              # free hjkl pointer movement (no scan)
mole ping              # check the daemon is alive
mole reload            # reload config now
mole dump-config       # print the default config to stdout
```

While hints are showing: type a label to pick it, **Backspace** to correct,
**Esc** to cancel. In `move` mode, `hjkl` nudges the pointer (Shift for big
steps), **Esc**/**Enter** exits.

### Binding it to a key

mole has no hotkey of its own — you bind one in whatever runs your X11 session
(your window manager, desktop environment, or `sxhkd`/`xbindkeys`). Point the key
at `mole <command>`. Only a hint command runs OCR; the daemon does nothing (and
never scans) until you press one.

The mechanism is the same everywhere; i3 is shown here purely as a concrete
example. In GNOME/KDE-on-Xorg you'd add the same `mole click` etc. as custom
keyboard shortcuts in Settings; with a standalone hotkey daemon you'd map them in
its config.

```i3config
# Example for i3 — adapt the syntax to your WM/DE.
exec_always --no-startup-id mole daemon

bindsym $mod+a       exec --no-startup-id mole click
bindsym $mod+Shift+a exec --no-startup-id mole teleport
bindsym $mod+g       exec --no-startup-id mole drag
bindsym $mod+m       exec --no-startup-id mole move
```

### Running the daemon under systemd

`mole daemon` runs in the foreground, so for a background service that starts
with your session and restarts on failure, a systemd **user** unit is provided
in [`contrib/mole.service`](contrib/mole.service):

```sh
cp contrib/mole.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now mole.service
```

The daemon needs `DISPLAY`/`XAUTHORITY` to reach X. A display manager usually
exports these into the systemd user environment for you; under a bare WM like i3
either uncomment the `Environment=` lines in the unit or run
`systemctl --user import-environment DISPLAY XAUTHORITY` from your WM autostart.
See the comments in the unit file for details.

## Configuration

mole reads `~/.config/mole/config.toml` (override with `--config`) and
hot-reloads it on save. Run `mole dump-config` for the full annotated default, or
see [`mole.example.toml`](./mole.example.toml). Everything is optional — omit a
key and the default is used. Highlights:

```toml
[keys]
hint_alphabet = "asdfghjkl"   # home-row keys used to build labels

[movement]
step = 24                      # hjkl small step (px) — base sensitivity
large_step = 160               # with Shift
acceleration = 1.0             # >1.0 = hold-to-accelerate; 1.0 = off
max_step = 600                 # ceiling on an accelerated step

[ocr]
language = "eng"               # tesseract language
min_confidence = 50.0          # drop shaky guesses (0–100)
max_word_gap = 1.0             # how readily words merge into one phrase
line_tolerance = 0.5           # how strict "same line" is
tiles = 4                      # parallel OCR strips — scan speed (see below)
prewarm = true                 # keep the OCR cache warm in the background

[hints]
background = [255, 220, 90, 230]   # RGBA
font_size = 13.0
```

`max_word_gap` and `line_tolerance` are the scan "sensitivity": raise
`max_word_gap` for longer phrases and fewer hints, lower it for more, shorter
targets.

OCR is the slow part of a hint. Three things make it fast:

- **Parallel OCR** — Tesseract is single-threaded per image, so mole OCRs the
  screen as `tiles` horizontal bands in parallel. On a multi-core machine
  `tiles = 4` roughly halves a full scan on a wide display (`tiles = 1` disables
  parallelism).
- **Incremental cache** — the daemon remembers the words it found and, on the
  next hint, re-reads only the *regions that changed* — a tight band around an
  edit, not the whole screen — splicing the new words in and keeping the rest. A
  hint on a screen you've been looking at does little or no OCR. Tiny changes (a
  blinking caret, a ticking clock) are ignored so the cache stays warm.
- **Background pre-warm** (`prewarm = true`) — a background thread watches the
  screen (via X DAMAGE) and refreshes the cache once it settles, so the scan is
  usually already done before you trigger and the hints appear instantly. It
  costs a little CPU while the screen is changing and stands down while a hint is
  on screen; set `prewarm = false` for purely on-demand scanning with no idle
  cost. Moving the mouse never triggers it (the cursor isn't part of the capture).
  A hint always wins: if you trigger one while the pre-warm happens to be mid-scan,
  it kills that background read so your hint never waits on it.
- **Instant hints, even on change** — when you trigger a hint, the cached hints
  for the unchanged part of the screen appear immediately while only the changed
  regions are re-read in the background; their hints pop in a moment later,
  without shifting the labels already on screen.

None of this is tied to a particular display: the capture, the cell grid, and the
tiles are all computed from your actual screen size, so the same behaviour applies
on any resolution.

## How it works

A hint trigger runs one pipeline:

1. **Capture** the screen (X11 `GetImage`).
2. **Detect** — run Tesseract over the capture and group the recognised words
   into phrase-level targets (words on a line, within normal spacing).
3. **Label** each phrase with a short, prefix-free key sequence and lay the boxes
   out without overlapping.
4. **Overlay** a transparent ARGB window and draw the hints with cairo.
5. **Match** keystrokes live, narrowing the visible hints as you type.
6. **Act** — teleport / click / drag, then tear the overlay down.

For a deeper, decision-by-decision write-up — including what's runtime-tested vs.
not, and the trade-offs behind each crate — see [`JOURNEY.md`](./JOURNEY.md).

## Limitations

- **X11 only** for now. Wayland needs `wlr-layer-shell` + virtual-pointer
  protocols; the detection/hint/render split is structured to grow a second
  backend.
- **OCR latency** is Tesseract-bound (~0.5–1s on a dense screen). PaddleOCR is a
  candidate if the scan needs to be quicker.
- **Tiny or stylised text** can be missed or misread; tune `min_confidence` and
  the language pack for your screen.
- **Pure-graphics targets** with no text (icon-only toolbars, games) aren't
  hintable yet — a fixed grid mode is the planned answer.

## License

MIT © Lucas Ly Ba

[Vimium]: https://github.com/philc/vimium
[Helix]: https://helix-editor.com/
[warpd]: https://github.com/rvaiya/warpd
[chelleport]: https://github.com/lubomyr/chelleport
