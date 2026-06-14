//! OCR detection: read the screen, group the words into phrase targets.
//!
//! This is mole's only detector. The work splits cleanly across three helpers:
//!
//! * [`tesseract`] — run the `tesseract` subprocess over a screen capture.
//! * [`tsv`] — parse its TSV output into confident word boxes.
//! * [`phrase`] — group those words into the phrase-level targets we hint.
//!
//! [`OcrDetector`] wires them together and applies the shared [`detect::finalize`]
//! pass (size filtering + reading order) to the result.

mod phrase;
mod tesseract;
mod tsv;

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{self, Detector, Element};
use crate::error::Result;

use phrase::Grouping;
use tesseract::Tesseract;

/// Detector that reads the screen with Tesseract and hints text phrases.
pub struct OcrDetector {
    tesseract: Tesseract,
    min_confidence: f32,
    min_element_size: i32,
    grouping: Grouping,
}

impl OcrDetector {
    pub fn new(config: &Config) -> Self {
        OcrDetector {
            tesseract: Tesseract::new(config.ocr.binary.clone(), config.ocr.language.clone()),
            min_confidence: config.ocr.min_confidence,
            min_element_size: config.ocr.min_element_size,
            grouping: Grouping {
                line_tolerance: config.ocr.line_tolerance,
                max_word_gap: config.ocr.max_word_gap,
            },
        }
    }
}

impl Detector for OcrDetector {
    fn name(&self) -> &'static str {
        "ocr"
    }

    fn detect(&self, screen: &Screen) -> Result<Vec<Element>> {
        let region = screen.bounds();
        let raw = self.tesseract.read(screen)?;
        let words = tsv::parse(&raw, region, self.min_confidence);
        let phrases = phrase::group(words, self.grouping);
        Ok(detect::finalize(phrases, self.min_element_size, region))
    }
}
