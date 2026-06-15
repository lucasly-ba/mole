//! OCR detection: read the screen, group the words into phrase targets.
//!
//! This is mole's only detector. The work splits cleanly across helpers:
//!
//! * [`tesseract`] — run the `tesseract` subprocess over a screen capture.
//! * [`tsv`] — parse its TSV output into confident word boxes.
//! * [`phrase`] — group those words into the phrase-level targets we hint.
//! * [`cache`] — remember words so unchanged regions of the screen aren't re-read.
//!
//! [`OcrDetector`] wires them together and applies the shared [`detect::finalize`]
//! pass (size filtering + reading order) to the result.
//!
//! Tesseract is single-threaded per image and scanning a whole desktop is the
//! dominant cost of a hint, so OCR runs on horizontal bands in parallel (one
//! `tesseract` process each). On a cold scan the bands tile the whole screen; on
//! a warm scan the [`cache`] returns only tight bands around what actually
//! changed, so a hint on a mostly-static screen re-reads little or nothing. Bands
//! overlap (and large changes are split for parallelism), so a line on a cut is
//! fully seen by one band; the duplicate words that creates are removed by
//! [`dedup_words`].

mod cache;
mod cancel;
mod phrase;
mod tesseract;
mod tsv;

use std::sync::{Arc, Mutex};

pub use cache::ScanCache;
pub use cancel::Cancel;

use cache::Band;

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{self, Detector, Element, Scan, ScanRest};
use crate::error::{Error, Result};
use crate::geometry::Rect;

use phrase::{Grouping, Word};
use tesseract::Tesseract;

/// Vertical overlap (px) between adjacent strips. Must comfortably exceed a line
/// of text so a line straddling a cut is fully seen by at least one strip.
const TILE_OVERLAP: i32 = 80;

/// How recognised words are turned into hintable targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// One target per word — every word on screen is reachable.
    Word,
    /// One target per phrase (a run of words on a line).
    Phrase,
}

/// Detector that reads the screen with Tesseract and hints text.
pub struct OcrDetector {
    tesseract: Tesseract,
    min_confidence: f32,
    min_element_size: i32,
    grouping: Grouping,
    /// Whether on-demand hints are per word or per phrase (drag always uses
    /// phrases via [`Detector::detect_phrases`]).
    granularity: Granularity,
    /// Also hint icon-only clickable regions (no text) found from the pixels.
    hint_icons: bool,
    tiles: usize,
    cache: Arc<Mutex<ScanCache>>,
    /// Lets another thread kill this detector's in-flight OCR. The on-demand path
    /// uses a fresh, never-aborted token; the pre-warm shares the daemon's so a
    /// hint can preempt it.
    cancel: Cancel,
}

impl OcrDetector {
    /// Build a detector sharing `cache` (the daemon owns one cache so it survives
    /// across hints and can be warmed in the background), with its own
    /// never-aborted cancellation token (the on-demand path).
    pub fn new(config: &Config, cache: Arc<Mutex<ScanCache>>) -> Self {
        Self::with_cancel(config, cache, Cancel::new())
    }

    /// Like [`OcrDetector::new`] but driven by a shared `cancel` token, so a
    /// caller (the daemon, on a hint) can abort this detector's OCR mid-read. Used
    /// by the background pre-warm.
    pub fn with_cancel(config: &Config, cache: Arc<Mutex<ScanCache>>, cancel: Cancel) -> Self {
        OcrDetector {
            tesseract: Tesseract::new(config.ocr.binary.clone(), config.ocr.language.clone()),
            min_confidence: config.ocr.min_confidence,
            min_element_size: config.ocr.min_element_size,
            grouping: Grouping {
                line_tolerance: config.ocr.line_tolerance,
                max_word_gap: config.ocr.max_word_gap,
            },
            granularity: if config.ocr.hint_words {
                Granularity::Word
            } else {
                Granularity::Phrase
            },
            hint_icons: config.ocr.hint_icons,
            tiles: config.ocr.tiles.max(1),
            cache,
            cancel,
        }
    }

    /// Capture-to-words in one shot, re-reading only the changed bands. The cache
    /// lock is held across the whole scan, so the on-demand hint and the
    /// background pre-warm never OCR at the same time (which would oversubscribe
    /// the cores). Used by [`Detector::detect`] and the pre-warmer.
    fn scan(&self, screen: &Screen, region: Rect) -> Result<Vec<Word>> {
        let width = screen.width();
        let height = screen.height();
        let rgb = screen.to_rgb();

        let mut cache = self.cache.lock().unwrap();
        let bands = cache.plan(&rgb, width, height, self.tiles, TILE_OVERLAP);
        log::debug!("cache: re-reading {} band(s)", bands.len());
        let fresh = ocr_bands(
            &self.tesseract,
            &rgb,
            width,
            region,
            &bands,
            self.min_confidence,
            &self.cancel,
        )?;
        for (band, words) in fresh {
            cache.splice(&rgb, region.y, band, words);
        }
        Ok(dedup_words(cache.all_words()))
    }

    fn elements(&self, words: Vec<Word>, region: Rect) -> Vec<Element> {
        words_to_elements(
            words,
            self.granularity,
            self.grouping,
            self.min_element_size,
            region,
        )
    }

    /// Append icon hints (clickable non-text regions) to already-finalized text
    /// `els`, found from `rgb`, then re-finalize so everything sorts into one
    /// reading order. A no-op when `hint_icons` is off.
    fn with_icons(
        &self,
        mut els: Vec<Element>,
        rgb: &[u8],
        width: i32,
        region: Rect,
    ) -> Vec<Element> {
        if !self.hint_icons {
            return els;
        }
        let height = if width > 0 {
            (rgb.len() / 3) as i32 / width
        } else {
            0
        };
        let icons = detect::icon::detect(rgb, width, height, region, &els, self.min_element_size);
        log::debug!("icons: {} icon hint(s)", icons.len());
        if icons.is_empty() {
            return els;
        }
        els.extend(icons);
        detect::finalize(els, self.min_element_size, region)
    }
}

/// Turn recognised words into finalized targets at the requested granularity:
/// each word as its own target, or words grouped into phrases. Free-standing so
/// the background `detect_split` worker can call it without borrowing the
/// detector across threads.
fn words_to_elements(
    words: Vec<Word>,
    granularity: Granularity,
    grouping: Grouping,
    min_size: i32,
    region: Rect,
) -> Vec<Element> {
    let raw = match granularity {
        Granularity::Word => words
            .into_iter()
            .map(|w| Element::new(w.rect, w.text))
            .collect(),
        Granularity::Phrase => phrase::group(words, grouping),
    };
    detect::finalize(raw, min_size, region)
}

impl Detector for OcrDetector {
    fn name(&self) -> &'static str {
        "ocr"
    }

    fn detect(&self, screen: &Screen) -> Result<Vec<Element>> {
        let region = screen.bounds();
        let words = self.scan(screen, region)?;
        Ok(self.elements(words, region))
    }

    /// Always group into phrases, whatever the configured hint granularity, so
    /// drag selection can grab a whole sentence.
    fn detect_phrases(&self, screen: &Screen) -> Result<Vec<Element>> {
        let region = screen.bounds();
        let words = self.scan(screen, region)?;
        Ok(words_to_elements(
            words,
            Granularity::Phrase,
            self.grouping,
            self.min_element_size,
            region,
        ))
    }

    /// Show the cached words for the unchanged part of the screen immediately,
    /// and re-OCR only the changed bands in the background (the pending part),
    /// yielding the words found there once done.
    fn detect_split(&self, screen: &Screen) -> Result<Scan> {
        let region = screen.bounds();
        let width = screen.width();
        let height = screen.height();
        let rgb = std::sync::Arc::new(screen.to_rgb());

        // Plan the changed bands and snapshot the current words under one lock.
        let (bands, cached) = {
            let mut cache = self.cache.lock().unwrap();
            let bands = cache.plan(&rgb, width, height, self.tiles, TILE_OVERLAP);
            (bands, cache.all_words())
        };

        if bands.is_empty() {
            // Nothing changed — every cached word is current.
            let ready = self.elements(dedup_words(cached), region);
            return Ok(Scan {
                ready: self.with_icons(ready, &rgb, width, region),
                pending: None,
            });
        }

        // Only when *almost the whole screen* changed (a workspace switch, a fresh
        // fullscreen window) is the cache too stale to be worth showing — the
        // cached hints would point at text that's gone. There we re-read
        // everything up front and show the complete, correct set at once. For any
        // lesser change we keep the cached hints on screen instantly and fold the
        // re-OCR'd regions in (the two-wave path below), so a hint feels instant
        // even when a chunk of the screen moved. (Threshold: > ~85% of height.)
        let coverage = band_coverage(&bands);
        // `height.max(1)` guards the divide without an if-expression (which two
        // rustfmt versions format differently); coverage is 0 when height is.
        let pct = coverage * 100 / height.max(1);
        log::debug!(
            "detect_split: {} band(s), {pct}% of height changed",
            bands.len()
        );
        if coverage * 100 > height * 85 {
            let all = {
                let fresh = ocr_bands(
                    &self.tesseract,
                    &rgb,
                    width,
                    region,
                    &bands,
                    self.min_confidence,
                    &self.cancel,
                )?;
                let mut cache = self.cache.lock().unwrap();
                for (band, words) in fresh {
                    cache.splice(&rgb, region.y, band, words);
                }
                cache.all_words()
            };
            let ready = self.elements(dedup_words(all), region);
            return Ok(Scan {
                ready: self.with_icons(ready, &rgb, width, region),
                pending: None,
            });
        }

        let ranges: Vec<(i32, i32)> = bands
            .iter()
            .map(|b| (region.y + b.y0, region.y + b.y0 + b.h))
            .collect();

        // Phase 1: show *every* cached word immediately — including the ones
        // inside the changed bands, at their last-known positions. A hovered link
        // or a button that just repainted hasn't moved its text, so its hint is
        // correct instantly; this is what makes a hint feel instant even right
        // after the mouse swept over (and repainted) the area.
        let ready = self.elements(dedup_words(cached.clone()), region);
        let ready = self.with_icons(ready, &rgb, width, region);

        // Keep the stale words inside the changed bands so the background pass can
        // tell a genuinely-new word from one already on screen.
        let stale_inside: Vec<Word> = cached
            .into_iter()
            .filter(|w| in_ranges(w.rect.center().y, &ranges))
            .collect();

        // Phase 2: OCR the changed bands off-thread, splice them into the cache,
        // and fold in only the words that weren't already shown (new/changed text),
        // so an unchanged-but-repainted region adds nothing and never flickers.
        let cache = std::sync::Arc::clone(&self.cache);
        let tess = self.tesseract.clone();
        let cancel = self.cancel.clone();
        let min_conf = self.min_confidence;
        let min_size = self.min_element_size;
        let grouping = self.grouping;
        let granularity = self.granularity;
        let pending: ScanRest = Box::new(move || {
            let fresh = ocr_bands(&tess, &rgb, width, region, &bands, min_conf, &cancel)?;
            let inside: Vec<Word> = {
                let mut cache = cache.lock().unwrap();
                for (band, words) in fresh {
                    cache.splice(&rgb, region.y, band, words);
                }
                cache
                    .all_words()
                    .into_iter()
                    .filter(|w| in_ranges(w.rect.center().y, &ranges))
                    .collect()
            };
            let added: Vec<Word> = dedup_words(inside)
                .into_iter()
                .filter(|w| !already_shown(w, &stale_inside))
                .collect();
            Ok(words_to_elements(
                added,
                granularity,
                grouping,
                min_size,
                region,
            ))
        });
        Ok(Scan {
            ready,
            pending: Some(pending),
        })
    }
}

/// True when `y` falls in any of the (start, end) ranges.
fn in_ranges(y: i32, ranges: &[(i32, i32)]) -> bool {
    ranges.iter().any(|&(a, b)| y >= a && y < b)
}

/// Maximum drift (px) between two boxes still considered the same word, absorbing
/// OCR jitter while staying well under the gap to a neighbouring word.
const SAME_WORD_TOL: i32 = 6;

/// Whether `word` is already represented among the `shown` (stale, still-on-screen)
/// words — same text and near-identical position. Used so the background re-OCR of
/// a repainted region only adds words that genuinely appeared or changed, instead
/// of duplicating ones already on screen.
fn already_shown(word: &Word, shown: &[Word]) -> bool {
    shown.iter().any(|s| {
        s.text == word.text
            && (s.rect.x - word.rect.x).abs() <= SAME_WORD_TOL
            && (s.rect.y - word.rect.y).abs() <= SAME_WORD_TOL
    })
}

/// Total screen height (px) covered by the bands, counting overlap once.
fn band_coverage(bands: &[Band]) -> i32 {
    let mut spans: Vec<(i32, i32)> = bands.iter().map(|b| (b.y0, b.y0 + b.h)).collect();
    spans.sort_unstable();
    let mut covered = 0;
    let mut reached = i32::MIN;
    for (start, end) in spans {
        let start = start.max(reached);
        if end > start {
            covered += end - start;
        }
        reached = reached.max(end);
    }
    covered
}

/// OCR the given bands in parallel, capping each process's threads so they don't
/// oversubscribe the cores. Returns `(band, words)`.
fn ocr_bands(
    tess: &Tesseract,
    rgb: &[u8],
    width: i32,
    region: Rect,
    bands: &[Band],
    min_conf: f32,
    cancel: &Cancel,
) -> Result<Vec<(Band, Vec<Word>)>> {
    if bands.is_empty() {
        return Ok(Vec::new());
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = (cores / bands.len()).max(1);

    std::thread::scope(|scope| {
        let handles: Vec<_> = bands
            .iter()
            .map(|&b| {
                scope.spawn(move || -> Result<(Band, Vec<Word>)> {
                    let ppm = tesseract::encode_ppm_band(rgb, width, b.y0, b.h);
                    let raw = tess.run(ppm, threads, cancel)?;
                    let area = Rect::new(region.x, region.y + b.y0, width, b.h);
                    Ok((b, tsv::parse(&raw, area, min_conf)))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .map_err(|_| Error::Ocr("OCR worker panicked".into()))?
            })
            .collect()
    })
}

/// Plan `tiles` horizontal strips covering `[0, height)`, each grown by `overlap`
/// pixels into its neighbours so text on a boundary is fully captured by at least
/// one strip. Returns `(y0, strip_height)` pairs; empty strips (when `tiles`
/// exceeds the useful row count) are dropped.
pub(super) fn plan_bands(height: i32, tiles: usize, overlap: i32) -> Vec<(i32, i32)> {
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
        assert_eq!(bands[0].0, 0);
        let (last_y, last_h) = *bands.last().unwrap();
        assert_eq!(last_y + last_h, 1000);
        for y in 0..1000 {
            assert!(
                bands.iter().any(|&(y0, h)| y >= y0 && y < y0 + h),
                "row {y} is in no strip"
            );
        }
        assert!(
            bands[0].1 > 1000 / 4,
            "first strip should be grown by overlap"
        );
    }

    #[test]
    fn band_coverage_merges_overlaps() {
        // Disjoint bands sum.
        assert_eq!(
            band_coverage(&[Band { y0: 0, h: 100 }, Band { y0: 300, h: 50 }]),
            150
        );
        // Overlapping bands count the union once.
        assert_eq!(
            band_coverage(&[Band { y0: 0, h: 100 }, Band { y0: 80, h: 100 }]),
            180
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
            Word::new(Rect::new(11, 11, 40, 12), "File"),
            Word::new(Rect::new(10, 10, 40, 12), "Edit"),
            Word::new(Rect::new(500, 400, 40, 12), "File"),
        ];
        let out = dedup_words(words);
        assert_eq!(out.len(), 3);
        assert_eq!(out.iter().filter(|w| w.text == "File").count(), 2);
    }

    #[test]
    fn already_shown_matches_same_word_not_a_neighbour() {
        let shown = vec![Word::new(Rect::new(100, 50, 40, 14), "File")];
        // Same word, a couple px of OCR jitter -> already shown.
        assert!(already_shown(
            &Word::new(Rect::new(102, 51, 40, 14), "File"),
            &shown
        ));
        // Same text but a different place on screen -> a distinct occurrence.
        assert!(!already_shown(
            &Word::new(Rect::new(400, 50, 40, 14), "File"),
            &shown
        ));
        // Different text at the same spot (the region's text changed) -> new.
        assert!(!already_shown(
            &Word::new(Rect::new(100, 50, 40, 14), "Edit"),
            &shown
        ));
    }

    #[test]
    fn word_granularity_keeps_each_word_phrase_merges_them() {
        let grouping = Grouping {
            line_tolerance: 0.5,
            max_word_gap: 1.0,
        };
        let region = Rect::new(0, 0, 1000, 800);
        let words = vec![
            Word::new(Rect::new(10, 10, 40, 14), "File"),
            Word::new(Rect::new(56, 10, 40, 14), "Edit"),
        ];
        // Word: two separate targets.
        let per_word = words_to_elements(words.clone(), Granularity::Word, grouping, 6, region);
        assert_eq!(per_word.len(), 2);
        // Phrase: the two adjacent words merge into one.
        let per_phrase = words_to_elements(words, Granularity::Phrase, grouping, 6, region);
        assert_eq!(per_phrase.len(), 1);
        assert_eq!(per_phrase[0].text, "File Edit");
    }

    #[test]
    fn overlap_fraction_detects_containment() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(10, 10, 20, 20);
        assert_eq!(overlap_fraction(a, b), 1.0);
        let c = Rect::new(200, 200, 10, 10);
        assert_eq!(overlap_fraction(a, c), 0.0);
    }
}
