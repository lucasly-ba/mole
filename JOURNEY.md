# The mole journey

This is the long-form companion to the README: how mole is built, *why*
each decision was made, how the plan maps onto the code, and — honestly — what is
runtime-tested versus what is structurally correct but needs a real display to
prove out. If you're returning to this project after a while, start here.

---

## 1. The shape of the problem

The goal (rooted in `mouseless_plan.pdf`): press a key, get two-letter labels
over the screen, type a label, and the pointer goes there. The
hard parts aren't the idea — they're the systems plumbing:

- **What do you point at, and how do you find it?** This is the central design
  choice, and it changed (see below): mole hints **text**, found by reading the
  screen with OCR.
- **How do you draw on top of everything?** A borderless, transparent X11 window
  the window manager won't touch.
- **How do you move/click the real pointer?** Synthetic input the rest of the
  system believes is hardware.
- **How do you make it feel instant?** Set everything up once in a daemon; a
  trigger only does the per-interaction work, and plain `hjkl` movement does no
  scanning at all.

So the architecture is a pipeline (capture → detect → label → render → match →
act) wrapped in a daemon, with every stage in its own module so it can be read
and tested alone.

### The pivot: text is the map

The first design hinted *clickable widgets*, found primarily through the AT-SPI
accessibility tree, with OCR only as a fallback for apps that expose nothing.
That was abandoned. Two problems: the accessibility tree only knows about
controls an app *chooses to declare*, so huge swaths of the screen (document
text, terminals, Electron apps, anything custom-drawn) were invisible to it; and
maintaining two very different detection paths doubled the surface area for a
feature that, in practice, the OCR path already covered.

The bet mole makes instead: **almost everything you want to click is, or sits
next to, a word.** So if you can jump to any *phrase* on screen, you can reach
anything — with no dependency on app cooperation. That makes OCR the single
source of targets, and turns "group recognised words into phrase-sized targets"
([`detect/ocr/phrase.rs`](src/detect/ocr/phrase.rs)) into the heart of the tool.
AT-SPI was removed outright.

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
│   ├── pointer.rs     warp + XTest clicks/drag
│   ├── overlay.rs     the transparent ARGB overlay window
│   └── grab.rs        windowless keyboard grab for free-move
├── detect/
│   ├── mod.rs         Element + Detector trait + finalize() (tested)
│   └── ocr/
│       ├── mod.rs        OcrDetector: wires the steps together
│       ├── tesseract.rs  drive the tesseract subprocess (capture → TSV)
│       ├── tsv.rs        parse TSV into confident word boxes (tested)
│       ├── cache.rs      incremental cache: re-read only changed bands (tested)
│       ├── cancel.rs     kill in-flight OCR so a hint preempts the pre-warm
│       └── phrase.rs     group words into phrase targets (tested hard)
├── hint/
│   ├── label.rs       prefix-free labels + live matching (tested hard)
│   └── layout.rs      anti-overlap box placement (tested)
├── motion.rs          velocity-based pointer glide for free-move (tested)
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
- **§1.2 Triggering.** The plan called for a global hotkey (`XGrabKey`). In
  practice every trigger comes from the WM binding a key to `exec mole <cmd>`,
  which reaches the daemon over its Unix socket — so the WM already owns the
  hotkey and mole needs no grab of its own. An early `XGrabKey` implementation
  existed but was removed as dead weight once socket triggering proved sufficient.
- **§1.3 Free-move** → `session::run_free_move` + `x11/grab.rs` + `motion.rs`.
  Move mode should feel like a real mouse, so the pointer **glides** rather than
  jumping a fixed step: `motion::Glide` models a velocity (px/second) that starts
  at a base speed, accelerates the longer a direction is held, and is multiplied
  by a **boost** key for crossing the screen — pure arithmetic with sub-pixel
  carry, unit-tested away from any X11. The session loop ticks it at ~125 Hz and
  warps the pointer by the per-frame delta. **Remappable keys**
  (`move_left`/`down`/`up`/`right`, default hjkl) steer; action keys click,
  double-click, right-click, and toggle a drag-and-copy — so move mode is a full
  pointer, not just a cursor nudger.

  Two design points worth keeping:
  - **No window at all.** Unlike the hint overlay, free-move shows nothing of its
    own (`x11::KeyboardGrab`): it grabs the keyboard on the root window
    (`owner_events = false`, so every key reaches us regardless of focus) and the
    pointer glides over the *live* desktop. That sidesteps the old "black screen
    without a compositor" problem entirely — there is no overlay to paint — and
    synthetic clicks fall straight through to the apps beneath, since nothing is
    covering them.
  - **Auto-repeat collapse.** Holding a key, the X server (without detectable
    auto-repeat) fakes a stream of `Release`+`Press` pairs sharing a timestamp.
    `KeyboardGrab::drain` drops a `Release` immediately followed by a same-key
    `Press` at the same time, so a held key reads as one press until it is truly
    released — exactly the held-state the glide loop tracks.

### Phase 2 — Overlay

- **§2.1 Overlay window** → `x11/overlay.rs`. 32-bit ARGB visual + colormap,
  `override_redirect` so the WM ignores it, keyboard grab while shown. The first
  cut relied on a running compositor to blend the transparent pixels — with none
  you got a black screen with floating labels and no sense of *where* a jump
  would land. So the renderer now paints a frozen snapshot of the desktop (which
  it already captured for OCR — see `render::screen_backdrop`) as an opaque
  backdrop, dimmed by `hints.dim`. The overlay is usable with no compositor; a
  compositor, if present, still shows the live desktop through the alpha.
- **§2.2 Hint rendering** → `render/mod.rs` + `render/palette.rs`. **Key
  decision:** I draw with cairo into an in-memory `ImageSurface` and upload the
  bytes to the window with `PutImage`, instead of using a cairo-xcb surface. That
  keeps `cairo-xcb` (and a libxcb FFI dependency) out of the build *and* makes
  rendering unit-testable with no display. Anti-overlap is in `hint/layout.rs`;
  adaptive contrast (text colour chosen by luminance) in `palette.rs`. The
  opaque backdrop is the expensive part of a frame (a per-pixel pass over the
  whole screen), so it is built once per interaction as a `render::Backdrop` and
  reused: each keystroke's repaint is then just a blit plus the labels, and the
  heavy pass runs *before* the overlay is mapped so showing it doesn't flash.

### Phase 3 — Detection

Detection is OCR, end to end, split into small single-purpose steps under
`detect/ocr/`:

- **§3.1 Reading the screen** → `ocr/tesseract.rs` + the tiling in `ocr/mod.rs`.
  **Decision:** shell out to the `tesseract` binary (PPM in, TSV out) rather than
  link its C API. No extra native build dependency, and the OCR engine becomes
  trivially swappable (PaddleOCR behind the same step). Run with `--psm 11`
  ("sparse text") so it finds text *anywhere*, not just in one assumed text block.
  **Speed:** OCR is by far the slowest stage (≈3 s on a 4480×1440 desktop) and
  Tesseract is single-threaded per image, so the screen is split into
  `ocr.tiles` overlapping horizontal strips that are OCR'd in parallel — one
  process per strip, each capped to a share of the cores via `OMP_THREAD_LIMIT`
  so they don't oversubscribe and thrash. That roughly halves scan time on a
  wide multi-core display. Downscaling was rejected: halving resolution lost
  about half the detected words. Strips overlap by `TILE_OVERLAP` so a line on a
  cut survives; the duplicates that creates are removed by `dedup_words` (same
  text + heavily overlapping box, keeping the fuller one) before grouping.
  **Caching + pre-warm** → `ocr/cache.rs` + `daemon/prewarm.rs`. Even halved, a
  full scan is too slow to do on every hint, and most hints land on a screen that
  barely changed. So the daemon owns one `ScanCache` that keeps the last scan's
  whole-screen pixels (the baseline) plus the flat list of words found. Each scan
  diffs the new capture against the baseline and re-OCRs **only the regions that
  changed**, splicing the fresh words in (words inside a re-read region replaced,
  everything outside kept) — a hint on a static screen does no OCR at all.

  Two things make this robust on a real desktop. First, change is judged by
  *locality*, not an exact hash: the screen is a grid of small cells, a cell only
  counts as changed when a real fraction of its pixels differ, and the screen is
  only re-read when more than a couple of cells changed. A ticking clock or a
  blinking text caret is one or two cells and is ignored; a scroll or a typed-out
  line is many and is caught. (An exact hash, or a flat pixel count, made every
  flicker re-OCR a whole region, so the cache never settled.) Second, the work is
  scoped to **tight bands**: the changed cells' rows are clustered into contiguous
  runs (padded by a margin so text straddling an edge is fully inside), and only
  those full-width bands are OCR'd — a chat message near the bottom re-reads a
  thin band, not half the display. A cold scan (no baseline, or after a resize)
  tiles the whole screen into `tiles` overlapping bands; a very large change is
  likewise split into chunks. Either way the bands run in parallel, overlap so a
  cut line is whole in one of them, and `dedup_words` removes the resulting
  duplicates.

  On top of that, a background thread watches the whole screen with the X DAMAGE
  extension and re-reads changed bands once the screen settles, so the cache is
  usually already current when you trigger — the hint then appears with no scan at
  all. DAMAGE works here even with no compositor (verified by probe), and reports
  drawing only, not pointer motion, so moving the mouse never wakes it. The whole
  scan holds the cache lock so the on-demand hint and the background warm never
  OCR at once (which would oversubscribe the cores); pre-warm also stands down
  while an interaction owns the screen, so it never captures mole's own overlay.
  `ocr.prewarm = false` turns the background thread off for a purely on-demand,
  zero-idle-cost mode.

  Finally, even when something *did* change, a hint needn't wait for it.
  `Detector::detect_split` returns the cached words for the unchanged part of the
  screen **immediately**, plus a closure that re-OCRs the changed bands. The
  session (`select_incremental`) shows the ready hints at once, runs the closure
  on a worker thread, and folds the late hints in when they arrive — placed
  around the existing ones, with labels drawn from a reserved pool so the labels
  already on screen never shift (and a key already typed is replayed onto the
  larger set). So a hint over a screen with one busy corner appears instantly for
  everything else, and the corner's hints pop in a fraction of a second later.
  This split only applies to *localised* change: when the changed bands cover
  more than half the screen (a fresh window, a workspace switch) there's no
  meaningful cached part to show, and a half-filled overlay just looks broken —
  so `detect_split` re-reads the whole thing up front and shows the complete set
  at once instead of dribbling it in.

  **Latency, honestly.** To keep the cache current the pre-warm reacts quickly: a
  short settle debounce (`DEBOUNCE`, ~120ms) and an *immediate* re-read of the
  first change after a quiet spell (`IDLE_REACT`), so a one-off edit is being
  cached before you can trigger a hint. The result is that a hint is usually
  ~instant. The one remaining hazard is a *collision*: the on-demand hint and the
  background warm share one OCR-at-a-time lock (so they don't both fire tesseract
  and thrash the cores — letting them run together measured ~2× slower for both),
  so a hint triggered exactly while the pre-warm is mid-re-read would otherwise
  wait for it (tens of ms in a gap, up to ~1s on a full collision). Two non-fixes
  were measured and rejected first: reacting faster doesn't remove the collision,
  and splitting into more, thinner bands doesn't help (tesseract has a fixed
  per-process startup cost, and a change spans more thin bands → more processes →
  more overhead).

  **Preempting the pre-warm** is what actually closes the collision, and it is
  now done. When a hint starts, the daemon `abort()`s the pre-warm's
  [`Cancel`](src/detect/ocr/cancel.rs) token, which kills its in-flight
  `tesseract` children so it drops the cache lock immediately — the hint never
  waits more than the time to reap a killed process. The subtle part is keeping a
  killed read from corrupting the cache: planning a scan no longer adopts the
  changed regions into the baseline; a band is adopted *only when its fresh words
  are spliced back in* (`cache::ScanCache::splice`). So an aborted read — whose
  bands were never spliced — leaves both the baseline and the cached words exactly
  as they were, and those regions are simply re-planned on the next scan instead
  of being silently marked "already read" and going stale. The child is shared
  behind a lock that the aborter can reach, while its output is read off that lock,
  so the killer is never itself blocked. The on-demand path carries its own
  `Cancel` that is never aborted; only the background warm is preemptible.
- **§3.2 Parsing** → `ocr/tsv.rs`. The TSV header maps column names to indices, so
  the layout isn't hard-coded; level-5 (word) rows above the confidence threshold
  become word boxes in absolute screen coordinates. Pure and tested.
- **§3.3 Word vs. phrase targets** → `ocr/phrase.rs` + `OcrDetector` granularity.
  Tesseract gives one box per word. **By default every word is its own hint**
  (`ocr.hint_words = true`), so every word — including the ends of lines — is
  reachable; an earlier phrase-only default left the back half of each line with
  no hint at all. Set `hint_words = false` to merge words into phrases instead
  (fewer labels, but you only land at the start of each): cluster into lines
  (vertical centres within `line_tolerance`), then split each line on wide
  horizontal gaps (`max_word_gap`, a multiple of text height) so columns and
  separate controls stay distinct; a phrase's box is the union of its words', its
  text the words rejoined. **Drag always uses phrase granularity**
  (`Detector::detect_phrases`) regardless of the setting, so a whole sentence
  stays selectable. The grouping is geometric — no pixels re-read — so it's
  deterministic and exhaustively unit-tested.
- **§3.4 Hint generation** → `hint/label.rs`. The algorithm grows a breadth-first
  frontier so labels are **prefix-free** (the instant your keys equal a label, the
  choice is unambiguous — no Enter needed) and as short as possible. Live matching
  narrows candidates per keystroke; a dead-end key is rejected without being
  consumed. Placement is `hint/layout.rs`; the chosen point is the **start of the
  element's text** (left edge, vertically centred, nudged in to the first glyph),
  not the phrase centre — a phrase target spans a whole line and its midpoint can
  fall in a gap between words or past the clickable part, whereas the first glyphs
  are reliably on the thing you meant to click.

`detect/mod.rs` keeps a one-method `Detector` trait (so the pipeline is testable
with fakes and open to a future backend) and a shared `finalize()` pass that drops
too-small / off-screen boxes and sorts into reading order, so the shortest labels
land top-left.

### Phase 4 — Interactions

- **§4.1 Clicks** → `pointer.rs`. `XTestFakeButtonEvent` for left/right; `count`
  gives double-click / N-click.
- **§4.2 Drag & select** → `session.rs` `Mode::Drag`: pick a start hint, pick an
  end hint, `MouseDown → move → MouseUp`, then mirror the resulting PRIMARY
  selection into the CLIPBOARD via `arboard` (`interaction/mod.rs`). To make a
  *sentence* selectable, drag mode lays **two** hints per phrase
  (`place_drag_hints`): one on the first glyph (`drag_start`, no inset) and one
  just past the last (`drag_end`). The earlier single-hint-per-phrase drag reused
  the teleport target, which insets into the text — so the press began mid-word
  and the drag grabbed too little or nothing. The start/end pair lets you select
  one whole phrase (its start + its end hint) or span several lines.

A subtlety that bit the first draft: the overlay must be **torn down before** the
synthetic click, or the click lands on our own window. `run_hint` now collects
the target(s) while the overlay is up, hides it, *then* acts.

- **§4.3 Teleport is a clean jump.** An earlier version handed off from a
  teleport straight into free-move (`movement.teleport_then_move`) so the pointer
  could be nudged after landing. It was removed: a teleport should just teleport —
  predictable, no lingering keyboard grab. Fine-tuning lives in `mole move`, which
  you trigger when you actually want it.

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
| Clipboard | `arboard` | X11 backend is itself x11rb-based; handles PRIMARY↔CLIPBOARD |
| Config | `serde` + `toml` + `notify` | Standard, ergonomic, hot-reload |
| CLI | `clap` | Subcommands for free |
| OCR | *(none)* — `tesseract` subprocess | Zero extra build deps; swappable |

## 5. What's tested vs. what needs a display

This was built and unit-tested in a headless sandbox, so be clear-eyed about the
two tiers:

**Unit-tested and trustworthy** (run `cargo test`): geometry (including box
union), config parsing & validation, the hint label algorithm (uniqueness +
prefix-freedom across many sizes), live matching, anti-overlap layout,
adaptive-contrast colour choice, the cairo render producing the expected buffer,
OCR TSV parsing, **phrase grouping** (adjacency, column splits, line tolerance,
degenerate input), the **movement accelerator**, the detector `finalize` pass,
the IPC command round-trip, modifier parsing, keysym mapping. The per-module unit
tests live inline (`#[cfg(test)]`) so they can reach private helpers.

**Integration-tested** (`tests/pipeline.rs`): the seams between modules, which
the isolated unit tests can't see. A `FakeDetector` implementing the public
`Detector` trait feeds the exact chain `run_hint` uses — detect → `generate_labels`
→ `place_hints` → `HintMatcher` — and asserts that typing a label lands on the
right *text start* (not the label-box corner or the phrase centre), that every
label among 30 elements is reachable, that stacked elements stay individually
selectable, that a
dead-end keystroke doesn't strand the user, and that edge boxes clamp on-screen.
All display-free, so it runs in the sandbox alongside the unit tests.

**Structurally complete, needs a real X session to verify end-to-end**: the
GetImage capture, the overlay window creation/keyboard grab, XTest clicks/drags,
and the OCR subprocess round-trip (needs the `tesseract` binary against a real
screen). The flake's `checkFlags` skip these in the sandboxed build for exactly
this reason.

If you pick this up on a real machine, the first things to validate are: (1) the
overlay actually appears transparent under your compositor, (2) `tesseract` reads
your screen and the phrases land where the text is (tune `min_confidence` /
`max_word_gap` for your DPI), (3) XTest clicks land where expected.

## 6. Where to take it next

- **Wayland backend.** The systems modules (`x11/`) are the only X-specific part;
  a `wayland/` sibling implementing capture/overlay/pointer behind the same
  shapes would let `session.rs` stay unchanged. `wlr-layer-shell` for the
  overlay, `wlr-virtual-pointer` for input, the screencopy protocol for capture.
- **Faster OCR.** Swap the `tesseract` subprocess for PaddleOCR behind the same
  `Detector` trait — `detect/ocr/` is already isolated for it.
- **Region scans.** OCR the focused window or the area under the cursor instead
  of the full screen, to cut latency on big displays.
- **Grid mode** for the no-text case (icon-only toolbars, games), as a second
  `Detector` alongside OCR.

## 7. Building & running

mole is a normal cargo project. The only non-Rust pieces are the **cairo**
development library (to build) and the **tesseract** binary (to run); install
those from your distro, then:

```sh
cargo test                 # the pure-logic suite (no display needed)
cargo build --release      # the binary at ./target/release/mole
cargo install --path .     # build release + put `mole` on your PATH (~/.cargo/bin)
mole daemon &              # then bind `mole click` etc. in your WM
```

See the README's [Installation](README.md#installation) section for the short
version. To *use* mole, get the binary onto your `PATH` (via `cargo install` or by
copying `target/release/mole` somewhere on it) rather than calling it through
`./target` each time.

**Nix.** A flake is kept for reproducibility and CI. `nix develop` gives a dev
shell with the toolchain, cairo and tesseract pre-wired (and a `mole` helper that
forwards to `cargo run`); `nix profile install .` installs the packaged binary
into `~/.nix-profile/bin`. On NixOS prefer the flake over `cargo install`, since
a cargo-built binary won't find the nix-store cairo at runtime. The headless test
commands used during development were `nix develop -c cargo test` / `clippy`.
