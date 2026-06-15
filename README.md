# Mole

[![CI](https://github.com/lucasly-ba/mole/actions/workflows/ci.yml/badge.svg)](https://github.com/lucasly-ba/mole/actions/workflows/ci.yml)

**Keyboard-only mouse navigation for Linux/X11.** Press a key and Mole labels
every line of text on your screen with two letters. Type a label and the pointer
teleports there — then click, drag, or keep moving by keyboard.

The name fits twice over: **mo**use-**le**ss, and a mole tunnels straight to
wherever you point.

![Mole in action](docs/demo.png)

**[▶ Watch the demo](https://github.com/lucasly-ba/mole)** <!-- TODO: replace with the demo video link -->

> Status: works on X11. Wayland is a planned next step (see
> [Limitations](#limitations)).

## Features

- **Hint every line of text** on screen and **teleport** the pointer to it.
- **Click** — left, right, or double.
- **Drag & select** between two hints, copied to the clipboard.
- **Free movement** — `hjkl` pointer nudging, with a big step and optional
  hold-to-accelerate.
- **Fast** — a background daemon keeps each trigger near-instant.
- **Configurable** — one TOML file, hot-reloaded on save.

## Installation

Mole is a Cargo project. It needs two system packages — **cairo** (to build) and
**tesseract** (at runtime) — both in every major distro's repositories. Then:

```sh
cargo build --release        # binary at target/release/mole
```

Put it on your `PATH` with `cargo install --path .` (installs to `~/.cargo/bin`),
or copy `target/release/mole` anywhere on your `PATH`.

<details>
<summary>Nix</summary>

`nix profile install github:lucasly-ba/mole` installs the binary with cairo and
tesseract wired in; `nix develop` gives a dev shell with the full toolchain. On
NixOS prefer this over `cargo install`.

</details>

## Usage

Mole runs as a daemon and is triggered by a small client command. Bind that
command to a key in whatever runs your session — your window manager, desktop
environment, or a hotkey daemon such as `sxhkd`.

```sh
mole daemon            # start the background process (autostart this)

mole teleport          # hint, then move the pointer there
mole click             # hint, then left-click
mole double-click      # hint, then double-click
mole right-click       # hint, then right-click
mole drag              # pick two hints, drag between them, copy the selection
mole move              # free hjkl pointer movement
mole reload            # reload the config now
mole dump-config       # print the default config
```

While hints are showing: type a label to pick it, **Backspace** to correct,
**Esc** to cancel. In `move` mode, `hjkl` nudges the pointer (Shift for big
steps), **Esc**/**Enter** exits.

### Autostart

`mole daemon` runs in the foreground, so to start it with your session use the
systemd **user** unit in [`contrib/mole.service`](contrib/mole.service):

```sh
cp contrib/mole.service ~/.config/systemd/user/
systemctl --user enable --now mole.service
```

The daemon needs `DISPLAY` and `XAUTHORITY`. Most login managers export these
into the systemd user environment; if yours doesn't, uncomment the
`Environment=` lines in the unit, or run
`systemctl --user import-environment DISPLAY XAUTHORITY` from your autostart.

## Configuration

Mole reads `~/.config/mole/config.toml` (override with `--config`) and
hot-reloads it on save. Every setting is optional — run `mole dump-config` for
the annotated default, or copy [`mole.example.toml`](mole.example.toml).

```toml
[keys]
hint_alphabet = "asdfghjkl"   # keys used to build hint labels

# Movement keys for `mole move`. Defaults are hjkl; remap to any keys you like
# — e.g. move_left = "l", move_down = ";", move_up = "'", move_right = "\\".
move_left  = "h"
move_down  = "j"
move_up    = "k"
move_right = "l"

[movement]
step = 24                      # small step (px)
large_step = 160               # step while Shift is held (works with any key)
acceleration = 1.0             # >1.0 = hold-to-accelerate; 1.0 = off

[hints]
background = [255, 220, 90, 230]   # label colour, RGBA
font_size = 13.0
```

## Limitations

- **X11 only** for now; Wayland is planned.
- **Tiny or stylised text** can be missed or misread.
- **Targets with no text** (icon-only toolbars, games) aren't hintable yet.

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). For the
design and the reasoning behind the code, read [JOURNEY.md](JOURNEY.md).

## License

[MIT](LICENSE) © Lucas Ly Ba
