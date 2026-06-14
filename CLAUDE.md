# CLAUDE.md

Guidance for Claude Code working in this repository.

## What this is

**mole** — a Rust tool for keyboard-only mouse navigation on Linux/X11:
Vimium-style two-letter hints over every clickable element on the desktop, found
via the AT-SPI accessibility tree with an OCR fallback. Built from the French
spec in `mouseless_plan.pdf`. Public repo; also a portfolio piece, so keep it
clean and the README honest about portability.

Read `JOURNEY.md` for the full architecture and design rationale — it is the
canonical explanation of how and why the code is shaped the way it is.

## Build / test / run

**There is no system Rust toolchain on PATH, and the build needs the cairo C
library.** Everything goes through the flake:

```sh
nix develop -c cargo build      # build
nix develop -c cargo test       # 44 pure-logic tests, no display needed
nix develop -c cargo clippy --all-targets
nix build                       # packaged binary at ./result/bin/mole
```

Flakes only see git-tracked files: **`git add` new files before `nix develop` /
`nix build`**, or Nix errors with "not tracked by Git".

## Architecture (one line each)

Pipeline: capture → detect → label → render → match → act, wrapped in a daemon.

- `geometry.rs`, `config/` — primitives + TOML config (hot-reloaded). Display-free.
- `capture.rs`, `x11/{connection,hotkey,pointer,overlay}.rs` — all X-specific code.
- `detect/{atspi,ocr}.rs` + `detect/mod.rs` — backends behind a `Detector` trait.
- `hint/{label,layout}.rs` — prefix-free labels + anti-overlap placement.
- `render/{mod,palette}.rs` — cairo ImageSurface → bytes uploaded via PutImage
  (deliberately **not** cairo-xcb).
- `session.rs` — wires one interaction together; `daemon/` — socket server + CLI.

Dependency arrows point inward: `geometry`/`config` know nothing about X11.

## Conventions

- **Commit as `Lucas Ly Ba <hi@lucaslyba.com>`. Do NOT add a `Co-Authored-By:
  Claude` trailer.** The repo's default git identity is wrong — set it explicitly.
- Prefer several focused commits over one large one.
- Keep modules small and single-purpose; match the existing comment density
  (each module opens with a doc comment explaining its role and the plan section
  it implements).
- The README must only claim portability the tool actually has: mention X11
  (any X11 user benefits); treat NixOS/i3 as examples, not requirements.

## Reality check

Built and unit-tested headless. The X11/AT-SPI/cairo-surface/OCR-subprocess
paths compile and are structurally complete but need a real X session to verify
end-to-end (see `JOURNEY.md` §5). The flake's `checkFlags` skip those in
sandboxed builds.
