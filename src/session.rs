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

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{self, Detector};
use crate::error::Result;
use crate::geometry::Point;
use crate::hint::{generate_labels, place_hints, HintBox, HintMatcher, MatchState};
use crate::interaction;
use crate::motion::{Accelerator, Dir};
use crate::render::Renderer;
use crate::x11::connection::KeyInput;
use crate::x11::overlay::{Overlay, OverlayInput};
use crate::x11::{Button, Conn, Pointer};

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
    pub fn new(conn: &'a Conn, config: &'a Config) -> Result<Self> {
        Ok(Session {
            conn,
            config,
            detector: detect::from_config(config),
            renderer: Renderer::new(config.hints.clone()),
        })
    }

    /// Run a hint-based interaction in `mode`.
    ///
    /// Targets are chosen while the overlay is up; the overlay is then torn down
    /// *before* any synthetic click/drag, so the input lands on the app below
    /// rather than on our own window.
    pub fn run_hint(&self, mode: Mode) -> Result<()> {
        let screen = Screen::capture_full(self.conn)?;
        let elements = self.detector.detect(&screen)?;
        if elements.is_empty() {
            log::info!("no hintable elements found");
            return Ok(());
        }
        log::info!("hinting {} elements", elements.len());

        let labels = generate_labels(&self.config.hint_alphabet(), elements.len());
        let boxes = place_hints(
            &elements,
            &labels,
            self.config.hints.font_size,
            self.config.hints.min_gap,
            screen.bounds(),
        );

        let mut overlay = Overlay::new(self.conn)?;
        overlay.show()?;

        // Gather the target(s) for the mode while the overlay owns the keyboard.
        let targets = match mode {
            Mode::Teleport | Mode::Click { .. } => match self.select(&overlay, &boxes, &screen)? {
                Some(t) => vec![t],
                None => vec![],
            },
            Mode::Drag { .. } => {
                let mut ts = Vec::new();
                if let Some(start) = self.select(&overlay, &boxes, &screen)? {
                    if let Some(end) = self.select(&overlay, &boxes, &screen)? {
                        ts.push(start);
                        ts.push(end);
                    }
                }
                ts
            }
        };

        overlay.hide()?;

        if targets.is_empty() {
            log::info!("selection cancelled");
            return Ok(());
        }

        self.act(mode, &targets)
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
        boxes: &[HintBox],
        screen: &Screen,
    ) -> Result<Option<Point>> {
        let labels: Vec<String> = boxes.iter().map(|b| b.label.clone()).collect();
        let mut matcher = HintMatcher::new(labels);

        self.repaint(overlay, boxes, screen, matcher.typed())?;

        loop {
            match overlay.next_input()? {
                OverlayInput::Key(KeyInput::Escape) => return Ok(None),
                OverlayInput::Click(_) => return Ok(None),
                OverlayInput::Key(KeyInput::Backspace) => {
                    matcher.pop();
                    self.repaint(overlay, boxes, screen, matcher.typed())?;
                }
                OverlayInput::Key(KeyInput::Char(c)) => {
                    match matcher.push(c.to_ascii_lowercase()) {
                        MatchState::Selected(idx) => return Ok(Some(boxes[idx].target)),
                        MatchState::Pending => {
                            self.repaint(overlay, boxes, screen, matcher.typed())?;
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
        boxes: &[HintBox],
        screen: &Screen,
        typed: &str,
    ) -> Result<()> {
        let frame = self.renderer.render(
            overlay.width() as i32,
            overlay.height() as i32,
            boxes,
            typed,
            screen,
        )?;
        overlay.present(&frame.data, frame.stride)
    }

    /// Free pointer movement with hjkl (Plan §1.3). Grabs the keyboard via a
    /// transparent overlay and moves the pointer until Escape/Enter.
    pub fn run_free_move(&self) -> Result<()> {
        let pointer = Pointer::new(self.conn);
        let mut overlay = Overlay::new(self.conn)?;
        overlay.show()?;

        let m = &self.config.movement;
        let k = &self.config.keys;
        let first = |s: &str| s.chars().next().unwrap_or('\0');
        let (left, down, up, right) = (
            first(&k.move_left),
            first(&k.move_down),
            first(&k.move_up),
            first(&k.move_right),
        );
        let mut accel = Accelerator::new(m.step, m.large_step, m.acceleration, m.max_step);

        loop {
            match overlay.next_input()? {
                OverlayInput::Key(KeyInput::Escape)
                | OverlayInput::Key(KeyInput::Enter)
                | OverlayInput::Click(_) => break,
                OverlayInput::Key(KeyInput::Char(c)) => {
                    let large = c.is_uppercase();
                    let lc = c.to_ascii_lowercase();
                    let dir = if lc == left {
                        Some(Dir::Left)
                    } else if lc == right {
                        Some(Dir::Right)
                    } else if lc == up {
                        Some(Dir::Up)
                    } else if lc == down {
                        Some(Dir::Down)
                    } else {
                        None
                    };
                    if let Some(dir) = dir {
                        let (dx, dy) = accel.next(dir, large);
                        pointer.move_relative(dx, dy)?;
                    }
                }
                OverlayInput::Key(_) => {}
            }
        }

        overlay.hide()?;
        Ok(())
    }
}
