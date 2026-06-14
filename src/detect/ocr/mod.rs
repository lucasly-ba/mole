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
//!
//! Tesseract is single-threaded per image and scanning a whole desktop is the
//! dominant cost of a hint, so the screen is split into horizontal strips that
//! are OCR'd in parallel (one `tesseract` process each) and merged. Strips
//! overlap by [`TILE_OVERLAP`] pixels so a line of text on a cut isn't lost;
//! the duplicate words that overlap creates are removed in [`dedup_words`].

mod phrase;
mod tesseract;
mod tsv;

use std::sync::Arc;

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{self, Detector, Element};
use crate::error::Result;
use crate::geometry::Rect;

use phrase::{Grouping, Word};
use tesseract::Tesseract;

/// Vertical overlap (px) between adjacent strips. Must comfortably exceed a
/// line of text so a line straddling a cut is fully seen by at least one strip.
const TILE_OVERLAP: i32 = 80;

/// Detector that reads the screen with Tesseract and hints text phrases.
pub struct OcrDetector {
    tesseract: Tesseract,
    min_confidence: f32,
    min_element_size: i32,
    grouping: Grouping,
    tiles: usize,
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
            tiles: config.ocr.tiles.max(1),
        }
    }

    /// One pass over the whole screen (no tiling).
    fn detect_whole(&self, screen: &Screen, region: Rect) -> Result<Vec<Word>> {
        let raw = self.tesseract.read(screen)?;
        Ok(tsv::parse(&raw, region, self.min_confidence))
    }

    /// Split the screen into overlapping strips, OCR them in parallel, and merge
    /// the (deduplicated) words.
    fn detect_tiled(&self, screen: &Screen, region: Rect) -> Result<Vec<Word>> {
        let width = screen.width();
        let height = screen.height();
        let bands = plan_bands(height, self.tiles, TILE_OVERLAP);

        // Build the whole-screen RGB once and share it across the worker threads;
        // each only reads its own strip out of it.
        let rgb = Arc::new(screen.to_rgb());
        // Cap each instance's threads so N parallel processes don't oversubscribe.
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = (cores / bands.len().max(1)).max(1);

        let mut handles = Vec::with_capacity(bands.len());
        for (y0, h) in bands {
            let tess = self.tesseract.clone();
            let rgb = Arc::clone(&rgb);
            let min_conf = self.min_confidence;
            let band_region = Rect::new(region.x, region.y + y0, width, h);
            handles.push(std::thread::spawn(move || -> Result<Vec<Word>> {
                let ppm = tesseract::encode_ppm_band(&rgb, width, y0, h);
                let raw = tess.run(ppm, threads)?;
                Ok(tsv::parse(&raw, band_region, min_conf))
            }));
        }

        let mut words = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(result) => words.extend(result?),
                Err(_) => return Err(crate::error::Error::Ocr("OCR worker panicked".into())),
            }
        }
        Ok(dedup_words(words))
    }
}

impl Detector for OcrDetector {
    fn name(&self) -> &'static str {
        "ocr"
    }

    fn detect(&self, screen: &Screen) -> Result<Vec<Element>> {
        let region = screen.bounds();
        let words = if self.tiles <= 1 {
            self.detect_whole(screen, region)?
        } else {
            self.detect_tiled(screen, region)?
        };
        let phrases = phrase::group(words, self.grouping);
        Ok(detect::finalize(phrases, self.min_element_size, region))
    }
}

/// Plan `tiles` horizontal strips covering `[0, height)`, each grown by
/// `overlap` pixels into its neighbours so text on a boundary is fully captured
/// by at least one strip. Returns `(y0, strip_height)` pairs; empty strips
/// (when `tiles` exceeds the useful row count) are dropped.
fn plan_bands(height: i32, tiles: usize, overlap: i32) -> Vec<(i32, i32)> {
    if height <= 0 {
        return Vec::new();
    }
    let tiles = tiles.max(1) as i32;
    let base = (height + tiles - 1) / tiles; // ceil division
    let mut bands = Vec::new();
    for i in 0..tiles {
        let core_start = i * base;
        if core_start >= height {
            break;
        }
        let core_end = ((i + 1) * base).min(height);
        let y0 = (core_start - overlap).max(0);
        let y1 = (core_end + overlap).min(height);
        bands.push((y0, y1 - y0));
    }
    bands
}

/// Drop near-duplicate words produced by overlapping strips. Two words are the
/// same detection when their text matches and their boxes overlap heavily; the
/// larger (more completely seen) box is kept.
fn dedup_words(mut words: Vec<Word>) -> Vec<Word> {
    // Keep the bigger box first so the survivor of a pair is the fuller one.
    words.sort_by_key(|w| std::cmp::Reverse(w.rect.area()));
    let mut kept: Vec<Word> = Vec::with_capacity(words.len());
    for w in words {
        let dup = kept
            .iter()
            .any(|k| k.text == w.text && overlap_fraction(k.rect, w.rect) > 0.5);
        if !dup {
            kept.push(w);
        }
    }
    kept
}

/// Intersection area over the smaller of the two areas (`0.0`..=`1.0`). `1.0`
/// means one box sits entirely inside the other.
fn overlap_fraction(a: Rect, b: Rect) -> f64 {
    let inter = a.clamp_to(b).map_or(0, |r| r.area());
    let smaller = a.area().min(b.area());
    if smaller == 0 {
        0.0
    } else {
        inter as f64 / smaller as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cover_the_whole_height_with_overlap() {
        let bands = plan_bands(1000, 4, 80);
        assert_eq!(bands.len(), 4);
        // First strip starts at the top, last reaches the bottom.
        assert_eq!(bands[0].0, 0);
        let (last_y, last_h) = *bands.last().unwrap();
        assert_eq!(last_y + last_h, 1000);
        // Every pixel row is covered by at least one strip.
        for y in 0..1000 {
            assert!(
                bands.iter().any(|&(y0, h)| y >= y0 && y < y0 + h),
                "row {y} is in no strip"
            );
        }
        // Adjacent strips actually overlap.
        assert!(
            bands[0].1 > 1000 / 4,
            "first strip should be grown by overlap"
        );
    }

    #[test]
    fn bands_drop_empties_when_tiles_exceed_height() {
        let bands = plan_bands(3, 8, 80);
        assert!(!bands.is_empty());
        assert!(bands.len() <= 3, "no empty strips past the last useful row");
        let (last_y, last_h) = *bands.last().unwrap();
        assert_eq!(last_y + last_h, 3);
    }

    #[test]
    fn dedup_removes_overlapping_same_text() {
        let words = vec![
            Word::new(Rect::new(10, 10, 40, 12), "File"),
            // Same word seen again in the next strip, shifted a couple px.
            Word::new(Rect::new(11, 11, 40, 12), "File"),
            // Different word at the same spot stays.
            Word::new(Rect::new(10, 10, 40, 12), "Edit"),
            // Same text far away stays.
            Word::new(Rect::new(500, 400, 40, 12), "File"),
        ];
        let out = dedup_words(words);
        assert_eq!(out.len(), 3);
        assert_eq!(out.iter().filter(|w| w.text == "File").count(), 2);
    }

    #[test]
    fn overlap_fraction_detects_containment() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(10, 10, 20, 20); // fully inside a
        assert_eq!(overlap_fraction(a, b), 1.0);
        let c = Rect::new(200, 200, 10, 10); // disjoint
        assert_eq!(overlap_fraction(a, c), 0.0);
    }
}
