//! End-to-end tests for the display-free spine of the pipeline.
//!
//! The per-module unit tests check each stage in isolation. These integration
//! tests check the *seams*: a [`Detector`]'s output flowing through label
//! generation, anti-overlap layout and keystroke matching, exactly as
//! `session::Session::run_hint` wires them together — minus the X11/cairo I/O,
//! which needs a real display and is covered by `JOURNEY.md §5` instead.
//!
//! Being a `tests/` crate, this sees only mole's public API, so it doubles
//! as a check that the pipeline is usable from outside the crate.

use mole::capture::Screen;
use mole::detect::{Detector, Element};
use mole::geometry::{Point, Rect};
use mole::hint::{generate_labels, place_hints, HintBox, HintMatcher, MatchState};
use mole::Result;

const ALPHABET: &str = "asdfghjkl";
const FONT_SIZE: f64 = 13.0;
const MIN_GAP: i32 = 4;

/// A stand-in detector that simply replays a fixed element list, so the pure
/// stages downstream can be driven from a real [`Detector`] without a display.
struct FakeDetector {
    elements: Vec<Element>,
}

impl Detector for FakeDetector {
    fn detect(&self, _screen: &Screen) -> Result<Vec<Element>> {
        Ok(self.elements.clone())
    }
    fn name(&self) -> &'static str {
        "fake"
    }
}

/// A blank off-screen-free capture to satisfy the [`Detector`] signature and the
/// layout's screen bounds. Contents don't matter here — nothing samples pixels.
fn blank_screen(w: i32, h: i32) -> Screen {
    Screen::from_raw(
        Rect::new(0, 0, w, h),
        vec![0u8; (w * h * 4) as usize],
        (w * 4) as usize,
        4,
    )
}

fn alphabet() -> Vec<char> {
    ALPHABET.chars().collect()
}

/// Reproduce the exact wiring of `Session::run_hint`: detect → labels → boxes.
fn run_layout(detector: &FakeDetector, screen: &Screen) -> Result<Vec<HintBox>> {
    let elements = detector.detect(screen)?;
    let labels = generate_labels(&alphabet(), elements.len());
    Ok(place_hints(
        &elements,
        &labels,
        FONT_SIZE,
        MIN_GAP,
        screen.bounds(),
    ))
}

/// Drive the matcher with a full label string from a fresh state, like a user
/// typing it with the overlay up. Returns the terminal [`MatchState`].
fn type_label(boxes: &[HintBox], label: &str) -> MatchState {
    let labels: Vec<String> = boxes.iter().map(|b| b.label.clone()).collect();
    let mut matcher = HintMatcher::new(labels);
    let mut state = MatchState::Pending;
    for c in label.chars() {
        state = matcher.push(c);
    }
    state
}

fn elem(x: i32, y: i32, w: i32, h: i32) -> Element {
    Element::new(Rect::new(x, y, w, h), "t")
}

#[test]
fn detect_to_target_roundtrip() {
    // Three well-separated elements; the second one is what we'll "click".
    let detector = FakeDetector {
        elements: vec![
            elem(10, 10, 40, 20),
            elem(300, 200, 60, 30),
            elem(700, 500, 40, 20),
        ],
    };
    let screen = blank_screen(800, 600);
    let boxes = run_layout(&detector, &screen).unwrap();

    assert_eq!(boxes.len(), 3, "one box per detected element");

    // Type the second box's label and confirm the matcher selects exactly it,
    // and that its target is the *start of the element's text* (left edge nudged
    // in, vertically centred), not the label box corner or the phrase centre.
    let target_box = &boxes[1];
    match type_label(&boxes, &target_box.label) {
        MatchState::Selected(idx) => {
            assert_eq!(idx, 1);
            // elem(300, 200, 60, 30): inset = min(15, 30) = 15.
            assert_eq!(boxes[idx].target, Point::new(315, 215));
        }
        other => panic!("expected Selected(1), got {other:?}"),
    }
}

#[test]
fn every_element_is_reachable_by_typing_its_label() {
    // Enough elements (>alphabet length) to force multi-character labels, laid
    // out on a grid. Whatever the layout does to the *boxes*, every *target*
    // must stay typeable and map back to the right element's text start.
    let mut elements = Vec::new();
    let mut expected_targets = Vec::new();
    for row in 0..6 {
        for col in 0..5 {
            let (x, y) = (40 + col * 140, 40 + row * 90);
            elements.push(elem(x, y, 50, 24));
            // text_start: left edge + min(h/2, w/2) in, vertically centred.
            expected_targets.push(Point::new(x + 12, y + 12));
        }
    }
    let detector = FakeDetector { elements };
    let screen = blank_screen(800, 600);
    let boxes = run_layout(&detector, &screen).unwrap();

    assert_eq!(boxes.len(), 30);

    // Labels must be unique (so a typed label is unambiguous).
    let unique: std::collections::HashSet<_> = boxes.iter().map(|b| &b.label).collect();
    assert_eq!(unique.len(), boxes.len(), "labels are not unique");

    // Exhaustively: typing each box's label selects that index, whose target is
    // the matching element's centre.
    for (idx, b) in boxes.iter().enumerate() {
        assert_eq!(
            type_label(&boxes, &b.label),
            MatchState::Selected(idx),
            "label {:?} did not select its own box",
            b.label
        );
        assert_eq!(
            b.target, expected_targets[idx],
            "box {idx} points at the wrong text start"
        );
    }
}

#[test]
fn stacked_elements_stay_individually_selectable() {
    // Three elements on the exact same spot (e.g. overlapping phrases the OCR
    // pass reports at the same origin). Layout must separate the boxes, and each
    // must remain reachable with its own keys.
    let detector = FakeDetector {
        elements: vec![
            elem(100, 100, 30, 16),
            elem(100, 100, 30, 16),
            elem(100, 100, 30, 16),
        ],
    };
    let screen = blank_screen(800, 600);
    let boxes = run_layout(&detector, &screen).unwrap();

    assert_eq!(boxes.len(), 3);
    for (idx, b) in boxes.iter().enumerate() {
        assert_eq!(type_label(&boxes, &b.label), MatchState::Selected(idx));
    }
}

#[test]
fn dead_end_keystroke_does_not_strand_the_user() {
    // Mirrors the overlay loop: a key that matches no remaining label is ignored
    // (NoMatch, input not consumed), so a valid label can still be completed.
    let detector = FakeDetector {
        elements: vec![elem(10, 10, 40, 20), elem(300, 200, 40, 20)],
    };
    let screen = blank_screen(800, 600);
    let boxes = run_layout(&detector, &screen).unwrap();

    let labels: Vec<String> = boxes.iter().map(|b| b.label.clone()).collect();
    let mut matcher = HintMatcher::new(labels);

    // A character outside the alphabet is a dead end and leaves state intact.
    assert_eq!(matcher.push('z'), MatchState::NoMatch);
    assert_eq!(matcher.typed(), "");

    // The first label is still fully typeable afterwards.
    let mut state = MatchState::Pending;
    for c in boxes[0].label.chars() {
        state = matcher.push(c);
    }
    assert_eq!(state, MatchState::Selected(0));
}

#[test]
fn boxes_stay_on_screen_for_edge_elements() {
    // Elements jammed into the screen's corners: every label box must end up
    // fully within the screen after the layout's clamping.
    let screen_rect = Rect::new(0, 0, 800, 600);
    let detector = FakeDetector {
        elements: vec![
            elem(0, 0, 10, 10),
            elem(795, 0, 10, 10),
            elem(0, 595, 10, 10),
            elem(795, 595, 10, 10),
        ],
    };
    let screen = blank_screen(800, 600);
    let boxes = run_layout(&detector, &screen).unwrap();

    for b in &boxes {
        assert_eq!(
            b.rect.clamp_to(screen_rect),
            Some(b.rect),
            "box {:?} spills off-screen",
            b.label
        );
    }
}

#[test]
fn no_elements_yields_no_hints() {
    // The "nothing hintable" branch of run_hint: empty detection means no labels
    // and no boxes — and crucially, generate_labels(_, 0) must not panic.
    let detector = FakeDetector { elements: vec![] };
    let screen = blank_screen(800, 600);
    let boxes = run_layout(&detector, &screen).unwrap();
    assert!(boxes.is_empty());
}
