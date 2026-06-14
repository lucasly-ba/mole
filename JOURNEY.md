# The mole journey

This is the long-form companion to the README: how mole is built, *why*
each decision was made, how the plan maps onto the code, and — honestly — what is
runtime-tested versus what is structurally correct but needs a real display to
prove out. If you're returning to this project after a while, start here.

---

## 1. The shape of the problem

The goal (from `mouseless_plan.pdf`): press a key, get Vimium-style two-letter
labels over every clickable thing on screen, type a label, and the pointer goes
there. The hard parts aren't the idea — they're the systems plumbing:

- **Where are the clickable things?** Two answers, very different in nature: the
  accessibility tree (structured, fast, sometimes absent) and OCR (universal,
  slow, fuzzy).
- **How do you draw on top of everything?** A borderless, click-through-ish,
  transparent X11 window the window manager won't touch.
- **How do you move/click the real pointer?** Synthetic input the rest of the
  system believes is hardware.
- **How do you make it feel instant?** Set everything up once in a daemon; a
  trigger only does the per-interaction work.

So the architecture is a pipeline (capture → detect → label → render → match →
act) wrapped in a daemon, with every stage in its own module so it can be read
and tested alone.

## 2. Module map

```
src/
├── main.rs            CLI: `daemon` vs the thin client commands
├── lib.rs             crate root + module docs
├── error.rs           one Error enum, Result alias
├── geometry.rs        Rect / Point + the maths (tested)
├── capture.rs         Screen: an X GetImage snapshot + pixel sampling
├── config/
│   ├── mod.rs         TOML structs, defaults, validation (tested)
│   └── watch.rs       hot reload via `notify`
├── x11/
│   ├── connection.rs  the connection + keysym/keycode maps (tested)
│   ├── hotkey.rs      global hotkeys (XGrabKey)
│   ├── pointer.rs     warp + XTest clicks/drag
│   └── overlay.rs     the transparent ARGB overlay window
├── detect/
│   ├── mod.rs         Element/Role + CompositeDetector dedup (tested)
│   ├── atspi.rs       AT-SPI tree walk over D-Bus (primary)
│   └── ocr.rs         tesseract subprocess + TSV parse (tested parser)
├── hint/
│   ├── label.rs       prefix-free labels + live matching (tested hard)
│   └── layout.rs      anti-overlap box placement (tested)
├── render/
│   ├── mod.rs         cairo drawing → raw ARGB buffer (tested)
│   └── palette.rs     adaptive contrast (tested)
├── interaction/mod.rs clipboard glue (arboard)
├── session.rs         orchestrates one interaction
└── daemon/
    ├── mod.rs         socket server + config hot-reload thread
    └── ipc.rs         the line protocol (tested)
```

The dependency arrows all point "inward": `geometry` and `config` know nothing
about X11; `detect`/`render`/`hint` depend on those primitives; `session` wires
the systems modules together; `daemon` drives `session`.

## 3. Plan → code, phase by phase

### Phase 1 — Foundations

- **§1.1 Capture** → `capture.rs`. X11 `GetImage` into a `Screen` that also knows
  how to read a pixel and average a region (for contrast later). The plan's
  "only capture needed zones" is `Screen::capture_region`, used by `capture_full`.
- **§1.2 Global hotkey** → `x11/hotkey.rs`. `XGrabKey` with the Caps/Num-Lock
  variants registered so the grab survives those modifiers. Clean event loop in
  `HotkeyManager::wait`. *Note:* in practice most users trigger via the socket
  from their WM (`exec mole click`), so this is the standalone path.
- **§1.3 hjkl movement** → `x11/pointer.rs` + `session::run_free_move`. Relative
  warps, configurable keys, normal/large step (Shift → uppercase keysym).

### Phase 2 — Overlay

- **§2.1 Overlay window** → `x11/overlay.rs`. 32-bit ARGB visual + colormap,
  `override_redirect` so the WM ignores it, keyboard grab while shown. The
  transparency relies on a running compositor.
- **§2.2 Hint rendering** → `render/mod.rs` + `render/palette.rs`. **Key
  decision:** I draw with cairo into an in-memory `ImageSurface` and upload the
  bytes to the window with `PutImage`, instead of using a cairo-xcb surface. That
  keeps `cairo-xcb` (and a libxcb FFI dependency) out of the build *and* makes
  rendering unit-testable with no display. Anti-overlap is in `hint/layout.rs`;
  adaptive contrast (text colour chosen by luminance) in `palette.rs`.

### Phase 3 — Detection

- **§3.1 AT-SPI** → `detect/atspi.rs`. Resolve the a11y bus via
  `org.a11y.Bus.GetAddress`, then walk the tree from the registry root reading
  `GetRole`, the `Name` property, and `GetExtents`. Bounded in depth/node count;
  every node is best-effort so one misbehaving app can't break a session.
- **§3.2 Hint generation** → `hint/label.rs`. The algorithm grows a breadth-first
  frontier so labels are **prefix-free** (the instant your keys equal a label,
  the choice is unambiguous — no Enter needed) and as short as possible. Live
  matching narrows candidates per keystroke; a dead-end key is rejected without
  being consumed.
- **§3.3 OCR fallback** → `detect/ocr.rs`. **Decision:** shell out to the
  `tesseract` binary (PPM in, TSV out) rather than link its C API. No extra
  native build dep, trivially swappable for PaddleOCR, and the TSV parser is pure
  and tested.

`detect/mod.rs` runs the configured backends in order and **deduplicates**,
preferring AT-SPI rectangles over OCR ones for the same spot, then sorts results
into reading order so the shortest labels land top-left.

### Phase 4 — Interactions

- **§4.1 Clicks** → `pointer.rs`. `XTestFakeButtonEvent` for left/right; `count`
  gives double-click / N-click.
- **§4.2 Drag & select** → `session.rs` `Mode::Drag`: pick a start hint, pick an
  end hint, `MouseDown → move → MouseUp`, then mirror the resulting PRIMARY
  selection into the CLIPBOARD via `arboard` (`interaction/mod.rs`).

A subtlety that bit the first draft: the overlay must be **torn down before** the
synthetic click, or the click lands on our own window. `run_hint` now collects
the target(s) while the overlay is up, hides it, *then* acts.

### Phase 5 — Config & daemon

- **§5.1 Config** → `config/`. TOML + serde, every field optional with a default,
  validated on load, hot-reloaded by watching the *directory* (editors rename on
  save, which a file-level watch misses).
- **§5.2 Daemon** → `daemon/`. A Unix-socket line protocol (`ipc.rs`): the client
  writes one word, the daemon runs it and replies `ok`/`err: …`. Interactions run
  one at a time on the main thread (they own the keyboard); config reload runs on
  a side thread behind an `Arc<Mutex<Config>>`.

## 4. Dependency choices

| Need | Crate | Why this one |
|------|-------|--------------|
| X11 | `x11rb` | Pure-Rust backend → no hard libxcb build dep; full XTest/Composite |
| Overlay drawing | `cairo-rs` (ImageSurface only) | Vector text/boxes without a toolkit; image surface keeps it display-free and testable |
| AT-SPI | `zbus` (blocking) | Pure-Rust D-Bus; blocking API keeps the session loop simple |
| Clipboard | `arboard` | X11 backend is itself x11rb-based; handles PRIMARY↔CLIPBOARD |
| Config | `serde` + `toml` + `notify` | Standard, ergonomic, hot-reload |
| CLI | `clap` | Subcommands for free |
| OCR | *(none)* — `tesseract` subprocess | Zero extra build deps; swappable |

## 5. What's tested vs. what needs a display

This was built and unit-tested in a headless sandbox, so be clear-eyed about the
two tiers:

**Unit-tested and trustworthy** (run `cargo test`): geometry, config parsing &
validation, the hint label algorithm (uniqueness + prefix-freedom across many
sizes), live matching, anti-overlap layout, adaptive-contrast colour choice, the
cairo render producing the expected buffer, OCR TSV parsing, detector dedup, the
IPC command round-trip, modifier parsing, keysym mapping. The per-module unit
tests live inline (`#[cfg(test)]`) so they can reach private helpers.

**Integration-tested** (`tests/pipeline.rs`): the seams between modules, which
the isolated unit tests can't see. A `FakeDetector` implementing the public
`Detector` trait feeds the exact chain `run_hint` uses — detect → `generate_labels`
→ `place_hints` → `HintMatcher` — and asserts that typing a label lands on the
right *element centre* (not the label-box corner), that every label among 30
elements is reachable, that stacked elements stay individually selectable, that a
dead-end keystroke doesn't strand the user, and that edge boxes clamp on-screen.
All display-free, so it runs in the sandbox alongside the unit tests.

**Structurally complete, needs a real X session to verify end-to-end**: the
GetImage capture, the overlay window creation/keyboard grab, XTest clicks/drags,
the live AT-SPI traversal (needs a running a11y bus with real apps), and the OCR
subprocess round-trip (needs the `tesseract` binary). The flake's `checkFlags`
skip these in the sandboxed build for exactly this reason.

If you pick this up on a real machine, the first things to validate are: (1)
overlay actually appears transparent under your compositor, (2) AT-SPI returns
nodes for a GTK app (`AT-SPI` must be enabled in your session), (3) XTest clicks
land where expected.

## 6. Where to take it next

- **Wayland backend.** The systems modules (`x11/`) are the only X-specific part;
  a `wayland/` sibling implementing capture/overlay/pointer behind the same
  shapes would let `session.rs` stay unchanged. `wlr-layer-shell` for the
  overlay, `wlr-virtual-pointer` for input, the screencopy protocol for capture.
- **Role-filtered modes.** `Role` is already detected; a "hint links only" mode
  is a one-line filter in `session`.
- **Faster fallback.** Swap the `tesseract` subprocess for PaddleOCR behind the
  same `Detector` trait.
- **Grid mode** for the no-accessibility, no-text case (games), as a third
  `Detector`.

## 7. Building & running

```sh
nix develop -c cargo test     # the pure-logic suite
nix develop -c cargo build    # full build (needs cairo via the flake)
nix build                     # the packaged binary at ./result/bin/mole
```

To actually *use* it, get `mole` onto your `PATH` rather than calling it through
`./result/bin` or `cargo run` each time. The flake makes this one command:

```sh
nix profile install .         # installs `mole` into ~/.nix-profile/bin (on PATH)
mole daemon &                 # then bind `mole click` etc. in your WM
```

`nix profile install` builds `packages.default` (the same `buildRustPackage` the
CI/`nix build` path uses) and symlinks the result into your user profile, so the
binary you run is byte-for-byte the packaged one. For day-to-day hacking the dev
shell is friendlier: `nix develop` defines a `mole` shell function that forwards
to `cargo run`, so `mole click` exercises your working tree without a reinstall.
Both routes exist on purpose — installed binary for *using* mole, `cargo run`
wrapper for *changing* it.
