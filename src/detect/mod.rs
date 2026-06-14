//! Detection: finding hintable targets on screen.
//!
//! mole is OCR-driven. When a hint interaction starts, the screen is captured
//! and read with Tesseract, and the recognised words are grouped into
//! *phrase-level* targets — a run of words separated by spaces, like a line of a
//! menu or a sentence in a document. Every visible piece of text becomes
//! something you can jump to, regardless of whether the app underneath exposes
//! anything to accessibility.
//!
//! The [`Detector`] trait is the seam the rest of the pipeline talks to. There
//! is one implementation today, [`ocr::OcrDetector`]; the trait keeps the
//! pipeline testable with fakes and leaves room for a future backend (PaddleOCR,
//! a Wayland screencopy reader, …) without touching the call sites.

pub mod ocr;

use crate::capture::Screen;
use crate::config::Config;
use crate::error::Result;
use crate::geometry::Rect;

/// A single detected, hintable target on screen: a phrase of text and the box
/// that bounds it, in absolute screen coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Bounding box in absolute screen coordinates.
    pub rect: Rect,
    /// The recognised text of the phrase.
    pub text: String,
}

impl Element {
    pub fn new(rect: Rect, text: impl Into<String>) -> Self {
        Element {
            rect,
            text: text.into(),
        }
    }
}

/// A source of hintable elements.
pub trait Detector {
    /// Find all hintable targets visible on `screen`.
    fn detect(&self, screen: &Screen) -> Result<Vec<Element>>;

    /// A short name for logs.
    fn name(&self) -> &'static str;
}

/// Build the detector described by `config`. mole is OCR-only, so this is always
/// the OCR backend; the indirection keeps `session` agnostic of which detector
/// it drives.
pub fn from_config(config: &Config) -> Box<dyn Detector> {
    Box::new(ocr::OcrDetector::new(config))
}

/// Post-process a raw element list into what the hint pipeline expects: drop
/// boxes that are too small or off-screen, then sort into reading order so the
/// shortest labels land on the top-left targets.
///
/// Kept here (rather than in a single backend) so every present and future
/// [`Detector`] shares one notion of "clean, ordered targets".
pub fn finalize(mut elements: Vec<Element>, min_size: i32, screen: Rect) -> Vec<Element> {
    elements.retain(|e| {
        e.rect.width >= min_size && e.rect.height >= min_size && e.rect.clamp_to(screen).is_some()
    });
    // Reading order: top-to-bottom, then left-to-right.
    elements.sort_by_key(|e| (e.rect.center().y, e.rect.center().x));
    elements
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(x: i32, y: i32, w: i32, h: i32) -> Element {
        Element::new(Rect::new(x, y, w, h), "t")
    }

    #[test]
    fn finalize_drops_tiny_elements() {
        let screen = Rect::new(0, 0, 1920, 1080);
        let out = finalize(vec![el(100, 100, 4, 4)], 6, screen);
        assert!(out.is_empty(), "below min_size is filtered");
    }

    #[test]
    fn finalize_drops_offscreen_elements() {
        let screen = Rect::new(0, 0, 800, 600);
        let out = finalize(vec![el(2000, 100, 40, 16)], 6, screen);
        assert!(out.is_empty(), "fully off-screen is filtered");
    }

    #[test]
    fn finalize_sorts_into_reading_order() {
        let screen = Rect::new(0, 0, 1920, 1080);
        let out = finalize(vec![el(500, 500, 40, 16), el(10, 10, 40, 16)], 6, screen);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rect.x, 10, "top-left comes first");
    }
}
