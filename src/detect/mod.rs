//! Text/element detection: the backends that find clickable targets on screen.
//!
//! Two backends are provided:
//!
//! * [`atspi`] — the primary path. Queries the AT-SPI accessibility tree over
//!   D-Bus. Near-instant, gives precise rectangles, roles and text.
//! * [`ocr`] — the fallback. Screenshots the display and runs Tesseract over it
//!   for apps that expose nothing to AT-SPI (Electron, terminals, games).
//!
//! Both implement [`Detector`], and [`CompositeDetector`] runs a configured list
//! of them, deduplicating overlapping results.

pub mod atspi;
#[cfg(feature = "ocr")]
pub mod ocr;

use crate::capture::Screen;
use crate::config::{Backend, Config};
use crate::error::Result;
use crate::geometry::Rect;

/// The semantic kind of a detected element, used for nothing functional yet but
/// handy for prioritising and for future role-filtered modes ("hint only links").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Link,
    Button,
    TextField,
    MenuItem,
    Label,
    Other,
}

impl Role {
    /// Map an AT-SPI numeric role to our coarse enum. Values follow the
    /// `AtspiRole` enumeration from the AT-SPI2 spec.
    pub fn from_atspi(code: u32) -> Role {
        match code {
            // PUSH_BUTTON, TOGGLE_BUTTON, SPIN_BUTTON, RADIO_BUTTON, CHECK_BOX
            43 | 63 | 52 | 44 | 7 => Role::Button,
            // LINK
            88 => Role::Link,
            // ENTRY, TEXT, PASSWORD_TEXT
            71 | 60 | 40 => Role::TextField,
            // MENU_ITEM, CHECK_MENU_ITEM, RADIO_MENU_ITEM, LIST_ITEM
            34 | 9 | 45 | 33 => Role::MenuItem,
            // LABEL, STATIC, HEADING
            29 | 84 | 81 => Role::Label,
            _ => Role::Other,
        }
    }

    /// Whether this role is worth offering as a hint by default. Pure labels are
    /// still useful (you may want to position the cursor on text to select it),
    /// so everything is hintable for now; this hook exists for future filtering.
    pub fn is_interactive(&self) -> bool {
        true
    }
}

/// Which backend produced an [`Element`]. Lets the deduplicator prefer the more
/// trustworthy source when two backends report the same spot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    AtSpi,
    Ocr,
}

/// A single detected, hintable target on screen.
#[derive(Debug, Clone)]
pub struct Element {
    /// Bounding box in absolute screen coordinates.
    pub rect: Rect,
    /// Human-readable text/label, if any.
    pub text: String,
    pub role: Role,
    pub source: Source,
}

impl Element {
    pub fn new(rect: Rect, text: impl Into<String>, role: Role, source: Source) -> Self {
        Element {
            rect,
            text: text.into(),
            role,
            source,
        }
    }
}

/// A source of hintable elements.
pub trait Detector {
    /// Find all hintable elements visible on `screen`.
    fn detect(&self, screen: &Screen) -> Result<Vec<Element>>;

    /// A short name for logs.
    fn name(&self) -> &'static str;
}

/// Runs several detectors in order and merges their output.
pub struct CompositeDetector {
    detectors: Vec<Box<dyn Detector>>,
    min_size: i32,
    require_text: bool,
}

impl CompositeDetector {
    /// Build the detector list described by `config.detection.backends`.
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut detectors: Vec<Box<dyn Detector>> = Vec::new();
        for backend in &config.detection.backends {
            match backend {
                Backend::AtSpi => {
                    detectors.push(Box::new(atspi::AtSpiDetector::new(config)?));
                }
                Backend::Ocr => {
                    #[cfg(feature = "ocr")]
                    detectors.push(Box::new(ocr::OcrDetector::new(config)));
                    #[cfg(not(feature = "ocr"))]
                    log::warn!("OCR backend requested but the `ocr` feature is disabled");
                }
            }
        }
        Ok(CompositeDetector {
            detectors,
            min_size: config.detection.min_element_size,
            require_text: config.detection.require_text,
        })
    }

    /// Detect with every backend, filter noise, and deduplicate.
    pub fn detect(&self, screen: &Screen) -> Result<Vec<Element>> {
        let mut all = Vec::new();
        for detector in &self.detectors {
            match detector.detect(screen) {
                Ok(found) => {
                    log::debug!("{} found {} elements", detector.name(), found.len());
                    all.extend(found);
                }
                // A backend failing (e.g. no a11y bus) must not abort the others.
                Err(e) => log::warn!("detector {} failed: {e}", detector.name()),
            }
        }
        Ok(self.filter_and_dedup(all, screen.bounds()))
    }

    /// Drop too-small / empty elements and remove near-duplicates, preferring
    /// AT-SPI results over OCR ones for the same location.
    fn filter_and_dedup(&self, mut elements: Vec<Element>, screen: Rect) -> Vec<Element> {
        elements.retain(|e| {
            e.rect.width >= self.min_size
                && e.rect.height >= self.min_size
                && e.rect.clamp_to(screen).is_some()
                && (!self.require_text || !e.text.trim().is_empty())
        });

        // Prefer AT-SPI: process those first so OCR duplicates are dropped.
        elements.sort_by_key(|e| match e.source {
            Source::AtSpi => 0,
            Source::Ocr => 1,
        });

        let mut kept: Vec<Element> = Vec::with_capacity(elements.len());
        for e in elements {
            let dup = kept.iter().any(|k| centers_close(k.rect, e.rect));
            if !dup {
                kept.push(e);
            }
        }

        // Reading order: top-to-bottom, then left-to-right, so the shortest
        // (first-assigned) labels land on the top-left elements.
        kept.sort_by_key(|e| (e.rect.center().y, e.rect.center().x));
        kept
    }
}

/// Two rectangles are "the same target" when their centres are within a small
/// radius of each other.
fn centers_close(a: Rect, b: Rect) -> bool {
    const RADIUS: i32 = 12;
    let ca = a.center();
    let cb = b.center();
    (ca.x - cb.x).abs() <= RADIUS && (ca.y - cb.y).abs() <= RADIUS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(x: i32, y: i32, src: Source) -> Element {
        Element::new(Rect::new(x, y, 30, 16), "x", Role::Link, src)
    }

    fn detector(min_size: i32, require_text: bool) -> CompositeDetector {
        CompositeDetector {
            detectors: vec![],
            min_size,
            require_text,
        }
    }

    #[test]
    fn dedup_prefers_atspi_over_ocr() {
        let d = detector(6, false);
        // Same spot reported by both backends.
        let input = vec![el(100, 100, Source::Ocr), el(102, 101, Source::AtSpi)];
        let out = d.filter_and_dedup(input, Rect::new(0, 0, 1920, 1080));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, Source::AtSpi);
    }

    #[test]
    fn dedup_keeps_distinct_elements_in_reading_order() {
        let d = detector(6, false);
        let input = vec![el(500, 500, Source::AtSpi), el(10, 10, Source::AtSpi)];
        let out = d.filter_and_dedup(input, Rect::new(0, 0, 1920, 1080));
        assert_eq!(out.len(), 2);
        // Top-left one comes first.
        assert_eq!(out[0].rect.x, 10);
    }

    #[test]
    fn tiny_elements_are_filtered() {
        let d = detector(20, false);
        let input = vec![el(100, 100, Source::AtSpi)]; // 30x16, height < 20
        let out = d.filter_and_dedup(input, Rect::new(0, 0, 1920, 1080));
        assert!(out.is_empty());
    }
}
