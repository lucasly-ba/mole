//! A single interaction, start to finish.
//!
//! A [`Session`] wires together capture → detect → label → overlay → input →
//! act. The daemon creates one per trigger. Modes:
//!
//! * [`Mode::Teleport`] — jump the pointer to a hinted element.
//! * [`Mode::Click`] — jump and click (N times / different buttons).
//! * [`Mode::Drag`] — pick two hints and drag between them, copying the
//!   resulting selection to the clipboard (Plan §4.2).
//!
//! Free pointer movement (Plan §1.3, hjkl) is [`Session::run_free_move`], which
//! needs no detection or hints at all.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{self, Detector, Element, Scan, ScanCache};
use crate::error::Result;
use crate::geometry::Point;
use crate::hint::{
    generate_labels, place_drag_hints, place_hints, place_hints_avoiding, HintBox, HintMatcher,
    MatchState,
};
use crate::interaction;
use crate::motion::Glide;
use crate::render::{Backdrop, Renderer};
use crate::x11::connection::KeyInput;
use crate::x11::overlay::{Overlay, OverlayInput};
use crate::x11::{Button, Conn, KeyboardGrab, Pointer};

/// Spare labels reserved beyond the immediately-shown hints, so late-arriving
/// (background-OCR'd) hints get stable labels without disturbing the ones the
/// user can already see.
const LABEL_RESERVE: usize = 64;
/// Idle poll interval for the incremental select loop (ms): low enough to feel
/// instant, high enough to cost no real CPU while waiting.
const POLL_MS: u64 = 8;
/// Free-move frame time (ms): the pointer is warped and the keyboard polled once
/// a frame, so this is both the glide's time-step and its input latency. ~125 Hz
/// is smooth to the eye and imperceptibly responsive.
const MOVE_TICK_MS: u64 = 8;

/// What a hint session does once a target is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Teleport,
    Click { button: Button, count: u32 },
    Drag { button: Button },
}

/// Holds the long-lived pieces shared across interactions.
pub struct Session<'a> {
    conn: &'a Conn,
    config: &'a Config,
    detector: Box<dyn Detector>,
    renderer: Renderer,
}

impl<'a> Session<'a> {
    pub fn new(conn: &'a Conn, config: &'a Config, cache: Arc<Mutex<ScanCache>>) -> Result<Self> {
        Ok(Session {
            conn,
            config,
            detector: detect::from_config(config, cache),
            renderer: Renderer::new(config.hints.clone()),
        })
    }

    /// Run a hint-based interaction in `mode`.
    ///
    /// Targets are chosen while the overlay is up; the overlay is then torn down
    /// *before* any synthetic click/drag, so the input lands on the app below
    /// rather than on our own window.
    pub fn run_hint(&self, mode: Mode) -> Result<()> {
        let t0 = std::time::Instant::now();
        let screen = Screen::capture_full(self.conn)?;
        let t_cap = t0.elapsed();

        let mut overlay = Overlay::new(self.conn)?;
        // Build the desktop backdrop while the overlay is still unmapped: it is a
        // per-pixel pass over the whole screen, so doing it before `show` keeps
        // the window from flashing black before the first frame lands. It is then
        // reused for every repaint instead of being rebuilt per keystroke.
        let backdrop =
            self.renderer
                .backdrop(overlay.width() as i32, overlay.height() as i32, &screen)?;

        // Drag needs the whole element set up front (it picks twice), so it scans
        // synchronously. Teleport/Click show cached hints immediately and fold in
        // the re-OCR of changed regions as it arrives.
        let (targets, pending): (Vec<Point>, Option<JoinHandle<()>>) = match mode {
            Mode::Drag { .. } => (
                self.run_drag(&mut overlay, &backdrop, &screen, t_cap)?,
                None,
            ),
            _ => {
                let scan = self.detector.detect_split(&screen)?;
                log::info!(
                    "scan: capture {}ms + scan {}ms -> {} ready{}",
                    t_cap.as_millis(),
                    (t0.elapsed() - t_cap).as_millis(),
                    scan.ready.len(),
                    if scan.pending.is_some() {
                        " (+changed regions loading)"
                    } else {
                        ""
                    },
                );
                if scan.ready.is_empty() && scan.pending.is_none() {
                    log::info!("no hintable elements found");
                    return Ok(());
                }
                overlay.show()?;
                let (picked, handle) =
                    self.select_incremental(&overlay, &backdrop, &screen, scan)?;
                (picked.into_iter().collect(), handle)
            }
        };

        overlay.hide()?;
        if targets.is_empty() {
            log::info!("selection cancelled");
            // Make sure the background re-OCR finished (and its cache splice
            // landed) before returning, so it can't race the next pre-warm.
            if let Some(handle) = pending {
                let _ = handle.join();
            }
            return Ok(());
        }
        let act_result = self.act(mode, &targets);
        if let Some(handle) = pending {
            let _ = handle.join();
        }
        act_result?;
        Ok(())
    }

    /// Synchronous two-pick drag selection: returns `[start, end]`, or empty if
    /// either pick was cancelled.
    fn run_drag(
        &self,
        overlay: &mut Overlay,
        backdrop: &Backdrop,
        screen: &Screen,
        t_cap: Duration,
    ) -> Result<Vec<Point>> {
        let elements = self.detector.detect_phrases(screen)?;
        log::info!(
            "scan: capture {}ms -> {} elements (drag)",
            t_cap.as_millis(),
            elements.len(),
        );
        if elements.is_empty() {
            log::info!("no hintable elements found");
            return Ok(Vec::new());
        }
        // Two hints per phrase (start + end), so a whole sentence can be selected
        // by picking its start hint and its end hint.
        let labels = generate_labels(&self.config.hint_alphabet(), elements.len() * 2);
        let boxes = place_drag_hints(
            &elements,
            &labels,
            self.config.hints.font_size,
            self.config.hints.min_gap,
            screen.bounds(),
        );
        overlay.show()?;
        let Some(start) = self.select(overlay, backdrop, &boxes, screen)? else {
            return Ok(Vec::new());
        };
        let Some(end) = self.select(overlay, backdrop, &boxes, screen)? else {
            return Ok(Vec::new());
        };
        Ok(vec![start, end])
    }

    /// Perform the pointer action for `mode` on the already-chosen `targets`.
    fn act(&self, mode: Mode, targets: &[Point]) -> Result<()> {
        let pointer = Pointer::new(self.conn);
        match mode {
            Mode::Teleport => pointer.move_to(targets[0])?,
            Mode::Click { button, count } => {
                pointer.move_to(targets[0])?;
                pointer.click(button, count)?;
            }
            Mode::Drag { button } => {
                pointer.drag(targets[0], targets[1], button)?;
                match interaction::copy_primary_to_clipboard() {
                    Ok(text) => log::info!("copied {} chars to clipboard", text.len()),
                    Err(e) => log::warn!("clipboard copy failed: {e}"),
                }
            }
        }
        Ok(())
    }

    /// Run the keystroke loop until a hint is selected or the user cancels.
    /// Returns the chosen target point, or `None` on cancel.
    fn select(
        &self,
        overlay: &Overlay,
        backdrop: &Backdrop,
        boxes: &[HintBox],
        screen: &Screen,
    ) -> Result<Option<Point>> {
        let labels: Vec<String> = boxes.iter().map(|b| b.label.clone()).collect();
        let mut matcher = HintMatcher::new(labels);

        self.repaint(overlay, backdrop, boxes, screen, matcher.typed())?;

        loop {
            match overlay.next_input()? {
                OverlayInput::Key(KeyInput::Escape) => return Ok(None),
                OverlayInput::Click(_) => return Ok(None),
                OverlayInput::Key(KeyInput::Backspace) => {
                    matcher.pop();
                    self.repaint(overlay, backdrop, boxes, screen, matcher.typed())?;
                }
                OverlayInput::Key(KeyInput::Char { c, .. }) => {
                    match matcher.push(c.to_ascii_lowercase()) {
                        MatchState::Selected(idx) => return Ok(Some(boxes[idx].target)),
                        MatchState::Pending => {
                            self.repaint(overlay, backdrop, boxes, screen, matcher.typed())?;
                        }
                        MatchState::NoMatch => { /* dead end: ignore the key */ }
                    }
                }
                OverlayInput::Key(_) => {}
            }
        }
    }

    fn repaint(
        &self,
        overlay: &Overlay,
        backdrop: &Backdrop,
        boxes: &[HintBox],
        screen: &Screen,
        typed: &str,
    ) -> Result<()> {
        let frame = self.renderer.render_onto(backdrop, boxes, typed, screen)?;
        overlay.present(&frame.data, frame.stride)
    }

    /// Selection loop that shows the ready (cached) hints immediately and folds
    /// in the re-OCR of changed regions when it completes, without disturbing the
    /// hints already on screen. Returns the chosen target (or `None`) plus the
    /// background worker's handle so the caller can join it.
    fn select_incremental(
        &self,
        overlay: &Overlay,
        backdrop: &Backdrop,
        screen: &Screen,
        scan: Scan,
    ) -> Result<(Option<Point>, Option<JoinHandle<()>>)> {
        let alphabet = self.config.hint_alphabet();
        let bounds = screen.bounds();

        let n1 = scan.ready.len();
        // One stable label pool: the ready hints take the first `n1`, late hints
        // draw from the reserve, so on-screen labels never change as more appear.
        let labels = generate_labels(&alphabet, n1 + LABEL_RESERVE);
        let mut boxes = place_hints(
            &scan.ready,
            &labels[..n1],
            self.config.hints.font_size,
            self.config.hints.min_gap,
            bounds,
        );
        let mut matcher = HintMatcher::new(labels[..n1].to_vec());
        self.repaint(overlay, backdrop, &boxes, screen, matcher.typed())?;

        // Re-OCR the changed regions off-thread, if there are any.
        let mut rx: Option<Receiver<Result<Vec<Element>>>> = None;
        let mut handle: Option<JoinHandle<()>> = None;
        if let Some(rest) = scan.pending {
            let (tx, receiver) = mpsc::channel();
            handle = Some(std::thread::spawn(move || {
                let _ = tx.send(rest());
            }));
            rx = Some(receiver);
        }

        let picked = loop {
            // Fold in the late hints once, when they arrive.
            let mut clear_rx = false;
            if let Some(receiver) = &rx {
                match receiver.try_recv() {
                    Ok(Ok(extra)) => {
                        self.add_hints(
                            overlay,
                            backdrop,
                            screen,
                            &mut boxes,
                            &mut matcher,
                            &labels,
                            &extra,
                        )?;
                        clear_rx = true;
                    }
                    Ok(Err(e)) => {
                        log::warn!("background OCR failed: {e}");
                        clear_rx = true;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => clear_rx = true,
                }
            }
            if clear_rx {
                rx = None;
            }

            match overlay.poll_input()? {
                Some(OverlayInput::Key(KeyInput::Escape)) | Some(OverlayInput::Click(_)) => {
                    break None;
                }
                Some(OverlayInput::Key(KeyInput::Backspace)) => {
                    matcher.pop();
                    self.repaint(overlay, backdrop, &boxes, screen, matcher.typed())?;
                }
                Some(OverlayInput::Key(KeyInput::Char { c, .. })) => {
                    match matcher.push(c.to_ascii_lowercase()) {
                        MatchState::Selected(idx) => break Some(boxes[idx].target),
                        MatchState::Pending => {
                            self.repaint(overlay, backdrop, &boxes, screen, matcher.typed())?;
                        }
                        MatchState::NoMatch => {}
                    }
                }
                Some(OverlayInput::Key(_)) => {}
                None => std::thread::sleep(Duration::from_millis(POLL_MS)),
            }
        };

        Ok((picked, handle))
    }

    /// Add late-arriving `extra` hints to the live overlay using reserved labels,
    /// without moving the existing boxes, then repaint. Existing labels (and any
    /// keys already typed) are preserved.
    #[allow(clippy::too_many_arguments)]
    fn add_hints(
        &self,
        overlay: &Overlay,
        backdrop: &Backdrop,
        screen: &Screen,
        boxes: &mut Vec<HintBox>,
        matcher: &mut HintMatcher,
        labels: &[String],
        extra: &[Element],
    ) -> Result<()> {
        let used = boxes.len();
        let take = extra.len().min(labels.len().saturating_sub(used));
        if take == 0 {
            return Ok(());
        }
        let new = place_hints_avoiding(
            &extra[..take],
            &labels[used..used + take],
            self.config.hints.font_size,
            self.config.hints.min_gap,
            screen.bounds(),
            boxes,
        );
        boxes.extend(new);

        // Rebuild the matcher over the full label set, replaying the typed prefix
        // so an in-progress selection survives the addition.
        let all: Vec<String> = boxes.iter().map(|b| b.label.clone()).collect();
        let typed = matcher.typed().to_string();
        let mut rebuilt = HintMatcher::new(all);
        for c in typed.chars() {
            let _ = rebuilt.push(c);
        }
        *matcher = rebuilt;
        self.repaint(overlay, backdrop, boxes, screen, matcher.typed())
    }

    /// Free pointer movement (Plan §1.3): grab the keyboard and glide the pointer
    /// with hjkl over the live desktop until Escape. The glide accelerates while a
    /// direction is held and a boost key crosses the screen fast; action keys
    /// click, double-click, right-click and drag-select where the pointer rests.
    ///
    /// No overlay is shown — the pointer moves over the real desktop and the
    /// synthetic clicks fall through to the apps beneath (see [`KeyboardGrab`]).
    pub fn run_free_move(&self) -> Result<()> {
        let pointer = Pointer::new(self.conn);
        let grab = KeyboardGrab::new(self.conn)?;

        let m = &self.config.movement;
        let k = &self.config.keys;
        let (left, down, up, right) = (
            key_char(&k.move_left),
            key_char(&k.move_down),
            key_char(&k.move_up),
            key_char(&k.move_right),
        );
        let boost_key = key_char(&k.speed_boost);
        let (left_click, right_click, double_click, drag_key) = (
            key_char(&k.left_click),
            key_char(&k.right_click),
            key_char(&k.double_click),
            key_char(&k.drag),
        );

        let mut glide = Glide::new(m.speed, m.max_speed, m.acceleration);
        let (mut hold_left, mut hold_right, mut hold_up, mut hold_down) =
            (false, false, false, false);
        let mut boosting = false;
        let mut dragging = false;

        let tick = Duration::from_millis(MOVE_TICK_MS);
        let mut last = std::time::Instant::now();

        let result = loop {
            let mut quit = false;
            for ev in grab.drain()? {
                // Arrow keys steer alongside the configured letter keys.
                match ev.input {
                    KeyInput::Escape => {
                        quit = true;
                        continue;
                    }
                    KeyInput::Left => {
                        hold_left = ev.pressed;
                        continue;
                    }
                    KeyInput::Right => {
                        hold_right = ev.pressed;
                        continue;
                    }
                    KeyInput::Up => {
                        hold_up = ev.pressed;
                        continue;
                    }
                    KeyInput::Down => {
                        hold_down = ev.pressed;
                        continue;
                    }
                    _ => {}
                }
                let lc = match ev.input {
                    KeyInput::Char { c, .. } => c.to_ascii_lowercase(),
                    _ => continue,
                };
                // Direction and boost keys track their held state every frame.
                if lc == left {
                    hold_left = ev.pressed;
                } else if lc == right {
                    hold_right = ev.pressed;
                } else if lc == up {
                    hold_up = ev.pressed;
                } else if lc == down {
                    hold_down = ev.pressed;
                } else if lc == boost_key {
                    boosting = ev.pressed;
                } else if ev.pressed {
                    // Action keys fire once, on the press.
                    if lc == left_click {
                        pointer.click(Button::Left, 1)?;
                    } else if lc == right_click {
                        pointer.click(Button::Right, 1)?;
                    } else if lc == double_click {
                        pointer.click(Button::Left, 2)?;
                    } else if lc == drag_key {
                        dragging = self.toggle_drag(&pointer, dragging)?;
                    }
                }
            }
            if quit {
                break Ok(());
            }

            let now = std::time::Instant::now();
            let dt = now.duration_since(last).as_secs_f64();
            last = now;
            let dx = hold_right as i32 - hold_left as i32;
            let dy = hold_down as i32 - hold_up as i32;
            let boost = if boosting { m.boost } else { 1.0 };
            let (mx, my) = glide.tick(dx, dy, dt, boost);
            if mx != 0 || my != 0 {
                pointer.move_relative(mx, my)?;
            }

            std::thread::sleep(tick);
        };

        // Never leave the button stuck down if we exit mid-drag.
        if dragging {
            let _ = pointer.release(Button::Left);
        }
        result
    }

    /// Flip a left-button drag on or off. Pressing starts it; pressing again
    /// releases and copies the resulting selection to the clipboard (Plan §4.2).
    /// Returns the new dragging state.
    fn toggle_drag(&self, pointer: &Pointer, dragging: bool) -> Result<bool> {
        if dragging {
            pointer.release(Button::Left)?;
            match interaction::copy_primary_to_clipboard() {
                Ok(text) => log::info!("copied {} chars to clipboard", text.len()),
                Err(e) => log::warn!("clipboard copy failed: {e}"),
            }
            Ok(false)
        } else {
            pointer.press(Button::Left)?;
            Ok(true)
        }
    }
}

/// The character a configured key string maps to. A few keys have no printable
/// character, so a name is accepted; today that's just `"space"`.
fn key_char(s: &str) -> char {
    match s {
        "space" => ' ',
        _ => s.chars().next().unwrap_or('\0'),
    }
}
